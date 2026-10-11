//! Attachments through the full router: private, typed by their bytes,
//! hashed, and exactly as visible as the record they belong to.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use homestead_domain::auth::Role;
use homestead_integrations::blobs::Blobs;
use homestead_server::attachments::Storage;
use homestead_server::{AppState, app};
use homestead_store::Store;
use http_body_util::BodyExt;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tower::ServiceExt;

mod common;

const VOLUNTEER: &str = "0000000000000000000000000000000000000000000000000000000000000003";
const FARM_LEAD: &str = "0000000000000000000000000000000000000000000000000000000000000005";
const BOT: &str = "0000000000000000000000000000000000000000000000000000000000000007";
const JPEG: &[u8] = b"\xFF\xD8\xFF\xE0\x00\x10JFIF\x00 a very small cat photo";
const PDF: &[u8] = b"%PDF-1.7\n1 0 obj << >> endobj\n%%EOF";

fn router(pool: &PgPool, blobs: Blobs, max_bytes: usize) -> Router {
    app(AppState {
        attachments: Some(Storage { blobs, max_bytes, link_secs: 300, retain_deleted_days: 30 }),
        ..AppState::new(Store::from_pool(pool.clone()))
    })
}

async fn send(r: &Router, req: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let resp = r.clone().oneshot(req).await.expect("infallible");
    let (parts, body) = resp.into_parts();
    (parts.status, parts.headers, body.collect().await.expect("body").to_bytes().to_vec())
}

fn multipart(name: &str, bytes: &[u8], back: &str) -> (String, Vec<u8>) {
    let boundary = "xXhomesteadXx";
    let mut body = Vec::new();
    body.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"back\"\r\n\r\n{back}\r\n").as_bytes());
    body.extend(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\
             Content-Type: image/jpeg\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend(bytes);
    body.extend(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

fn upload(cookie: &str, owner: &str, id: &str, name: &str, bytes: &[u8]) -> Request<Body> {
    let (ct, body) = multipart(name, bytes, "/animals");
    Request::post(format!("/attachments/{owner}/{id}"))
        .header(header::COOKIE, format!("sbh_session={cookie}"))
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body))
        .expect("req")
}

fn get(cookie: &str, uri: &str) -> Request<Body> {
    Request::get(uri).header(header::COOKIE, format!("sbh_session={cookie}")).body(Body::empty()).expect("req")
}

async fn cat(pool: &PgPool) -> uuid::Uuid {
    sqlx::query_scalar(
        "insert into homestead.animals (species, name, sex, status, origin, arrived_on)
         values ('cat', 'Mochi', 'female', 'in_care', 'trapped', '2026-09-01') returning id",
    )
    .fetch_one(pool)
    .await
    .expect("cat")
}

