//! A real round trip against S3-compatible storage (MinIO locally, or an
//! R2 bucket). Ignored by default; run with the bucket already created:
//!
//! ```sh
//! S3_TEST_ENDPOINT=http://localhost:9000 S3_TEST_BUCKET=homestead-test \
//! S3_TEST_KEY_ID=… S3_TEST_SECRET=… cargo test -p homestead-integrations --test s3_live -- --ignored
//! ```

use homestead_integrations::blobs::{Blobs, Download, Headers};
use sha2::{Digest, Sha256};

#[tokio::test]
#[ignore = "needs a live S3-compatible endpoint"]
async fn put_presigned_get_and_delete_against_a_real_bucket() {
    let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k} is required"));
    let region = std::env::var("S3_TEST_REGION").unwrap_or_else(|_| "auto".into());
    let client = reqwest::Client::new();
    let blobs = Blobs::s3(
        client.clone(),
        &var("S3_TEST_ENDPOINT"),
        &var("S3_TEST_BUCKET"),
        &region,
        &var("S3_TEST_KEY_ID"),
        &var("S3_TEST_SECRET"),
    );
    let key = format!("att/{}", hex::encode(Sha256::digest(uuid::Uuid::new_v4().as_bytes())));
    let body = b"%PDF-1.7 live round trip".to_vec();
    let sha = hex::encode(Sha256::digest(&body));

    // A body that doesn't match its signed hash must be refused. (Some
    // local fakes, like `rclone serve s3`, don't check; set
    // S3_TEST_NO_HASH_CHECK=1 for those.)
    if std::env::var("S3_TEST_NO_HASH_CHECK").is_err() {
        let wrong = hex::encode(Sha256::digest(b"something else"));
        let h = Headers { content_type: "application/pdf", content_disposition: "attachment; filename=\"x.pdf\"" };
        assert!(blobs.put(&key, &body, &wrong, h).await.is_err(), "storage accepted bytes that don't match their hash");
    }

    // A wrong secret must be refused: proves the server checks signatures.
    let forged = Blobs::s3(
        client.clone(),
        &var("S3_TEST_ENDPOINT"),
        &var("S3_TEST_BUCKET"),
        &region,
        &var("S3_TEST_KEY_ID"),
        "wrong",
    );
    let h = Headers { content_type: "application/pdf", content_disposition: "attachment" };
    assert!(forged.put(&key, &body, &sha, h).await.is_err(), "storage accepted a forged signature");

    let h = Headers { content_type: "application/pdf", content_disposition: "attachment; filename=\"x.pdf\"" };
    blobs.put(&key, &body, &sha, h).await.expect("put");
    let Download::Redirect(url) = blobs.download(&key, 60).await.expect("presign") else { panic!("expected a link") };
    let resp = client.get(&url).send().await.expect("get");
    assert!(resp.status().is_success(), "presigned GET: {}", resp.status());
    assert_eq!(resp.headers()["content-type"], "application/pdf");
    assert_eq!(resp.headers()["content-disposition"], "attachment; filename=\"x.pdf\"");
    assert_eq!(resp.bytes().await.expect("bytes").as_ref(), body.as_slice());

    // Without the signature the bucket is closed.
    let bare = url.split('?').next().expect("url");
    assert!(!client.get(bare).send().await.expect("get").status().is_success(), "bucket is publicly readable");

    blobs.delete(&key).await.expect("delete");
    assert!(!client.get(&url).send().await.expect("get").status().is_success(), "still there after delete");
}
