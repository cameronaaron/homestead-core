//! Documenso: legal releases sent for e-signature, and its completion webhook.
//!
//! Releases are sent straight from the PDF and fields in the repo
//! (`documenso/releases.toml`) as one-off documents via the v2 API, so no
//! Documenso-side templates exist to drift. Each document carries
//! `externalId = "sbh-release:<uuid>"`, which Documenso echoes back in its
//! webhooks; that uuid is the `release_requests` row.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use uuid::Uuid;

pub const EXTERNAL_ID_PREFIX: &str = "sbh-release:";

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    /// The steward has approved these documents for use. Sending is refused
    /// and every PDF carries a DRAFT watermark until this is set.
    pub approved: bool,
    /// Whether a California attorney has reviewed them. Informational: it
    /// gates nothing, but `documenso-verify` reports it so the gap is not
    /// forgotten.
    pub reviewed_by_counsel: bool,
    /// Envelope `meta` for every release (language, timezone, dateFormat,
    /// redirectUrl, reminderSettings…); a release's own `meta` wins.
    #[serde(default)]
    pub meta: Meta,
    pub releases: Vec<Release>,
    /// Sets of releases the send form can preselect; the first is the
    /// default.
    #[serde(default)]
    pub bundles: Vec<Bundle>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    pub key: String,
    pub title: String,
    pub releases: Vec<String>,
}

/// A box on the document above the signature block.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detail {
    pub key: String,
    pub label: String,
    /// Who fills it in: the sender, in the app's send form (then it's
    /// locked on the document), or the signer, while signing.
    pub by: FilledBy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilledBy {
    Sender,
    Signer,
}

/// Raw fields of Documenso's envelope `meta`.
pub type Meta = std::collections::BTreeMap<String, Value>;

/// `meta` fields the typed spec owns.
const META_OWNED: &[&str] = &["subject", "message"];

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    pub key: String,
    pub title: String,
    pub pdf: String,
    pub source: String,
    pub subject: String,
    pub message: String,
    #[serde(default)]
    pub meta: Meta,
    #[serde(default)]
    pub details: Vec<Detail>,
}

impl Release {
    pub fn sender_details(&self) -> impl Iterator<Item = &Detail> {
        self.details.iter().filter(|d| d.by == FilledBy::Sender)
    }

    /// The signing fields its layout drew, written by `legal/build.sh`
    /// beside the PDF.
    pub fn layout_path(&self) -> String {
        format!("{}.fields.json", self.pdf.strip_suffix(".pdf").unwrap_or(&self.pdf))
    }
}

/// A box drawn on the document (legal/release.typ) and the Documenso field
/// that goes on it. Positions are percentages of the page.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub kind: FieldKind,
    pub page: u32,
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The Documenso field types the layout draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum FieldKind {
    Signature,
    Date,
    Name,
    Email,
    /// Initials beside a liability release (`release-box` in
    /// legal/release.typ), so the signer acknowledges it separately.
    Initials,
    /// A detail box: the release's `details`, in order.
    Text,
}

/// Every release's layout, read from `root` (the repository root) and
/// checked.
pub fn load_layouts(spec: &Spec, root: &std::path::Path) -> Result<BTreeMap<String, Vec<Field>>, String> {
    spec.releases
        .iter()
        .map(|r| {
            let path = root.join(r.layout_path());
            let json = std::fs::read_to_string(&path)
                .map_err(|e| format!("reading {}: {e} (run ./legal/build.sh)", path.display()))?;
            Ok((r.key.clone(), parse_layout(r, &json)?))
        })
        .collect()
}

