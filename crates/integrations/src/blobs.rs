//! Private object storage for attachment bytes.
//!
//! - `S3` speaks the S3 API (Cloudflare R2 in production) with AWS
//!   Signature V4. Uploads are signed over the body's SHA-256, so the
//!   storage service rejects bytes that changed in transit. Downloads are
//!   short-lived presigned GET URLs: the bucket stays private and the app
//!   hands out a five-minute link only after checking who's asking.
//! - `Filesystem` keeps bytes on local disk (development; a Cloudflare
//!   container's disk does not survive a restart).
//! - `Memory` is for tests.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

/// How a download is served.
#[derive(Debug, PartialEq, Eq)]
pub enum Download {
    /// Send the browser here (a presigned, expiring URL).
    Redirect(String),
    /// Serve these bytes directly.
    Bytes(Vec<u8>),
}

#[derive(Clone)]
pub struct S3 {
    client: reqwest::Client,
    /// `https://<account>.r2.cloudflarestorage.com`, or a MinIO URL.
    endpoint: String,
    bucket: String,
    region: String,
    key_id: String,
    secret: String,
}

#[derive(Clone)]
pub enum Blobs {
    S3(S3),
    Filesystem(PathBuf),
    Memory(Arc<Mutex<HashMap<String, Vec<u8>>>>),
}

/// What to store with the object so any later download is safe on its own.
pub struct Headers<'a> {
    pub content_type: &'a str,
    pub content_disposition: &'a str,
}

impl Blobs {
    pub fn s3(client: reqwest::Client, endpoint: &str, bucket: &str, region: &str, key_id: &str, secret: &str) -> Self {
        Blobs::S3(S3 {
            client,
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            bucket: bucket.to_owned(),
            region: region.to_owned(),
            key_id: key_id.to_owned(),
            secret: secret.to_owned(),
        })
    }

    pub fn memory() -> Self {
        Blobs::Memory(Arc::default())
    }

    /// Stores `bytes` under `key`. `sha256` is the hex digest of `bytes`.
    pub async fn put(&self, key: &str, bytes: &[u8], sha256: &str, h: Headers<'_>) -> Result<(), String> {
        match self {
            Blobs::S3(s) => s.put(key, bytes, sha256, h).await,
            Blobs::Filesystem(root) => {
                let path = root.join(key);
                if let Some(dir) = path.parent() {
                    tokio::fs::create_dir_all(dir).await.map_err(|e| format!("creating {}: {e}", dir.display()))?;
                }
                tokio::fs::write(&path, bytes).await.map_err(|e| format!("writing {}: {e}", path.display()))
            }
            Blobs::Memory(m) => {
                m.lock().map_err(|_| "poisoned".to_owned())?.insert(key.to_owned(), bytes.to_vec());
                Ok(())
            }
        }
    }

    pub async fn download(&self, key: &str, valid_secs: u32) -> Result<Download, String> {
        match self {
            Blobs::S3(s) => Ok(Download::Redirect(s.presigned_get(key, valid_secs, OffsetDateTime::now_utc()))),
            Blobs::Filesystem(root) => {
                let path = root.join(key);
                tokio::fs::read(&path)
                    .await
                    .map(Download::Bytes)
                    .map_err(|e| format!("reading {}: {e}", path.display()))
            }
            Blobs::Memory(m) => m
                .lock()
                .map_err(|_| "poisoned".to_owned())?
                .get(key)
                .cloned()
                .map(Download::Bytes)
                .ok_or_else(|| format!("{key} not found")),
        }
    }

    pub async fn delete(&self, key: &str) -> Result<(), String> {
        match self {
            Blobs::S3(s) => s.delete(key).await,
            Blobs::Filesystem(root) => match tokio::fs::remove_file(root.join(key)).await {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
                _ => Ok(()),
            },
            Blobs::Memory(m) => {
                m.lock().map_err(|_| "poisoned".to_owned())?.remove(key);
                Ok(())
            }
        }
    }
}

impl S3 {
    fn host(&self) -> &str {
        self.endpoint.split_once("://").map_or(self.endpoint.as_str(), |(_, h)| h)
    }

    /// Path-style: `/<bucket>/<key>` (R2 and MinIO both accept it).
    fn path(&self, key: &str) -> String {
        format!("/{}/{}", uri_encode(&self.bucket, false), uri_encode(key, true))
    }

