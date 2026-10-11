//! Installable shell. The service worker caches the icon, the manifest, and
//! the offline page only. Authenticated HTML stays on the network so a
//! shared phone does not keep someone else's Today.
//!
//! Field posts still work with JavaScript off. When the browser is offline
//! and a script is running, Today's forms wait on this phone and send on
//! the next visit that has a connection.

use axum::Router;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use maud::html;

use crate::AppState;
use crate::ui::layout;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/manifest.webmanifest", get(manifest))
        .route("/sw.js", get(worker))
        .route("/icon.svg", get(icon))
        .route("/offline", get(offline))
}

async fn manifest() -> impl IntoResponse {
    const BODY: &str = r##"{
  "name": "Sun Brewed Homestead",
  "short_name": "Homestead",
  "start_url": "/",
  "scope": "/",
  "display": "standalone",
  "background_color": "#fff8f5",
  "theme_color": "#faebe0",
  "icons": [{ "src": "/icon.svg", "sizes": "any", "type": "image/svg+xml", "purpose": "any" }]
}
"##;
    ([(header::CONTENT_TYPE, "application/manifest+json; charset=utf-8")], BODY)
}

async fn worker() -> impl IntoResponse {
    const BODY: &str = r#"// Authenticated pages stay on the network.
const SHELL = ["/manifest.webmanifest", "/icon.svg", "/offline"];
// Bump when a shell file changes; activation drops the older caches.
const CACHE = "sbh-shell-2";
self.addEventListener("install", (event) => {
  event.waitUntil(caches.open(CACHE).then((cache) => cache.addAll(SHELL)).then(() => self.skipWaiting()));
});
self.addEventListener("activate", (event) => {
  event.waitUntil(caches.keys()
    .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
    .then(() => self.clients.claim()));
});
self.addEventListener("fetch", (event) => {
  const path = new URL(event.request.url).pathname;
  if (event.request.method !== "GET" || !SHELL.includes(path)) return;
  event.respondWith(caches.match(event.request).then((hit) => hit || fetch(event.request)));
});
"#;
    ([(header::CONTENT_TYPE, "application/javascript; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], BODY)
}

/// The brand mark, also shown in every page's app bar.
async fn icon() -> impl IntoResponse {
    const BODY: &str = include_str!("../../../brand/mark.svg");
    ([(header::CONTENT_TYPE, "image/svg+xml; charset=utf-8")], BODY)
}

async fn offline() -> impl IntoResponse {
    layout(
        "Offline",
        html! {
            p { "This phone has no connection. Water, harvest, egg, and checklist notes saved here will send the next time Today opens with a connection." }
            p { a href="/" { "Try Today" } }
        },
    )
}

/// Registered on every page. Does nothing where service workers are absent.
pub const REGISTER: &str = r#"<script>
if ("serviceWorker" in navigator) navigator.serviceWorker.register("/sw.js");
</script>"#;

/// Queues Today's form posts while offline, then sends them on the next load.
pub const FIELD_QUEUE: &str = r#"<script>
(function () {
  var key = "sbh-field-queue";
  var note = document.getElementById("field-queue");
  function read() { try { return JSON.parse(localStorage.getItem(key) || "[]"); } catch (e) { return []; } }
  function write(rows) { localStorage.setItem(key, JSON.stringify(rows)); show(rows.length); }
  function show(n) {
    if (!note) return;
    note.textContent = n ? n + " field note" + (n === 1 ? "" : "s") + " waiting to send." : "";
  }
  document.addEventListener("submit", function (event) {
    var form = event.target;
    if (!form || !form.action) return;
    var path = new URL(form.action, location.href).pathname;
    if (path.indexOf("/today/") !== 0 || navigator.onLine) return;
    event.preventDefault();
    var rows = read();
    rows.push({ path: path, body: new URLSearchParams(new FormData(form)).toString() });
    write(rows);
  }, true);
  async function flush() {
    if (!navigator.onLine) return;
    var rows = read();
    var left = [];
    for (var i = 0; i < rows.length; i++) {
      try {
        var response = await fetch(rows[i].path, {
          method: "POST",
          credentials: "same-origin",
          headers: { "content-type": "application/x-www-form-urlencoded" },
          body: rows[i].body
        });
        var landed = new URL(response.url).pathname;
        if (response.status === 401 || landed === "/login" || response.status >= 500) left.push(rows[i]);
      } catch (e) { left.push(rows[i]); }
    }
    var sent = rows.length - left.length;
    write(left);
    if (sent > 0 && left.length === 0) location.reload();
  }
  show(read().length);
  window.addEventListener("online", flush);
  flush();
})();
</script>"#;