/// Reads and checks a release's layout (`<pdf>.fields.json`): one
/// signature, on the last page; the detail boxes the release lists, in
/// order; every box on the page.
pub fn parse_layout(r: &Release, json: &str) -> Result<Vec<Field>, String> {
    let fields: Vec<Field> = serde_json::from_str(json).map_err(|e| format!("{}: {e}", r.layout_path()))?;
    let mut e = Vec::new();
    let last = fields.iter().map(|f| f.page).max().unwrap_or(0);
    let signatures: Vec<_> = fields.iter().filter(|f| f.kind == FieldKind::Signature).collect();
    if signatures.len() != 1 || signatures[0].page != last {
        e.push("it needs exactly one signature, on its last page".to_owned());
    }
    let drawn: Vec<&str> = fields.iter().filter(|f| f.kind == FieldKind::Text).map(|f| f.label.as_str()).collect();
    let wanted: Vec<&str> = r.details.iter().map(|d| d.label.as_str()).collect();
    if drawn != wanted {
        e.push(format!("it draws detail boxes {drawn:?} but releases.toml lists {wanted:?}"));
    }
    for f in &fields {
        let inside = |a: f64, len: f64| a >= 0.0 && len > 0.0 && a + len <= 100.0;
        if f.page == 0 || !inside(f.x, f.width) || !inside(f.y, f.height) {
            e.push(format!("its {:?} box isn't on the page", f.label));
        }
    }
    if e.is_empty() { Ok(fields) } else { Err(format!("release {:?}: {}", r.key, e.join("; "))) }
}

impl Spec {
    /// Whether Documenso emails signers their link. With
    /// `distributionMethod = "NONE"` the sender passes it on instead.
    pub fn emails_signers(&self) -> bool {
        self.meta.get("distributionMethod").and_then(Value::as_str) != Some("NONE")
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let spec: Spec = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut e = Vec::new();
        let mut keys = BTreeSet::new();
        for r in &spec.releases {
            if !r.key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') || r.key.is_empty() {
                e.push(format!("release key {:?} must be lowercase-with-dashes", r.key));
            }
            if !keys.insert(r.key.as_str()) {
                e.push(format!("release {:?} is listed twice", r.key));
            }
        }
        let metas = std::iter::once(("[meta]".to_owned(), &spec.meta))
            .chain(spec.releases.iter().map(|r| (format!("release {:?} meta", r.key), &r.meta)));
        for (whose, meta) in metas {
            for k in meta.keys() {
                if META_OWNED.contains(&k.as_str()) {
                    e.push(format!("{whose}: {k:?} is set by the release's own field, not meta"));
                }
                let mut chars = k.chars();
                if !chars.next().is_some_and(|c| c.is_ascii_lowercase()) || !chars.all(|c| c.is_ascii_alphanumeric()) {
                    e.push(format!("{whose}: {k:?} is not a Documenso API field name (camelCase)"));
                }
            }
        }
        for r in &spec.releases {
            let mut seen = BTreeSet::new();
            for d in &r.details {
                if d.key.is_empty() || !d.key.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                    e.push(format!("release {:?}: detail key {:?} must be lowercase_with_underscores", r.key, d.key));
                }
                if !seen.insert(d.key.as_str()) {
                    e.push(format!("release {:?}: detail {:?} is listed twice", r.key, d.key));
                }
                if d.label.trim().is_empty() {
                    e.push(format!("release {:?}: detail {:?} needs a label", r.key, d.key));
                }
            }
        }
        let mut bundles = BTreeSet::new();
        for b in &spec.bundles {
            if !bundles.insert(b.key.as_str()) {
                e.push(format!("bundle {:?} is listed twice", b.key));
            }
            for k in &b.releases {
                if !keys.contains(k.as_str()) {
                    e.push(format!("bundle {:?} names unknown release {k:?}", b.key));
                }
            }
        }
        if e.is_empty() { Ok(spec) } else { Err(e.join("\n")) }
    }

    pub fn release(&self, key: &str) -> Option<&Release> {
        self.releases.iter().find(|r| r.key == key)
    }
}