    /// Stores `bytes` under `key`, signed over their SHA-256 so the service
    /// refuses bytes that changed in transit.
    pub async fn put(&self, key: &str, bytes: &[u8], sha256: &str, h: Headers<'_>) -> Result<(), String> {
        let now = OffsetDateTime::now_utc();
        let path = self.path(key);
        let headers = vec![
            ("content-disposition", h.content_disposition.to_owned()),
            ("content-type", h.content_type.to_owned()),
            ("host", self.host().to_owned()),
            ("x-amz-content-sha256", sha256.to_owned()),
            ("x-amz-date", amz_date(now)),
        ];
        let auth = sign_request(&SigningInput {
            method: "PUT",
            path: &path,
            query: &[],
            headers: &headers,
            payload_hash: sha256,
            now,
            region: &self.region,
            key_id: &self.key_id,
            secret: &self.secret,
        });
        let mut req = self.client.put(format!("{}{path}", self.endpoint)).body(bytes.to_vec());
        for (k, v) in headers.iter().filter(|(k, _)| *k != "host") {
            req = req.header(*k, v);
        }
        let resp = req.header("authorization", auth).send().await.map_err(|e| format!("object storage: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("object storage refused the upload: HTTP {}", resp.status()))
        }
    }

    pub async fn delete(&self, key: &str) -> Result<(), String> {
        let now = OffsetDateTime::now_utc();
        let path = self.path(key);
        let empty = hex::encode(Sha256::digest(b""));
        let headers = vec![
            ("host", self.host().to_owned()),
            ("x-amz-content-sha256", empty.clone()),
            ("x-amz-date", amz_date(now)),
        ];
        let auth = sign_request(&SigningInput {
            method: "DELETE",
            path: &path,
            query: &[],
            headers: &headers,
            payload_hash: &empty,
            now,
            region: &self.region,
            key_id: &self.key_id,
            secret: &self.secret,
        });
        let mut req = self.client.delete(format!("{}{path}", self.endpoint));
        for (k, v) in headers.iter().filter(|(k, _)| *k != "host") {
            req = req.header(*k, v);
        }
        let resp = req.header("authorization", auth).send().await.map_err(|e| format!("object storage: {e}"))?;
        if resp.status().is_success() || resp.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(format!("object storage refused the delete: HTTP {}", resp.status()))
        }
    }

    /// A header-signed request with an empty body.
    fn signed(&self, method: reqwest::Method, path: &str, query: &[(String, String)]) -> reqwest::RequestBuilder {
        let now = OffsetDateTime::now_utc();
        let empty = hex::encode(Sha256::digest(b""));
        let headers = vec![
            ("host", self.host().to_owned()),
            ("x-amz-content-sha256", empty.clone()),
            ("x-amz-date", amz_date(now)),
        ];
        let auth = sign_request(&SigningInput {
            method: method.as_str(),
            path,
            query,
            headers: &headers,
            payload_hash: &empty,
            now,
            region: &self.region,
            key_id: &self.key_id,
            secret: &self.secret,
        });
        let url = if query.is_empty() {
            format!("{}{path}", self.endpoint)
        } else {
            format!("{}{path}?{}", self.endpoint, canonical_query(query))
        };
        let mut req = self.client.request(method, url);
        for (k, v) in headers.iter().filter(|(k, _)| *k != "host") {
            req = req.header(*k, v);
        }
        req.header("authorization", auth)
    }

