//! Search across the app. A caller searches only the kinds of record they
//! could open (`SearchKind::visible`), so a result never reveals a record
//! they couldn't otherwise read; incidents aren't searchable at all.

use axum::extract::Query;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use homestead_domain::search::{SearchHit, SearchKind, prefix_query};
use maud::html;
use serde::Deserialize;

use crate::AppState;
use crate::api::ApiResult;
use crate::auth::{Authed, Principal, deny};
use crate::ui::{Back, UiResult, layout};

pub fn ui_routes() -> Router<AppState> {
    Router::new().route("/search", get(page))
}

pub fn api_routes() -> Router<AppState> {
    Router::new().route("/search", get(api))
}

/// Results per kind: enough to find a thing by name, few enough that a
/// one-letter query stays cheap.
const PER_KIND: i64 = 8;

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: String,
}

/// The kinds `who` may search. There is no single capability for search:
/// it's refused to someone who can't open any kind of record at all.
fn kinds(who: &Principal) -> Option<Vec<SearchKind>> {
    Some(SearchKind::visible(who.caps)).filter(|k| !k.is_empty())
}

fn refuse(who: &Principal) -> Response {
    deny(who, SearchKind::ALL[0].read_cap())
}

async fn page(a: Authed, Query(q): Query<SearchQuery>) -> UiResult {
    let Some(kinds) = kinds(&a.who) else { return Ok(refuse(&a.who)) };
    let query = prefix_query(&q.q);
    let hits = match &query {
        Some(query) => a.store.search(&kinds, query, PER_KIND).await.back("/")?,
        None => Vec::new(),
    };
    Ok(layout(
        "Search",
        html! {
            form method="get" action="/search" role="search" {
                input type="search" name="q" value=(q.q) autofocus aria-label="Search";
                " " button { "Search" }
            }
            @if query.is_some() {
                @if hits.is_empty() { p.muted { "Nothing matches “" (q.q.trim()) "”." } } @else {
                    table {
                        @for h in &hits {
                            tr {
                                td.muted { (h.kind.label()) }
                                td { a href=(h.url) { (h.title) } @if !h.subtitle.is_empty() { br; span.muted { (h.subtitle) } } }
                            }
                        }
                    }
                }
            }
            p.muted {
                "Matches the start of any word. Searching: "
                @for (n, k) in kinds.iter().enumerate() { @if n > 0 { ", " } (k.label().to_lowercase()) } "."
            }
        },
    )
    .into_response())
}

async fn api(a: Authed, Query(q): Query<SearchQuery>) -> ApiResult {
    let Some(kinds) = kinds(&a.who) else { return Ok(refuse(&a.who)) };
    let hits: Vec<SearchHit> = match prefix_query(&q.q) {
        Some(query) => a.store.search(&kinds, &query, PER_KIND).await?,
        None => Vec::new(),
    };
    Ok(Json(hits).into_response())
}