/// `meta` keys the instance doesn't list for `/envelope/create`, which it
/// would silently drop. `openapi` is its `/api/v2/openapi.json`.
pub fn unknown_meta(spec: &Spec, openapi: &Value) -> Result<Vec<String>, String> {
    fn find(v: &Value) -> Option<&serde_json::Map<String, Value>> {
        match v {
            Value::Object(o) => {
                o.get("meta").and_then(|m| m["properties"].as_object()).or_else(|| o.values().find_map(find))
            }
            Value::Array(a) => a.iter().find_map(find),
            _ => None,
        }
    }
    let known = find(&openapi["paths"]["/envelope/create"]).ok_or("no envelope meta schema in openapi.json")?;
    let keys = spec.meta.keys().chain(spec.releases.iter().flat_map(|r| r.meta.keys()));
    let mut unknown: Vec<String> = keys.filter(|k| !known.contains_key(*k)).cloned().collect();
    unknown.sort();
    unknown.dedup();
    Ok(unknown)
}

pub fn external_id(id: Uuid) -> String {
    format!("{EXTERNAL_ID_PREFIX}{id}")
}

/// A settled signing, extracted from a webhook we care about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    Completed(Uuid),
    Rejected(Uuid),
}

/// Reads a Documenso webhook body. `Ok(None)` for events or documents that
/// aren't ours to track (other event types, documents sent by hand).
pub fn parse_webhook(body: &[u8]) -> Result<Option<Settled>, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("malformed webhook: {e}"))?;
    let Some(id) = v["payload"]["externalId"]
        .as_str()
        .and_then(|x| x.strip_prefix(EXTERNAL_ID_PREFIX))
        .and_then(|x| Uuid::parse_str(x).ok())
    else {
        return Ok(None);
    };
    Ok(match v["event"].as_str() {
        Some("DOCUMENT_COMPLETED") => Some(Settled::Completed(id)),
        Some("DOCUMENT_REJECTED") => Some(Settled::Rejected(id)),
        _ => None,
    })
}

/// Constant-time check of the `X-Documenso-Secret` header.
pub fn secret_matches(presented: &str, expected: &str) -> bool {
    !expected.is_empty() && bool::from(presented.as_bytes().ct_eq(expected.as_bytes()))
}

/// Waits before each retry of a distribute. A core rollout on 2026-09-30
/// left three created envelopes undistributed, each needing only the one
/// call that failed.
const DISTRIBUTE_RETRY_WAITS: [std::time::Duration; 2] =
    [std::time::Duration::from_secs(1), std::time::Duration::from_secs(4)];