    /// An object's bytes and stored headers, or `None` if there is none.
    pub async fn get(&self, key: &str) -> Result<Option<Object>, String> {
        let resp = self
            .signed(reqwest::Method::GET, &self.path(key), &[])
            .send()
            .await
            .map_err(|e| format!("object storage: {e}"))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(format!("object storage refused reading {key}: HTTP {}", resp.status()));
        }
        let header = |name: &str| resp.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
        let (content_type, content_disposition) = (header("content-type"), header("content-disposition"));
        let bytes = resp.bytes().await.map_err(|e| format!("object storage: {e}"))?.to_vec();
        Ok(Some(Object { bytes, content_type, content_disposition }))
    }

    /// Every key under `prefix` (ListObjectsV2, following continuation).
    pub async fn list(&self, prefix: &str) -> Result<Vec<String>, String> {
        let path = format!("/{}", uri_encode(&self.bucket, false));
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut query = vec![("list-type".to_owned(), "2".to_owned()), ("prefix".to_owned(), prefix.to_owned())];
            if let Some(t) = &token {
                query.push(("continuation-token".to_owned(), t.clone()));
            }
            let resp = self
                .signed(reqwest::Method::GET, &path, &query)
                .send()
                .await
                .map_err(|e| format!("object storage: {e}"))?;
            let status = resp.status();
            let xml = resp.text().await.map_err(|e| format!("object storage: {e}"))?;
            if !status.is_success() {
                return Err(format!("object storage refused listing: HTTP {status}"));
            }
            let page = parse_list(&xml);
            keys.extend(page.keys);
            match page.next {
                Some(t) => token = Some(t),
                None => return Ok(keys),
            }
        }
    }

    pub fn presigned_get(&self, key: &str, valid_secs: u32, now: OffsetDateTime) -> String {
        let path = self.path(key);
        let query = presign_query(&PresignInput {
            method: "GET",
            host: self.host(),
            path: &path,
            valid_secs,
            now,
            region: &self.region,
            key_id: &self.key_id,
            secret: &self.secret,
        });
        format!("{}{path}?{query}", self.endpoint)
    }
}

/// An object read back from storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
    pub content_disposition: Option<String>,
}

struct ListPage {
    keys: Vec<String>,
    /// The continuation token, when the listing is truncated.
    next: Option<String>,
}

/// The parts of a ListObjectsV2 response we use. Keys we write are hex
/// under `att/`, but any key is unescaped so none is misread.
fn parse_list(xml: &str) -> ListPage {
    fn all<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
        let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
        let mut out = Vec::new();
        let mut rest = xml;
        while let Some(i) = rest.find(&open) {
            let after = &rest[i + open.len()..];
            let Some(j) = after.find(&close) else { break };
            out.push(&after[..j]);
            rest = &after[j + close.len()..];
        }
        out
    }
    let unescape = |s: &str| {
        s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
    };
    let truncated = all(xml, "IsTruncated").first().is_some_and(|v| *v == "true");
    ListPage {
        keys: all(xml, "Key").into_iter().map(unescape).collect(),
        next: if truncated { all(xml, "NextContinuationToken").first().map(|t| unescape(t)) } else { None },
    }
}

// ── AWS Signature Version 4 ────────────────────────────────────────────────

fn amz_date(t: OffsetDateTime) -> String {
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// SigV4 URI encoding: everything but unreserved characters, and `/` too
/// unless it separates path segments.
fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn hmac(key: &[u8], msg: &str) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    m.update(msg.as_bytes());
    m.finalize().into_bytes().to_vec()
}

fn scope(now: OffsetDateTime, region: &str) -> String {
    format!("{}/{region}/s3/aws4_request", &amz_date(now)[..8])
}

fn signature(canonical_request: &str, now: OffsetDateTime, region: &str, secret: &str) -> String {
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        amz_date(now),
        scope(now, region),
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), &amz_date(now)[..8]);
    let k_region = hmac(&k_date, region);
    let k_service = hmac(&k_region, "s3");
    let k_signing = hmac(&k_service, "aws4_request");
    hex::encode(hmac(&k_signing, &to_sign))
}

fn canonical_query(query: &[(String, String)]) -> String {
    let mut q: Vec<(String, String)> =
        query.iter().map(|(k, v)| (uri_encode(k, false), uri_encode(v, false))).collect();
    q.sort();
    q.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
}

struct SigningInput<'a> {
    method: &'a str,
    /// Already URI-encoded.
    path: &'a str,
    query: &'a [(String, String)],
    /// Lower-case names; must include `host`.
    headers: &'a [(&'a str, String)],
    payload_hash: &'a str,
    now: OffsetDateTime,
    region: &'a str,
    key_id: &'a str,
    secret: &'a str,
}

/// The `Authorization` header for a header-signed request.
fn sign_request(i: &SigningInput<'_>) -> String {
    let mut headers: Vec<(&str, &str)> = i.headers.iter().map(|(k, v)| (*k, v.trim())).collect();
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed: String = headers.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(";");
    let canonical = format!(
        "{}\n{}\n{}\n{canonical_headers}\n{signed}\n{}",
        i.method,
        i.path,
        canonical_query(i.query),
        i.payload_hash
    );
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{},SignedHeaders={signed},Signature={}",
        i.key_id,
        scope(i.now, i.region),
        signature(&canonical, i.now, i.region, i.secret)
    )
}