async fn only_attachment(pool: &PgPool) -> (uuid::Uuid, String, String, i64, Option<uuid::Uuid>, String) {
    sqlx::query_as("select id, storage_key, sha256, byte_size, created_by, original_name from homestead.attachments")
        .fetch_one(pool)
        .await
        .expect("one attachment")
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_photo_is_stored_privately_hashed_and_served_safely(pool: PgPool) {
    let kiki = common::steward(&pool).await;
    let blobs = Blobs::memory();
    let r = router(&pool, blobs.clone(), 1024 * 1024);
    let id = cat(&pool).await;

    let (status, headers, _) =
        send(&r, upload(common::STEWARD, "animal", &id.to_string(), "../../Mochi;best.jpg", JPEG)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/animals");

    let (att, key, sha, size, by, name) = only_attachment(&pool).await;
    assert_eq!(sha, hex::encode(Sha256::digest(JPEG)));
    assert_eq!((size, by, name.as_str()), (JPEG.len() as i64, Some(kiki.id), "Mochi_best.jpg"));
    assert!(key.starts_with("att/") && key.len() == 68 && !key.contains(&id.to_string()), "unpredictable: {key}");
    let Blobs::Memory(m) = &blobs else { unreachable!() };
    assert_eq!(m.lock().expect("lock").get(&key).map(Vec::as_slice), Some(JPEG));

    let (_, _, page) = send(&r, get(common::STEWARD, &format!("/animals/{id}"))).await;
    assert!(String::from_utf8_lossy(&page).contains(&format!("/attachments/{att}")));

    let (status, headers, body) = send(&r, get(common::STEWARD, &format!("/attachments/{att}"))).await;
    assert_eq!((status, body.as_slice()), (StatusCode::OK, JPEG));
    assert_eq!(headers[header::CONTENT_TYPE], "image/jpeg");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert!(headers[header::CONTENT_SECURITY_POLICY].to_str().expect("ascii").contains("sandbox"));
    assert!(
        headers[header::CONTENT_DISPOSITION]
            .to_str()
            .expect("ascii")
            .starts_with("inline; filename=\"Mochi_best.jpg\"")
    );
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn files_are_judged_by_their_bytes_size_and_owner(pool: PgPool) {
    common::steward(&pool).await;
    let blobs = Blobs::memory();
    let r = router(&pool, blobs.clone(), 64);
    let id = cat(&pool).await.to_string();
    let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\" onload=\"alert(1)\"/>";
    for (owner, oid, name, bytes, want) in [
        ("animal", id.as_str(), "x.jpg", &svg[..], StatusCode::UNSUPPORTED_MEDIA_TYPE),
        ("animal", id.as_str(), "big.jpg", &[JPEG, &[0u8; 100][..]].concat()[..], StatusCode::PAYLOAD_TOO_LARGE),
        ("animal", id.as_str(), "empty.jpg", &b""[..], StatusCode::UNPROCESSABLE_ENTITY),
        ("tractor", id.as_str(), "x.jpg", JPEG, StatusCode::NOT_FOUND),
        ("animal", "8f14e45f-ceea-467a-9575-7d8f7c1c2a11", "x.jpg", JPEG, StatusCode::NOT_FOUND),
    ] {
        let (status, _, body) = send(&r, upload(common::STEWARD, owner, oid, name, bytes)).await;
        assert_eq!(status, want, "{name} on {owner}: {}", String::from_utf8_lossy(&body));
    }
    let count: i64 =
        sqlx::query_scalar("select count(*) from homestead.attachments").fetch_one(&pool).await.expect("n");
    let Blobs::Memory(m) = &blobs else { unreachable!() };
    assert_eq!((count, m.lock().expect("lock").len()), (0, 0), "no rows and no orphaned bytes");

    // Far past the limit is refused before the body is even read.
    let (status, _, _) = send(&r, upload(common::STEWARD, "animal", &id, "huge.jpg", &vec![0xFF; 200 * 1024])).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn an_attachment_is_exactly_as_private_as_its_record(pool: PgPool) {
    common::steward(&pool).await;
    common::person(&pool, "Val", &[Role::Volunteer], VOLUNTEER).await;
    common::person(&pool, "Fern", &[Role::FarmLead], FARM_LEAD).await;
    let r = router(&pool, Blobs::memory(), 1024 * 1024);
    let id = cat(&pool).await;
    let care: uuid::Uuid = sqlx::query_scalar(
        "insert into homestead.animal_care (animal_id, occurred_on, kind) values ($1, '2026-09-02', 'vaccine') returning id",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .expect("care");

    send(&r, upload(common::STEWARD, "animal", &id.to_string(), "photo.jpg", JPEG)).await;
    send(&r, upload(common::STEWARD, "animal_care", &care.to_string(), "rabies.pdf", PDF)).await;
    let ids: Vec<(uuid::Uuid, String)> =
        sqlx::query_as("select id, resource_type from homestead.attachments").fetch_all(&pool).await.expect("ids");
    let find = |t: &str| ids.iter().find(|(_, x)| x == t).expect("uploaded").0;
    let (photo, vet) = (find("animal"), find("animal_care"));

    // A volunteer sees the cat's photo but not its vet record, and can't add
    // either. An unreadable file is indistinguishable from a missing one.
    assert_eq!(send(&r, get(VOLUNTEER, &format!("/attachments/{photo}"))).await.0, StatusCode::OK);
    assert_eq!(send(&r, get(VOLUNTEER, &format!("/attachments/{vet}"))).await.0, StatusCode::NOT_FOUND);
    let (status, _, _) = send(&r, upload(VOLUNTEER, "animal", &id.to_string(), "mine.jpg", JPEG)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // The farm lead has animals.medical and gets the PDF as a download.
    let (status, headers, body) = send(&r, get(FARM_LEAD, &format!("/attachments/{vet}"))).await;
    assert_eq!((status, body.as_slice()), (StatusCode::OK, PDF));
    assert!(headers[header::CONTENT_DISPOSITION].to_str().expect("ascii").starts_with("attachment;"));

    // Removing needs the write capability; the audit log keeps the hash.
    let remove = |who: &str| {
        Request::post(format!("/attachments/{vet}/delete"))
            .header(header::COOKIE, format!("sbh_session={who}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("back=%2Fanimals"))
            .expect("req")
    };
    // A volunteer can attach files to a task-owned record (an obligation),
    // so the request is looked up. Someone else's vet file still answers
    // as if it were not there.
    assert_eq!(send(&r, remove(VOLUNTEER)).await.0, StatusCode::NOT_FOUND);
    assert_eq!(send(&r, remove(FARM_LEAD)).await.0, StatusCode::SEE_OTHER);
    assert_eq!(send(&r, get(FARM_LEAD, &format!("/attachments/{vet}"))).await.0, StatusCode::NOT_FOUND);
    let kept: String = sqlx::query_scalar(
        "select before ->> 'sha256' from homestead.audit_entries where resource_type = 'attachments' and action = 'delete'",
    )
    .fetch_one(&pool)
    .await
    .expect("audited delete");
    assert_eq!(kept, hex::encode(Sha256::digest(PDF)));
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn cloud_storage_downloads_are_short_lived_signed_links(pool: PgPool) {
    common::steward(&pool).await;
    let id = cat(&pool).await;
    sqlx::query(
        "insert into homestead.attachments (resource_type, resource_id, storage_key, original_name, media_type, byte_size, sha256)
         values ('animal', $1, $2, 'cat.jpg', 'image/jpeg', 10, $3)",
    )
    .bind(id)
    .bind(format!("att/{}", "ab".repeat(32)))
    .bind("cd".repeat(32))
    .execute(&pool)
    .await
    .expect("row");
    let (att,): (uuid::Uuid,) =
        sqlx::query_as("select id from homestead.attachments").fetch_one(&pool).await.expect("id");
    let s3 =
        Blobs::s3(reqwest::Client::new(), "https://acct.r2.cloudflarestorage.com", "files", "auto", "KEYID", "secret");
    let r = router(&pool, s3, 1024);
    let (status, headers, _) = send(&r, get(common::STEWARD, &format!("/attachments/{att}"))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let to = headers[header::LOCATION].to_str().expect("ascii");
    assert!(to.starts_with(&format!("https://acct.r2.cloudflarestorage.com/files/att/{}?", "ab".repeat(32))), "{to}");
    assert!(to.contains("X-Amz-Expires=300") && to.contains("X-Amz-Signature="), "{to}");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn services_attach_receipts_through_the_api(pool: PgPool) {
    common::service(&pool, "receipt scanner", BOT).await;
    let r = router(&pool, Blobs::memory(), 1024 * 1024);
    let purchase: uuid::Uuid = sqlx::query_scalar(
        "insert into homestead.purchases (purchased_on, amount, category) values ('2026-09-10', 42.50, 'feed') returning id",
    )
    .fetch_one(&pool)
    .await
    .expect("purchase");
    let (ct, body) = multipart("receipt.pdf", PDF, "/");
    let req = Request::post(format!("/api/v1/attachments/purchase/{purchase}"))
        .header(header::AUTHORIZATION, format!("Bearer {BOT}"))
        .header("idempotency-key", "receipt-1")
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body))
        .expect("req");
    let (status, _, body) = send(&r, req).await;
    assert_eq!(status, StatusCode::CREATED, "{}", String::from_utf8_lossy(&body));
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["media_type"], "application/pdf");
    assert!(json.get("storage_key").is_none(), "storage keys stay server-side");
    // Services never see animals' files.
    let (ct, body) = multipart("x.jpg", JPEG, "/");
    let req = Request::post(format!("/api/v1/attachments/animal/{purchase}"))
        .header(header::AUTHORIZATION, format!("Bearer {BOT}"))
        .header("idempotency-key", "cat-1")
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body))
        .expect("req");
    assert_eq!(send(&r, req).await.0, StatusCode::FORBIDDEN);
}

/// Deleting a file keeps its bytes (restorable) for the retention period,
/// then the purge pass removes them; a purge the storage refuses (the R2
/// bucket lock still holding the object) stays queued, never forgotten.
#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn deleted_files_are_restorable_until_purged_after_retention(pool: PgPool) {
    use homestead_server::attachments::purge_due;
    use std::sync::{Arc, Mutex};

    common::steward(&pool).await;
    let bytes: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>> = Arc::default();
    let storage = Storage {
        blobs: Blobs::Memory(bytes.clone()),
        max_bytes: 1024 * 1024,
        link_secs: 300,
        retain_deleted_days: 30,
    };
    let r = app(AppState { attachments: Some(storage.clone()), ..AppState::new(Store::from_pool(pool.clone())) });
    let store = Store::from_pool(pool.clone());
    let id = cat(&pool).await;
    send(&r, upload(common::STEWARD, "animal", &id.to_string(), "photo.jpg", JPEG)).await;
    let (att, key, ..) = only_attachment(&pool).await;
    let remove = Request::post(format!("/attachments/{att}/delete"))
        .header(header::COOKIE, format!("sbh_session={}", common::STEWARD))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("back=%2Fanimals"))
        .expect("req");
    assert_eq!(send(&r, remove).await.0, StatusCode::SEE_OTHER);
    assert_eq!(send(&r, get(common::STEWARD, &format!("/attachments/{att}"))).await.0, StatusCode::NOT_FOUND);
    assert!(bytes.lock().expect("lock").contains_key(&key), "bytes outlive the record");
    assert_eq!(purge_due(&store, &storage).await.expect("purge"), 0, "not due for 30 days");

    // Within retention, the delete can be undone, bytes and all.
    assert_eq!(store.restore_attachment(att).await.expect("restore").storage_key, key);
    assert_eq!(send(&r, get(common::STEWARD, &format!("/attachments/{att}"))).await.0, StatusCode::OK);
    assert!(store.restore_attachment(att).await.is_err(), "nothing left to restore");

    // Deleted again and past retention: the bytes go, and so does the queue entry.
    store.delete_attachment(att, 30).await.expect("delete");
    sqlx::query("update homestead.attachment_purges set purge_after = now() - interval '1 second'")
        .execute(&pool)
        .await
        .expect("age");
    assert_eq!(purge_due(&store, &storage).await.expect("purge"), 1);
    assert!(!bytes.lock().expect("lock").contains_key(&key));
    assert!(store.due_blob_purges(10).await.expect("due").is_empty());
    assert!(store.restore_attachment(att).await.is_err(), "purged for good");
}

#[sqlx::test(migrator = "homestead_store::MIGRATOR")]
async fn a_purge_the_bucket_lock_refuses_stays_queued(pool: PgPool) {
    use homestead_server::attachments::purge_due;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    // R2 answers a delete of a locked object with 403 AccessDenied.
    Mock::given(method("DELETE")).respond_with(ResponseTemplate::new(403)).expect(1).mount(&server).await;
    let blobs = Blobs::s3(homestead_integrations::http_client(), &server.uri(), "b", "auto", "id", "secret");
    let storage = Storage { blobs, max_bytes: 1024, link_secs: 300, retain_deleted_days: 0 };
    let store = Store::from_pool(pool.clone());
    let key = format!("att/{}", "ab".repeat(32));
    store.queue_unrecorded_purge(&key, 0).await.expect("queue");
    assert_eq!(purge_due(&store, &storage).await.expect("purge"), 0);
    assert_eq!(store.due_blob_purges(10).await.expect("due"), vec![key], "kept for the next pass");
}