/// How long [`Client::wake`] waits for a cold start.
const WAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Client {
    /// `base_url` is the instance root, e.g. `https://app.documenso.com`.
    pub fn new(http: reqwest::Client, base_url: &str, token: String) -> Self {
        Self { http, base: format!("{}/api/v2", base_url.trim_end_matches('/')), token }
    }

    async fn check(resp: reqwest::Response, what: &str) -> Result<Value, String> {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            let hint = if status.as_u16() == 401 { " (check DOCUMENSO_API_TOKEN)" } else { "" };
            return Err(format!(
                "Documenso {what} → HTTP {status}{hint}: {}",
                text.chars().take(300).collect::<String>()
            ));
        }
        serde_json::from_str(&text).map_err(|e| format!("Documenso {what}: {e}"))
    }

    /// Waits for a sleeping instance to start. Ours sleeps when idle and
    /// takes about half a minute to answer, past the shared client's
    /// timeout, so a create sent cold failed. Retrying the create instead
    /// could make two envelopes, since the first may land once it wakes.
    /// Failures are ignored: the create reports anything real.
    pub async fn wake(&self) {
        let root = self.base.trim_end_matches("/api/v2");
        let _ = self.http.get(format!("{root}/api/health")).timeout(WAKE_TIMEOUT).send().await;
    }

    /// The instance's API description, which lists the `meta` it accepts.
    pub async fn openapi(&self) -> Result<Value, String> {
        let resp = self
            .http
            .get(format!("{}/openapi.json", self.base))
            .send()
            .await
            .map_err(|e| format!("Documenso unreachable at {}: {e}", self.base))?;
        Self::check(resp, "openapi.json").await
    }

    /// Confirms the instance is reachable and the token is accepted.
    pub async fn ping(&self) -> Result<(), String> {
        let resp = self
            .http
            .get(format!("{}/envelope?perPage=1", self.base))
            .header("Authorization", &self.token)
            .send()
            .await
            .map_err(|e| format!("Documenso unreachable at {}: {e}", self.base))?;
        Self::check(resp, "list envelopes").await.map(drop)
    }

    /// Distributes a created envelope (by email, unless the spec's `meta`
    /// says `distributionMethod = "NONE"`), retrying a dropped connection
    /// or a 5xx. Distributing twice is harmless: Documenso answers with the
    /// same signing link (verified against production 2026-09-30), so a
    /// retry, or a later resume of a created-but-unsent envelope, can't
    /// invite anyone twice.
    pub async fn distribute(&self, envelope: String, email: &str) -> Result<Sent, String> {
        let mut waits = DISTRIBUTE_RETRY_WAITS.iter();
        loop {
            let attempt = match self
                .http
                .post(format!("{}/envelope/distribute", self.base))
                .header("Authorization", &self.token)
                .json(&json!({ "envelopeId": envelope }))
                .send()
                .await
            {
                Err(e) => Err((true, format!("Documenso distribute: {e}"))),
                Ok(resp) => {
                    let transient = resp.status().is_server_error() || resp.status().as_u16() == 429;
                    Self::check(resp, "distribute").await.map_err(|e| (transient, e))
                }
            };
            match (attempt, waits.next()) {
                (Ok(distributed), _) => {
                    return Ok(Sent { signing_url: signing_url(&distributed, email), envelope });
                }
                (Err((true, e)), Some(wait)) => {
                    tracing::warn!(error = %e, envelope, "retrying Documenso distribute");
                    tokio::time::sleep(*wait).await;
                }
                (Err((_, e)), _) => return Err(e),
            }
        }
    }

    /// Creates the signing document as a draft, sent to nobody. Returns its
    /// envelope id.
    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &self,
        spec: &Spec,
        release: &Release,
        pdf: Vec<u8>,
        layout: &[Field],
        signer: Signer<'_>,
        values: &BTreeMap<String, String>,
        id: Uuid,
    ) -> Result<String, String> {
        let payload = envelope_payload(spec, release, layout, signer, values, id);
        let file = reqwest::multipart::Part::bytes(pdf)
            .file_name(format!("{}.pdf", release.key))
            .mime_str("application/pdf")
            .map_err(|e| e.to_string())?;
        let form = reqwest::multipart::Form::new().text("payload", payload.to_string()).part("files", file);
        let resp = self
            .http
            .post(format!("{}/envelope/create", self.base))
            .header("Authorization", &self.token)
            .multipart(form)
            .send()
            .await
            .map_err(|e| format!("Documenso: {e}"))?;
        let created = Self::check(resp, "create").await?;
        match &created["id"] {
            Value::String(s) => Ok(s.clone()),
            Value::Number(n) => Ok(n.to_string()),
            other => Err(format!("Documenso create: unexpected id {other}")),
        }
    }

    /// An envelope as Documenso stores it, fields included.
    pub async fn envelope(&self, envelope: &str) -> Result<Value, String> {
        let resp = self
            .http
            .get(format!("{}/envelope/{envelope}", self.base))
            .header("Authorization", &self.token)
            .send()
            .await
            .map_err(|e| format!("Documenso: {e}"))?;
        Self::check(resp, "get envelope").await
    }

    pub async fn delete(&self, envelope: &str) -> Result<(), String> {
        let resp = self
            .http
            .post(format!("{}/envelope/delete", self.base))
            .header("Authorization", &self.token)
            .json(&json!({ "envelopeId": envelope }))
            .send()
            .await
            .map_err(|e| format!("Documenso: {e}"))?;
        Self::check(resp, "delete").await.map(drop)
    }
}