struct PresignInput<'a> {
    method: &'a str,
    host: &'a str,
    path: &'a str,
    valid_secs: u32,
    now: OffsetDateTime,
    region: &'a str,
    key_id: &'a str,
    secret: &'a str,
}

/// The query string of a presigned URL (signed over `host` only).
fn presign_query(i: &PresignInput<'_>) -> String {
    let query = vec![
        ("X-Amz-Algorithm".to_owned(), "AWS4-HMAC-SHA256".to_owned()),
        ("X-Amz-Credential".to_owned(), format!("{}/{}", i.key_id, scope(i.now, i.region))),
        ("X-Amz-Date".to_owned(), amz_date(i.now)),
        ("X-Amz-Expires".to_owned(), i.valid_secs.to_string()),
        ("X-Amz-SignedHeaders".to_owned(), "host".to_owned()),
    ];
    let canonical =
        format!("{}\n{}\n{}\nhost:{}\n\nhost\nUNSIGNED-PAYLOAD", i.method, i.path, canonical_query(&query), i.host);
    format!("{}&X-Amz-Signature={}", canonical_query(&query), signature(&canonical, i.now, i.region, i.secret))
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    // The worked examples in AWS's "Signature Calculations for the
    // Authorization Header" and "Query String Authentication" S3 docs.
    const KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn header_signature_matches_the_aws_get_object_example() {
        let now = datetime!(2013-05-24 00:00:00 UTC);
        let auth = sign_request(&SigningInput {
            method: "GET",
            path: "/test.txt",
            query: &[],
            headers: &[
                ("host", "examplebucket.s3.amazonaws.com".into()),
                ("range", "bytes=0-9".into()),
                ("x-amz-content-sha256", EMPTY.into()),
                ("x-amz-date", "20130524T000000Z".into()),
            ],
            payload_hash: EMPTY,
            now,
            region: "us-east-1",
            key_id: KEY_ID,
            secret: SECRET,
        });
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,\
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date,\
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn presigned_url_matches_the_aws_query_string_example() {
        let q = presign_query(&PresignInput {
            method: "GET",
            host: "examplebucket.s3.amazonaws.com",
            path: "/test.txt",
            valid_secs: 86400,
            now: datetime!(2013-05-24 00:00:00 UTC),
            region: "us-east-1",
            key_id: KEY_ID,
            secret: SECRET,
        });
        assert!(q.ends_with("X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"), "{q}");
        assert!(q.contains("X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request"));
    }

    #[test]
    fn keys_and_buckets_are_encoded_like_sigv4_expects() {
        assert_eq!(uri_encode("att/ab cd", true), "att/ab%20cd");
        assert_eq!(uri_encode("a/b", false), "a%2Fb");
        assert_eq!(amz_date(datetime!(2026-01-02 03:04:05 UTC)), "20260102T030405Z");
    }

    #[test]
    fn listings_are_parsed_with_continuation_and_escapes() {
        let page = parse_list(
            "<?xml version=\"1.0\"?><ListBucketResult><Name>b</Name><IsTruncated>true</IsTruncated>\
             <Contents><Key>att/aa</Key><Size>3</Size></Contents>\
             <Contents><Key>odd &amp; key</Key></Contents>\
             <NextContinuationToken>tok&amp;en==</NextContinuationToken></ListBucketResult>",
        );
        assert_eq!(page.keys, ["att/aa", "odd & key"]);
        assert_eq!(page.next.as_deref(), Some("tok&en=="));
        let last = parse_list("<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>");
        assert!(last.keys.is_empty() && last.next.is_none());
    }

    #[tokio::test]
    async fn memory_and_disk_round_trip_and_delete() {
        let dir = std::env::temp_dir().join(format!("blobs-{}", uuid::Uuid::new_v4()));
        for blobs in [Blobs::memory(), Blobs::Filesystem(dir.clone())] {
            let h = Headers { content_type: "image/png", content_disposition: "inline" };
            blobs.put("att/k", b"bytes", "x", h).await.expect("put");
            assert_eq!(blobs.download("att/k", 60).await.expect("get"), Download::Bytes(b"bytes".to_vec()));
            blobs.delete("att/k").await.expect("delete");
            blobs.delete("att/k").await.expect("deleting twice is fine");
            assert!(blobs.download("att/k", 60).await.is_err());
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
