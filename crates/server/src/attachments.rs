//! Uploading, downloading and removing attached files, for any record.
//!
//! Uploads pass through the app (the UI has no JavaScript, and it lets the
//! app judge the file by its bytes and hash it before storage sees it).
//! Downloads are authorized here, then served as a short-lived presigned
//! link to the private bucket, or streamed when storage is local.

use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, Path, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use homestead_domain::attachments::{Attachment, AttachmentOwner, NewAttachment, ascii_filename, safe_filename, sniff};
use homestead_domain::auth::Capability;
use homestead_domain::incidents::IncidentStatus;
use homestead_integrations::blobs::{Blobs, Download, Headers};
use maud::{Markup, html};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Authed, Principal, Via, deny, new_secret};
use crate::ui::{error_page, see};

/// Configured storage and limits.
#[derive(Clone)]
pub struct Storage {
    pub blobs: Blobs,
    pub max_bytes: usize,
    pub link_secs: u32,
    /// Days a deleted file stays restorable before its bytes are purged.
    pub retain_deleted_days: i32,
}

pub fn ui_routes(max_bytes: usize) -> Router<AppState> {
    Router::new()
        .route("/attachments/{owner}/{owner_id}", post(upload))
        .route("/attachments/{id}", get(download))
        .route("/attachments/{id}/delete", post(delete))
        // Room for the file plus the multipart envelope; anything bigger is
        // refused before it is read into memory.
        .layer(DefaultBodyLimit::max(max_bytes + 64 * 1024))
}

pub fn api_routes(max_bytes: usize) -> Router<AppState> {
    Router::new()
        .route("/attachments/{owner}/{owner_id}", post(upload))
        .route("/attachments/{id}", get(download))
        .layer(DefaultBodyLimit::max(max_bytes + 64 * 1024))
}

/// Refuses callers who can't read (or write) files on any kind of record,
/// before anything is looked up.
fn any_of(who: &Principal, cap: fn(AttachmentOwner) -> Capability, reporters: bool) -> Result<(), Box<Response>> {
    // Reporters reach their own incident's files (see `own_report`) to add
    // or read them, never to remove them.
    if AttachmentOwner::ALL.iter().any(|o| who.can(cap(*o))) || (reporters && who.can(Capability::IncidentsReport)) {
        Ok(())
    } else {
        Err(Box::new(deny(who, cap(AttachmentOwner::ALL[0]))))
    }
}

/// The one exception to "as private as the record": whoever filed an
/// incident may add evidence while it's open, and see what's attached to
/// their own report, without being able to read anyone else's.
async fn own_report(a: &Authed, owner: AttachmentOwner, owner_id: Uuid, adding: bool) -> bool {
    if owner != AttachmentOwner::Incident || !a.who.can(Capability::IncidentsReport) {
        return false;
    }
    matches!(
        a.store.incident_reporter(owner_id).await,
        Ok(Some((Some(by), status))) if by == a.who.actor.id && (!adding || status == IncidentStatus::Open)
    )
}

/// Where to send the browser afterwards: a path on this site, nothing else.
fn local_path(back: &str) -> &str {
    if back.starts_with('/') && !back.starts_with("//") && !back.contains('\\') { back } else { "/" }
}

fn content_disposition(a_media_type: &str, name: &str) -> String {
    let kind = if a_media_type.starts_with("image/") { "inline" } else { "attachment" };
    let encoded: String = form_urlencoded::byte_serialize(name.as_bytes()).collect::<String>().replace('+', "%20");
    format!("{kind}; filename=\"{}\"; filename*=UTF-8''{encoded}", ascii_filename(name))
}