/// A release Documenso has accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub envelope: String,
    /// Where the signer signs; anyone holding it can sign as them.
    pub signing_url: Option<String>,
}

/// The signer's link in a distribute response.
fn signing_url(distributed: &Value, email: &str) -> Option<String> {
    distributed["recipients"]
        .as_array()?
        .iter()
        .find(|r| r["email"].as_str().is_some_and(|e| e.eq_ignore_ascii_case(email.trim())))?["signingUrl"]
        .as_str()
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
        .map(str::to_owned)
}

/// Who a release goes to.
#[derive(Debug, Clone, Copy)]
pub struct Signer<'a> {
    pub name: &'a str,
    pub email: &'a str,
}

/// The sender's details for `r`, trimmed, or which ones are missing.
pub fn sender_values(r: &Release, given: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>, Vec<String>> {
    let mut out = BTreeMap::new();
    let mut missing = Vec::new();
    for d in r.sender_details() {
        match given.get(&d.key).map(|v| v.trim()).filter(|v| !v.is_empty()) {
            Some(v) => {
                out.insert(d.key.clone(), v.to_owned());
            }
            None => missing.push(d.label.clone()),
        }
    }
    if missing.is_empty() { Ok(out) } else { Err(missing) }
}

/// "Cat Adoption Agreement · Jane Doe · Mochi": what the sender typed in
/// makes the document findable in Documenso and in the app.
pub fn document_title(r: &Release, signer_name: &str, values: &BTreeMap<String, String>) -> String {
    let mut parts = vec![r.title.clone(), signer_name.trim().to_owned()];
    parts.extend(r.sender_details().filter_map(|d| values.get(&d.key).cloned()));
    parts.join(" · ")
}

pub(crate) fn envelope_payload(
    spec: &Spec,
    r: &Release,
    layout: &[Field],
    signer: Signer<'_>,
    values: &BTreeMap<String, String>,
    id: Uuid,
) -> Value {
    // parse_layout checked the detail boxes are the release's details, in order.
    let mut details = r.details.iter();
    let fields: Vec<Value> = layout
        .iter()
        .map(|f| {
            let mut field = json!({
                "identifier": 0, "type": f.kind, "page": f.page,
                "positionX": f.x, "positionY": f.y, "width": f.width, "height": f.height,
            });
            let meta = match f.kind {
                FieldKind::Text => details.next().map(|d| {
                    let mut meta = json!({ "type": "text", "label": d.label, "required": true, "fontSize": 10 });
                    if d.by == FilledBy::Sender {
                        meta["text"] = json!(values.get(&d.key).cloned().unwrap_or_default());
                        meta["readOnly"] = json!(true);
                    }
                    meta
                }),
                _ => None,
            };
            if let Some(meta) = meta {
                field["fieldMeta"] = meta;
            }
            field
        })
        .collect();
    json!({
        "type": "DOCUMENT",
        "title": document_title(r, signer.name, values),
        "externalId": external_id(id),
        "recipients": [{
            "email": signer.email.trim(), "name": signer.name.trim(), "role": "SIGNER", "fields": fields,
        }],
        "meta": meta(spec, r),
    })
}

/// Spec-wide meta, then the release's, then its subject and message.
fn meta(spec: &Spec, r: &Release) -> Value {
    let mut m: serde_json::Map<String, Value> = spec.meta.clone().into_iter().chain(r.meta.clone()).collect();
    m.insert("subject".into(), json!(r.subject));
    m.insert("message".into(), json!(r.message));
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = include_str!("../../../documenso/releases.toml");

    #[test]
    fn shipped_spec_is_valid_and_gated() {
        let spec = Spec::parse(SPEC).unwrap_or_else(|e| panic!("documenso/releases.toml is invalid:\n{e}"));
        let keys: Vec<&str> = spec.releases.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "onboarding",
                "liability-waiver",
                "photo-release",
                "nda",
                "volunteer-agreement",
                "cat-adoption",
                "cat-foster",
                "cat-surrender",
                "conflict-of-interest"
            ]
        );
        assert!(!spec.reviewed_by_counsel, "no attorney has reviewed them; say so until one has");
    }

    #[test]
    fn the_signers_link_is_taken_from_the_distribute_response() {
        let r = json!({ "success": true, "id": "envelope_1", "recipients": [
            { "email": "someone@else.example", "signingUrl": "https://sign.example/sign/other" },
            { "email": "jane@example.org", "signingUrl": "https://sign.example/sign/tok" },
        ]});
        assert_eq!(signing_url(&r, " Jane@Example.org ").as_deref(), Some("https://sign.example/sign/tok"));
        assert_eq!(signing_url(&r, "nobody@example.org"), None);
        assert_eq!(signing_url(&json!({ "success": true }), "jane@example.org"), None, "older instances");
        let odd = json!({ "recipients": [{ "email": "jane@example.org", "signingUrl": "javascript:alert(1)" }] });
        assert_eq!(signing_url(&odd, "jane@example.org"), None, "only web links are shown as links");
    }

    /// The committed layout of `r`, as the core loads it.
    fn layout(r: &Release) -> Vec<Field> {
        let path = r.layout_path();
        let json = std::fs::read_to_string(format!("../../{path}")).unwrap_or_else(|e| panic!("{path}: {e}"));
        parse_layout(r, &json).unwrap_or_else(|e| panic!("{e}"))
    }

    /// Every release has a layout, and it passes the checks the core makes
    /// at startup: one signature on the last page, the detail boxes the
    /// release lists, every box on the page.
    #[test]
    fn every_release_has_a_valid_layout() {
        let spec = Spec::parse(SPEC).expect("spec");
        for r in &spec.releases {
            let fields = layout(r);
            let sign = fields.iter().find(|f| f.kind == FieldKind::Signature).expect("signature");
            // The standard block, where every one-page release has had it.
            assert_eq!((sign.x, sign.y, sign.width, sign.height), (10.588, 77.727, 41.176, 5.0), "{}", r.key);
        }
    }

    /// Everything joining takes is signed once, with its release initialled
    /// and the emergency contact filled in by the signer.
    #[test]
    fn the_welcome_agreement_is_signed_once() {
        let spec = Spec::parse(SPEC).expect("spec");
        let r = spec.release("onboarding").expect("welcome agreement");
        let fields = layout(r);
        let mut kinds: Vec<FieldKind> = fields.iter().map(|f| f.kind).collect();
        kinds.sort_by_key(|k| format!("{k:?}"));
        use FieldKind::*;
        assert_eq!(kinds, [Date, Email, Initials, Name, Signature, Text, Text], "the release");
        assert!(r.details.iter().all(|d| d.by == FilledBy::Signer), "nothing for a sender to fill in");
    }

    /// A release of our own negligence, or a permission given for good, is
    /// only enforceable in California if it is conspicuous. Every document
    /// with one sets it apart (legal/release.typ `release-box`), and the
    /// signer initials each box, once.
    #[test]
    fn every_release_and_irrevocable_permission_is_boxed_and_initialled() {
        let spec = Spec::parse(SPEC).expect("spec");
        let mut boxed = 0;
        for r in &spec.releases {
            let source = std::fs::read_to_string(format!("../../{}", r.source)).expect("source");
            let boxes = source.matches("#release-box").count();
            let initials = layout(r).iter().filter(|f| f.kind == FieldKind::Initials).count();
            assert_eq!(initials, boxes, "{}: one initials box per release-box", r.key);
            let lower = source.to_lowercase();
            for gives_up in ["negligence", "irrevocabl"] {
                assert!(
                    !lower.contains(gives_up) || boxes > 0,
                    "{}: mentions {gives_up:?} outside a release-box",
                    r.key
                );
            }
            boxed += boxes;
        }
        assert!(boxed >= 4, "the welcome agreement, waiver, adoption and foster are boxed");
    }

    #[test]
    fn a_layout_that_disagrees_with_its_release_is_refused() {
        let spec = Spec::parse(SPEC).expect("spec");
        let r = spec.release("volunteer-agreement").expect("volunteer");
        let json = std::fs::read_to_string(format!("../../{}", r.layout_path())).expect("layout");
        let relabelled = json.replacen("Emergency contact phone", "Phone", 1);
        assert!(parse_layout(r, &relabelled).expect_err("label").contains("detail boxes"));
        let mut fields: Vec<Value> = serde_json::from_str(&json).expect("json");
        fields.push(
            json!({ "kind": "SIGNATURE", "page": 2, "label": "Signature", "x": 1, "y": 1, "width": 1, "height": 1 }),
        );
        let overflowed = Value::from(fields).to_string();
        assert!(parse_layout(r, &overflowed).expect_err("page 2").contains("last page"));
        let off = json.replacen("\"x\": 10.588", "\"x\": 90.0", 1);
        assert_ne!(off, json);
        assert!(parse_layout(r, &off).expect_err("off page").contains("isn't on the page"));
    }

    #[test]
    fn sender_details_are_locked_on_the_document_and_signer_details_are_blank() {
        let spec = Spec::parse(SPEC).expect("spec");
        let r = spec.release("cat-surrender").expect("surrender").clone();
        assert_eq!(sender_values(&r, &BTreeMap::new()), Err(vec!["Cat's name and description".to_owned()]));
        let values = sender_values(&r, &BTreeMap::from([("cat".into(), "  Mochi, grey tabby ".into())])).expect("ok");
        let p = envelope_payload(
            &spec,
            &r,
            &layout(&r),
            Signer { name: "Jane Doe", email: "jane@example.org" },
            &values,
            Uuid::nil(),
        );
        assert_eq!(p["title"], "Cat Surrender Agreement · Jane Doe · Mochi, grey tabby");
        let fields = p["recipients"][0]["fields"].as_array().expect("fields");
        assert_eq!(fields.len(), 4 + 2);
        let (cat, history) = (&fields[4], &fields[5]);
        assert_eq!(cat["type"], "TEXT");
        assert_eq!(cat["fieldMeta"]["text"], "Mochi, grey tabby");
        assert_eq!(cat["fieldMeta"]["readOnly"], true);
        // Two details take the row nearest the signature.
        assert_eq!((cat["positionX"].as_f64(), cat["positionY"].as_f64()), (Some(10.588), Some(70.182)));
        assert!(history["fieldMeta"].get("text").is_none() && history["fieldMeta"]["required"] == true);
        assert_eq!(history["positionX"].as_f64(), Some(56.471), "the second detail takes the second slot");
    }

    #[test]
    fn bundles_and_details_are_validated() {
        let bad = SPEC.replacen("releases = [\"cat-adoption\"]", "releases = [\"cat-adopton\"]", 1);
        assert!(Spec::parse(&bad).expect_err("bundle").contains("unknown release"));
        let dup = SPEC.replacen("key = \"microchip\"", "key = \"cat\"", 1);
        assert!(Spec::parse(&dup).expect_err("dup").contains("listed twice"));
    }

    #[test]
    fn payload_tags_the_document_and_trims_the_signer() {
        let spec = Spec::parse(SPEC).expect("spec");
        let id = Uuid::nil();
        let r = spec.release("photo-release").expect("photo");
        let p = envelope_payload(
            &spec,
            r,
            &layout(r),
            Signer { name: "  Jane Doe ", email: " jane@example.org " },
            &BTreeMap::new(),
            id,
        );
        assert_eq!(p["externalId"], "sbh-release:00000000-0000-0000-0000-000000000000");
        assert_eq!(p["title"], "Photo Release · Jane Doe");
        assert_eq!(p["recipients"][0]["email"], "jane@example.org");
        let types: Vec<&str> = p["recipients"][0]["fields"]
            .as_array()
            .expect("fields")
            .iter()
            .filter_map(|f| f["type"].as_str())
            .collect();
        assert_eq!(
            types,
            ["SIGNATURE", "DATE", "NAME", "EMAIL"],
            "a withdrawable photo permission has nothing to initial"
        );
    }

    #[test]
    fn webhooks_are_matched_by_our_external_id_only() {
        let id = Uuid::new_v4();
        let body =
            |event: &str, ext: Value| json!({ "event": event, "payload": { "id": 10, "externalId": ext } }).to_string();
        assert_eq!(
            parse_webhook(body("DOCUMENT_COMPLETED", json!(external_id(id))).as_bytes()),
            Ok(Some(Settled::Completed(id)))
        );
        assert_eq!(
            parse_webhook(body("DOCUMENT_REJECTED", json!(external_id(id))).as_bytes()),
            Ok(Some(Settled::Rejected(id)))
        );
        assert_eq!(parse_webhook(body("DOCUMENT_OPENED", json!(external_id(id))).as_bytes()), Ok(None));
        assert_eq!(
            parse_webhook(body("DOCUMENT_COMPLETED", Value::Null).as_bytes()),
            Ok(None),
            "hand-sent docs are ignored"
        );
        assert_eq!(parse_webhook(body("DOCUMENT_COMPLETED", json!("other:123")).as_bytes()), Ok(None));
        assert!(parse_webhook(b"not json").is_err());
    }

    #[test]
    fn secret_check_rejects_blank_and_wrong() {
        assert!(secret_matches("s3cret", "s3cret"));
        assert!(!secret_matches("s3cre", "s3cret"));
        assert!(!secret_matches("", ""), "an unset secret must never match");
    }

    #[test]
    fn meta_layers_and_typed_fields_win() {
        let mut spec = Spec::parse(SPEC).expect("spec");
        spec.meta = Meta::from([("language".into(), json!("en")), ("timezone".into(), json!("America/Los_Angeles"))]);
        spec.releases[0].meta.insert("language".into(), json!("es"));
        let r = spec.releases[0].clone();
        let m = &envelope_payload(
            &spec,
            &r,
            &[],
            Signer { name: "Kiki", email: "kiki@example.org" },
            &BTreeMap::new(),
            Uuid::nil(),
        )["meta"];
        assert_eq!(m["language"], json!("es"), "a release's own meta wins");
        assert_eq!(m["timezone"], json!("America/Los_Angeles"));
        assert_eq!(m["subject"], json!(r.subject));
        let owned = SPEC.replacen("\n[meta]\n", "\n[meta]\nsubject = \"x\"\n", 1);
        assert_ne!(owned, SPEC, "the shipped spec has a [meta] table to add to");
        assert!(Spec::parse(&owned).expect_err("owned").contains("own field"));
    }

    #[test]
    fn meta_the_instance_would_drop_is_named() {
        let mut spec = Spec::parse(SPEC).expect("spec");
        spec.meta = Meta::from([("languge".into(), json!("en")), ("timezone".into(), json!("UTC"))]);
        // Shape of Documenso's openapi.json (observed on the self-hosted instance).
        let meta = json!({ "type": "object", "properties": { "timezone": {}, "language": {}, "subject": {} } });
        let schema = json!({ "properties": { "payload": { "properties": { "meta": meta } } } });
        let body = json!({ "content": { "multipart/form-data": { "schema": schema } } });
        let openapi = json!({ "paths": { "/envelope/create": { "post": { "requestBody": body } } } });
        assert_eq!(unknown_meta(&spec, &openapi), Ok(vec!["languge".to_owned()]));
        assert!(unknown_meta(&spec, &json!({})).is_err(), "no schema is an error, not a pass");
    }
}
