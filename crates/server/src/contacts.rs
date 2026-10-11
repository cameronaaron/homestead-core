//! The mailing list: add, import from the website's sign-up export,
//! unsubscribe, and export a CSV for whatever email tool sends the news.

use axum::extract::Query;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Router};
use homestead_domain::contacts::{Interest, NewContact};
use maud::html;
use serde::Deserialize;

use crate::AppState;
use crate::auth::{Can, cap};
use crate::ui::{Back, UiError, UiResult, layout, see};

pub fn ui_routes() -> Router<AppState> {
    Router::new()
        .route("/contacts", get(page).post(add))
        .route("/contacts/import", post(import))
        .route("/contacts/unsubscribe", post(unsubscribe))
        .route("/contacts/export.csv", get(export))
}

fn interest_label(i: Interest) -> &'static str {
    match i {
        Interest::Newsletter => "News",
        Interest::Volunteering => "Volunteering",
        Interest::Workshops => "Workshops",
        Interest::CatAdoption => "Adopting a cat",
        Interest::Donating => "Donating",
    }
}

async fn page(c: Can<cap::ContactsRead>) -> UiResult {
    let export = c.who.can(homestead_domain::auth::Capability::ContactsExport);
    const B: &str = "/contacts";
    let all = c.store.subscribed_contacts(None).await.back(B)?;
    let gone = c.store.unsubscribed_count().await.back(B)?;
    Ok(layout(
        "Mailing list",
        html! {
            p.muted { "People who asked to hear from us. An unsubscribe is permanent: imports never re-add them." }
            div.grid {
                div.card { div.stat { (all.len()) } div.muted { "subscribed" } }
                @for i in Interest::ALL {
                    div.card {
                        div.stat { (all.iter().filter(|c| c.interests.contains(i)).count()) }
                        div.muted {
                            (interest_label(*i))
                            @if export { " · " a href={"/contacts/export.csv?interest=" (i)} { "CSV" } }
                        }
                    }
                }
                div.card { div.stat { (gone) } div.muted { "unsubscribed" } }
            }
            @if export { p { a href="/contacts/export.csv" { "Download everyone subscribed (CSV)" } } }
            div.grid {
                @if c.who.can(homestead_domain::auth::Capability::ContactsWrite) {
                    form.card method="post" action="/contacts" {
                        strong { "Add someone" }
                        label { "Name" input name="name"; }
                        label { "Email" input name="email" type="email" required; }
                        @for i in Interest::ALL { label.inline { input type="checkbox" name="interests" value=(i); (interest_label(*i)) } }
                        label { "Where they signed up" input name="source" required placeholder="Farm stand clipboard, workshop…"; }
                        button { "Add" }
                    }
                }
                @if c.who.can(homestead_domain::auth::Capability::ContactsWrite) {
                    form.card method="post" action="/contacts/import" {
                        strong { "Import a CSV" }
                        p.muted { "Columns: email (required), name, interests (separated by ;). Export the website's sign-ups from Wix and paste them here." }
                        label { "Where these came from" input name="source" required value="website"; }
                        label { "CSV" textarea name="csv" rows="8" required placeholder="name,email,interests\nKiki,kiki@example.org,workshops;cat_adoption" {} }
                        button { "Import" }
                    }
                }
                @if c.who.can(homestead_domain::auth::Capability::ContactsWrite) {
                    form.card method="post" action="/contacts/unsubscribe" {
                        strong { "Unsubscribe someone" }
                        label { "Email" input name="email" type="email" required; }
                        button { "Unsubscribe" }
                    }
                }
            }
        },
    )
    .into_response())
}

async fn add(c: Can<cap::ContactsWrite>, body: String) -> UiResult {
    const B: &str = "/contacts";
    // Repeated `interests` checkboxes don't fit serde_urlencoded's structs.
    let (mut name, mut email, mut source, mut interests) = (String::new(), String::new(), String::new(), vec![]);
    for (k, v) in form_urlencoded::parse(body.as_bytes()) {
        match k.as_ref() {
            "name" => name = v.into_owned(),
            "email" => email = v.into_owned(),
            "source" => source = v.into_owned(),
            "interests" => interests.push(v.parse::<Interest>().back(B)?),
            _ => {}
        }
    }
    c.store.add_contact(NewContact { name, email, interests, source }).await.back(B)?;
    Ok(see(B))
}