async fn upload(
    a: Authed,
    State(s): State<AppState>,
    Path((owner, owner_id)): Path<(String, Uuid)>,
    request: Request,
) -> Response {
    if let Err(r) = any_of(&a.who, AttachmentOwner::write_cap, true) {
        return *r;
    }
    let owner = match owner.parse::<AttachmentOwner>() {
        Ok(o) => o,
        Err(e) => return (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    };
    if !a.who.can(owner.write_cap()) && !own_report(&a, owner, owner_id, true).await {
        return deny(&a.who, owner.write_cap());
    }
    // Parsed only now, so nobody learns anything about the body's format
    // before they're authorized.
    let mut form = match Multipart::from_request(request, &s).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    let mut back = String::from("/");
    let mut file: Option<(String, Vec<u8>)> = None;
    loop {
        match form.next_field().await {
            Ok(Some(field)) => match field.name() {
                Some("back") => back = field.text().await.unwrap_or_default(),
                Some("file") => {
                    let name = field.file_name().unwrap_or("file").to_owned();
                    match field.bytes().await {
                        Ok(b) => file = Some((name, b.to_vec())),
                        Err(e) => return (e.status(), e.body_text()).into_response(),
                    }
                }
                _ => {}
            },
            Ok(None) => break,
            Err(e) => return (e.status(), e.body_text()).into_response(),
        }
    }
    let back = local_path(&back);
    let result = store_file(&a, s.attachments.as_ref(), owner, owner_id, file).await;
    match (result, a.who.via) {
        (Ok(_), Via::Session) => see(back),
        (Ok(att), _) => (StatusCode::CREATED, axum::Json(att)).into_response(),
        (Err((status, message)), Via::Session) => (status, error_page(&message, back)).into_response(),
        (Err((status, message)), _) => (status, axum::Json(serde_json::json!({ "error": message }))).into_response(),
    }
}

async fn store_file(
    a: &Authed,
    storage: Option<&Storage>,
    owner: AttachmentOwner,
    owner_id: Uuid,
    file: Option<(String, Vec<u8>)>,
) -> Result<Attachment, (StatusCode, String)> {
    let Some(storage) = storage else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "File storage isn't set up yet ([attachments] in homestead.toml).".into(),
        ));
    };
    let Some((name, bytes)) = file.filter(|(_, b)| !b.is_empty()) else {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "Choose a file to attach.".into()));
    };
    if bytes.len() > storage.max_bytes {
        let mib = storage.max_bytes / (1024 * 1024);
        return Err((StatusCode::PAYLOAD_TOO_LARGE, format!("That file is too big (the limit is {mib} MiB).")));
    }
    let Some(media_type) = sniff(&bytes) else {
        return Err((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Only photos (JPEG, PNG, GIF, WebP, HEIC) and PDFs can be attached.".into(),
        ));
    };
    let name = safe_filename(&name);
    let sha256 = hex::encode(Sha256::digest(&bytes));
    let key = format!("att/{}", new_secret().0);
    let headers = Headers { content_type: media_type, content_disposition: &content_disposition(media_type, &name) };
    if let Err(e) = storage.blobs.put(&key, &bytes, &sha256, headers).await {
        tracing::error!(error = %e, "attachment upload to storage failed");
        return Err((StatusCode::BAD_GATEWAY, "File storage didn't accept the file; try again.".into()));
    }
    let input = NewAttachment {
        owner,
        owner_id,
        original_name: name,
        media_type: media_type.to_owned(),
        byte_size: bytes.len() as i64,
        sha256,
        storage_key: key.clone(),
    };
    match a.store.record_attachment(input).await {
        Ok(att) => Ok(att),
        Err(e) => {
            // Don't leave bytes nobody can reach. A locked bucket refuses to
            // delete a fresh object, so those wait in the purge queue.
            if let Err(del) = storage.blobs.delete(&key).await
                && let Err(q) = a.store.queue_unrecorded_purge(&key, storage.retain_deleted_days).await
            {
                tracing::error!(error = %del, queue_error = %q, key, "an unrecorded upload is orphaned in storage");
            }
            Err(crate::status_of(&e))
        }
    }
}

/// The attachment, if the caller may read it. "Not yours to see" and
/// "doesn't exist" are both 404, so ids can't be probed.
async fn readable(
    a: &Authed,
    id: Uuid,
    cap: fn(AttachmentOwner) -> Capability,
    reporter_may: bool,
) -> Result<Attachment, Box<Response>> {
    any_of(&a.who, cap, reporter_may)?;
    match a.store.get_attachment(id).await {
        Ok(att) if a.who.can(cap(att.owner)) => Ok(att),
        Ok(att) if reporter_may && own_report(a, att.owner, att.owner_id, false).await => Ok(att),
        Ok(_) => Err(Box::new(StatusCode::NOT_FOUND.into_response())),
        Err(e) => Err(Box::new(crate::status_of(&e).into_response())),
    }
}