#[derive(Deserialize)]
struct ImportForm {
    source: String,
    csv: String,
}

async fn import(c: Can<cap::ContactsWrite>, Form(f): Form<ImportForm>) -> UiResult {
    const B: &str = "/contacts";
    let mut reader = csv::ReaderBuilder::new().trim(csv::Trim::All).from_reader(f.csv.as_bytes());
    let headers = reader.headers().map_err(|e| UiError::invalid(format!("CSV: {e}"), B))?.clone();
    let col = |n: &str| headers.iter().position(|h| h.eq_ignore_ascii_case(n));
    let email_col = col("email").ok_or_else(|| UiError::invalid("CSV needs an email column", B))?;
    let (name_col, interests_col) = (col("name"), col("interests"));
    let mut rows = vec![];
    for (line, record) in reader.records().enumerate() {
        let r = record.map_err(|e| UiError::invalid(format!("CSV row {}: {e}", line + 2), B))?;
        let interests = match interests_col.and_then(|i| r.get(i)) {
            Some(list) => list
                .split(';')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(|x| x.parse::<Interest>())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| UiError::invalid(format!("CSV row {}: {e}", line + 2), B))?,
            None => vec![],
        };
        rows.push(NewContact {
            name: name_col.and_then(|i| r.get(i)).unwrap_or_default().to_owned(),
            email: r.get(email_col).unwrap_or_default().to_owned(),
            interests,
            source: f.source.clone(),
        });
    }
    let report = c.store.import_contacts(rows).await.back(B)?;
    Ok(layout(
        "Imported",
        html! {
            p { (report.added) " added, " (report.updated) " already on the list (interests merged), "
                (report.kept_unsubscribed) " left unsubscribed because they had opted out." }
            p { a href=(B) { "← Mailing list" } }
        },
    )
    .into_response())
}

#[derive(Deserialize)]
struct EmailForm {
    email: String,
}

async fn unsubscribe(c: Can<cap::ContactsWrite>, Form(f): Form<EmailForm>) -> UiResult {
    const B: &str = "/contacts";
    if !c.store.unsubscribe_contact(&f.email).await.back(B)? {
        return Err(UiError::invalid("nobody with that email is on the list", B));
    }
    Ok(see(B))
}

/// A spreadsheet treats a cell starting with `= + - @` (or a tab/CR) as a
/// formula; names come from a public sign-up form, so neutralise them.
pub fn csv_cell(s: &str) -> String {
    if s.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{s}") } else { s.to_owned() }
}

#[derive(Deserialize)]
struct ExportQuery {
    #[serde(default)]
    interest: Option<String>,
}

async fn export(c: Can<cap::ContactsExport>, Query(q): Query<ExportQuery>) -> UiResult {
    const B: &str = "/contacts";
    let interest = match q.interest.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(i) => Some(i.parse::<Interest>().back(B)?),
    };
    let contacts = c.store.subscribed_contacts(interest).await.back(B)?;
    let mut w = csv::Writer::from_writer(vec![]);
    let internal = |e: csv::Error| UiError::invalid(format!("CSV: {e}"), B);
    w.write_record(["name", "email", "interests", "source", "consented_at"]).map_err(internal)?;
    for c in &contacts {
        let interests: Vec<&str> = c.interests.iter().map(|i| i.as_str()).collect();
        let consented = c.consented_at.date().to_string();
        w.write_record([csv_cell(&c.name), c.email.clone(), interests.join(";"), csv_cell(&c.source), consented])
            .map_err(internal)?;
    }
    let bytes = w.into_inner().map_err(|e| UiError::invalid(format!("CSV: {e}"), B))?;
    let name = interest.map_or("contacts".to_owned(), |i| format!("contacts-{i}"));
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}.csv\"")),
        ],
        bytes,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::csv_cell;

    #[test]
    fn formula_looking_cells_are_neutralised() {
        for evil in ["=HYPERLINK(\"http://x\")", "+1", "-2+3", "@SUM(A1)", "\tx"] {
            assert!(csv_cell(evil).starts_with('\''), "{evil:?}");
        }
        assert_eq!(csv_cell("Kiki"), "Kiki");
        assert_eq!(csv_cell("Mary-Jane"), "Mary-Jane", "only a leading sign matters");
    }
}