async fn download(a: Authed, State(s): State<AppState>, Path(id): Path<Uuid>) -> Response {
    let att = match readable(&a, id, AttachmentOwner::read_cap, true).await {
        Ok(att) => att,
        Err(r) => return *r,
    };
    let Some(storage) = s.attachments.as_ref() else { return StatusCode::SERVICE_UNAVAILABLE.into_response() };
    match storage.blobs.download(&att.storage_key, storage.link_secs).await {
        Ok(Download::Redirect(url)) => (
            StatusCode::SEE_OTHER,
            [
                (header::LOCATION, url),
                (header::CACHE_CONTROL, "no-store".into()),
                (header::REFERRER_POLICY, "no-referrer".into()),
            ],
        )
            .into_response(),
        Ok(Download::Bytes(bytes)) => (
            [
                (header::CONTENT_TYPE, att.media_type.clone()),
                (header::CONTENT_DISPOSITION, content_disposition(&att.media_type, &att.original_name)),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
                (header::CONTENT_SECURITY_POLICY, "sandbox; default-src 'none'".into()),
                (header::CACHE_CONTROL, "private, no-store".into()),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, attachment = %id, "attachment download failed");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

#[derive(Deserialize)]
struct DeleteForm {
    #[serde(default)]
    back: String,
}

async fn delete(a: Authed, State(s): State<AppState>, Path(id): Path<Uuid>, Form(f): Form<DeleteForm>) -> Response {
    let back = local_path(&f.back);
    let att = match readable(&a, id, AttachmentOwner::write_cap, false).await {
        Ok(att) => att,
        Err(r) => return *r,
    };
    // The bytes stay (restorable) until the purge pass; see `purge_due`.
    let retain = s.attachments.as_ref().map_or(0, |st| st.retain_deleted_days);
    if let Err(e) = a.store.delete_attachment(att.id, retain).await {
        let (status, message) = crate::status_of(&e);
        return (status, error_page(&message, back)).into_response();
    }
    see(back)
}

/// Deletes the bytes of files whose retention has run out. A delete the
/// storage refuses (a bucket lock still holding the object) stays queued
/// for the next pass. Returns how many were purged.
pub async fn purge_due(
    store: &homestead_store::Store,
    storage: &Storage,
) -> Result<usize, homestead_store::StoreError> {
    let mut purged = 0;
    for key in store.due_blob_purges(100).await? {
        match storage.blobs.delete(&key).await {
            Ok(()) => {
                store.blob_purged(&key).await?;
                purged += 1;
            }
            Err(e) => tracing::warn!(error = %e, key, "purge refused; will retry"),
        }
    }
    Ok(purged)
}

/// The list of files on a record, with an upload form for those who may.
pub fn panel(who: &Principal, owner: AttachmentOwner, owner_id: Uuid, files: &[Attachment], back: &str) -> Markup {
    panel_for(owner, owner_id, files, back, who.can(owner.write_cap()), who.can(owner.write_cap()))
}

/// [`panel`] with explicit rights (a reporter may add to, but not remove
/// from, their own report).
pub fn panel_for(
    owner: AttachmentOwner,
    owner_id: Uuid,
    files: &[Attachment],
    back: &str,
    can_add: bool,
    can_write: bool,
) -> Markup {
    html! {
        @if !files.is_empty() {
            ul.files {
                @for f in files {
                    li {
                        @if f.is_image() {
                            a href={"/attachments/" (f.id)} { img src={"/attachments/" (f.id)} alt=(f.original_name) loading="lazy" style="max-width:8rem;max-height:6rem;vertical-align:middle"; }
                            " "
                        }
                        a href={"/attachments/" (f.id)} { (f.original_name) }
                        span.muted { " · " (f.byte_size / 1024 + 1) " KB · " (f.created_at.date()) }
                        @if can_write {
                            form.inline method="post" action={"/attachments/" (f.id) "/delete"} {
                                input type="hidden" name="back" value=(back);
                                button.small { "Remove" }
                            }
                        }
                    }
                }
            }
        }
        @if can_add {
            form.inline method="post" enctype="multipart/form-data" action={"/attachments/" (owner) "/" (owner_id)} {
                input type="hidden" name="back" value=(back);
                input type="file" name="file" accept="image/*,application/pdf" required;
                button.small { "Attach" }
            }
        }
    }
}
