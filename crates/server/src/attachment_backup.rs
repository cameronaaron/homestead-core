//! Copies attachment bytes to a second bucket and checks both against the
//! SHA-256 the database recorded (`homestead attachments-backup`).
//!
//! R2 has no object versioning, so this second bucket is the recovery
//! copy: written only by this job, with its own credentials (the app never
//! holds them), and under its own bucket lock. Keys are never reused
//! (`att/` + 256 random bits), so "copy what's missing" is a complete
//! backup, and an object already copied never needs copying again.
//!
//! Pruning (dropping backup copies of files that were deleted and purged)
//! is what makes a deletion eventually real in the backup too. It refuses
//! to run when it would remove more than a small share of the backup: an
//! empty or restored-too-far database looks exactly like "everything was
//! deleted", and the job must not follow it.

use std::collections::BTreeSet;
use std::future::Future;

use homestead_integrations::blobs::{Headers, Object, S3};
use sha2::{Digest, Sha256};

/// The object-storage operations the job needs.
pub trait Objects {
    fn list(&self, prefix: &str) -> impl Future<Output = Result<Vec<String>, String>> + Send;
    fn get(&self, key: &str) -> impl Future<Output = Result<Option<Object>, String>> + Send;
    fn put(&self, key: &str, object: &Object, sha256: &str) -> impl Future<Output = Result<(), String>> + Send;
    fn delete(&self, key: &str) -> impl Future<Output = Result<(), String>> + Send;
}

impl Objects for S3 {
    async fn list(&self, prefix: &str) -> Result<Vec<String>, String> {
        S3::list(self, prefix).await
    }
    async fn get(&self, key: &str) -> Result<Option<Object>, String> {
        S3::get(self, key).await
    }
    async fn put(&self, key: &str, o: &Object, sha256: &str) -> Result<(), String> {
        let h = Headers {
            content_type: o.content_type.as_deref().unwrap_or("application/octet-stream"),
            content_disposition: o.content_disposition.as_deref().unwrap_or("attachment"),
        };
        S3::put(self, key, &o.bytes, sha256, h).await
    }
    async fn delete(&self, key: &str) -> Result<(), String> {
        S3::delete(self, key).await
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Also read back every copy (both buckets) and check its hash.
    pub verify: bool,
    /// Drop backup copies of files the database no longer accounts for.
    pub prune: bool,
    /// Copy from the backup into primary instead, for keys primary lacks.
    pub restore: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub accounted: usize,
    pub copied: usize,
    pub restored: usize,
    pub verified: usize,
    pub pruned: usize,
    /// Backup copies a prune couldn't delete yet (the backup bucket lock).
    pub held_by_lock: usize,
    /// In the database, in neither bucket: gone.
    pub lost: Vec<String>,
    /// Bytes that don't match the recorded SHA-256, as `bucket: key`.
    pub corrupt: Vec<String>,
    /// In the database and the backup, not in primary (`--restore` fixes).
    pub missing_from_primary: Vec<String>,
    /// In primary, not in the database: an upload whose record never landed.
    pub orphans: Vec<String>,
    /// Why pruning was skipped, if it was.
    pub prune_skipped: Option<String>,
}

impl Report {
    /// Anything that needs a person: lost or corrupt bytes, or files only
    /// the backup still has.
    pub fn healthy(&self) -> bool {
        self.lost.is_empty() && self.corrupt.is_empty() && self.missing_from_primary.is_empty()
    }
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The most copies one prune may remove: ten, or a tenth of the backup.
fn prune_limit(backup_size: usize) -> usize {
    (backup_size / 10).max(10)
}

/// Runs one pass. `accounted` is every key the database knows (live and
/// awaiting purge) with its recorded SHA-256; `None` for bytes that were
/// never recorded, which are neither copied nor reported lost.
pub async fn run(
    accounted: &[(String, Option<String>)],
    primary: &impl Objects,
    backup: &impl Objects,
    opts: Options,
) -> Result<Report, String> {
    let mut r = Report { accounted: accounted.len(), ..Report::default() };
    let in_primary: BTreeSet<String> = primary.list("att/").await?.into_iter().collect();
    let in_backup: BTreeSet<String> = backup.list("att/").await?.into_iter().collect();
    let known: BTreeSet<&str> = accounted.iter().map(|(k, _)| k.as_str()).collect();

    for (key, sha) in accounted {
        let Some(sha) = sha else { continue };
        let (p, b) = (in_primary.contains(key), in_backup.contains(key));
        match (p, b) {
            (false, false) => r.lost.push(key.clone()),
            (true, false) => match primary.get(key).await? {
                Some(o) if hash(&o.bytes) == *sha => {
                    backup.put(key, &o, sha).await?;
                    r.copied += 1;
                }
                Some(_) => r.corrupt.push(format!("primary: {key}")),
                None => r.lost.push(key.clone()),
            },
            (false, true) if opts.restore => match backup.get(key).await? {
                Some(o) if hash(&o.bytes) == *sha => {
                    primary.put(key, &o, sha).await?;
                    r.restored += 1;
                }
                Some(_) => r.corrupt.push(format!("backup: {key}")),
                None => r.missing_from_primary.push(key.clone()),
            },
            (false, true) => r.missing_from_primary.push(key.clone()),
            (true, true) if opts.verify => {
                let copies = [("primary", primary.get(key).await?), ("backup", backup.get(key).await?)];
                for (name, got) in copies {
                    if got.is_none_or(|o| hash(&o.bytes) != *sha) {
                        r.corrupt.push(format!("{name}: {key}"));
                    }
                }
                r.verified += 1;
            }
            (true, true) => {}
        }
    }
    r.orphans = in_primary.iter().filter(|k| !known.contains(k.as_str())).cloned().collect();

    if opts.prune {
        let stale: Vec<&String> = in_backup.iter().filter(|k| !known.contains(k.as_str())).collect();
        let limit = prune_limit(in_backup.len());
        if accounted.is_empty() && !in_backup.is_empty() {
            r.prune_skipped = Some("the database accounts for no files at all".into());
        } else if stale.len() > limit {
            r.prune_skipped = Some(format!(
                "{} backup copies aren't in the database, more than the {limit} one prune may remove; \
                 check the database is the right one, then prune by hand",
                stale.len()
            ));
        } else {
            for key in stale {
                match backup.delete(key).await {
                    Ok(()) => r.pruned += 1,
                    Err(_) => r.held_by_lock += 1,
                }
            }
        }
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// A bucket. Keys in `locked` refuse deletion, like an R2 bucket lock.
    #[derive(Default)]
    struct Fake {
        objects: Mutex<BTreeMap<String, Vec<u8>>>,
        locked: BTreeSet<String>,
    }

    impl Fake {
        fn with(items: &[(&str, &[u8])]) -> Self {
            let f = Fake::default();
            for (k, v) in items {
                f.objects.lock().expect("lock").insert((*k).to_owned(), v.to_vec());
            }
            f
        }
        fn keys(&self) -> Vec<String> {
            self.objects.lock().expect("lock").keys().cloned().collect()
        }
    }

    impl Objects for Fake {
        async fn list(&self, prefix: &str) -> Result<Vec<String>, String> {
            Ok(self.keys().into_iter().filter(|k| k.starts_with(prefix)).collect())
        }
        async fn get(&self, key: &str) -> Result<Option<Object>, String> {
            let bytes = self.objects.lock().expect("lock").get(key).cloned();
            Ok(bytes.map(|bytes| Object { bytes, content_type: None, content_disposition: None }))
        }
        async fn put(&self, key: &str, o: &Object, sha256: &str) -> Result<(), String> {
            // Like R2 with a signed payload hash: refuses bytes that don't match.
            if hash(&o.bytes) != sha256 {
                return Err("hash mismatch".into());
            }
            self.objects.lock().expect("lock").insert(key.to_owned(), o.bytes.clone());
            Ok(())
        }
        async fn delete(&self, key: &str) -> Result<(), String> {
            if self.locked.contains(key) {
                return Err("HTTP 403".into());
            }
            self.objects.lock().expect("lock").remove(key);
            Ok(())
        }
    }

    fn rec(key: &str, bytes: &[u8]) -> (String, Option<String>) {
        (key.to_owned(), Some(hash(bytes)))
    }

    #[tokio::test]
    async fn copies_what_the_backup_lacks_once_and_reports_loss_and_orphans() {
        let primary = Fake::with(&[("att/a", b"cat"), ("att/b", b"vet"), ("att/orphan", b"?")]);
        let backup = Fake::default();
        let db = [rec("att/a", b"cat"), rec("att/b", b"vet"), rec("att/gone", b"lost")];
        let r = run(&db, &primary, &backup, Options::default()).await.expect("run");
        assert_eq!(backup.keys(), ["att/a", "att/b"]);
        assert_eq!(
            (r.copied, r.lost.clone(), r.orphans.clone()),
            (2, vec!["att/gone".into()], vec!["att/orphan".into()])
        );
        assert!(!r.healthy(), "a lost file needs a person");

        let again = run(&db, &primary, &backup, Options::default()).await.expect("run");
        assert_eq!(again.copied, 0, "already-copied keys are never re-read");
    }

    #[tokio::test]
    async fn bytes_that_dont_match_the_database_are_never_copied() {
        let primary = Fake::with(&[("att/a", b"tampered")]);
        let backup = Fake::default();
        let r = run(&[rec("att/a", b"cat")], &primary, &backup, Options::default()).await.expect("run");
        assert_eq!((r.copied, r.corrupt.clone()), (0, vec!["primary: att/a".into()]));
        assert!(backup.keys().is_empty(), "a corrupt copy would overwrite nothing but hide the loss");
    }

    #[tokio::test]
    async fn verify_reads_both_copies_and_restore_refills_primary() {
        let primary = Fake::with(&[("att/a", b"cat")]);
        let backup = Fake::with(&[("att/a", b"bit rot"), ("att/b", b"vet")]);
        let db = [rec("att/a", b"cat"), rec("att/b", b"vet")];
        let r = run(&db, &primary, &backup, Options { verify: true, ..Options::default() }).await.expect("run");
        assert_eq!(r.corrupt, ["backup: att/a"]);
        assert_eq!(r.missing_from_primary, ["att/b"]);

        let r = run(&db, &primary, &backup, Options { restore: true, ..Options::default() }).await.expect("run");
        assert_eq!(r.restored, 1);
        assert_eq!(primary.keys(), ["att/a", "att/b"]);
    }

    #[tokio::test]
    async fn prune_drops_purged_copies_but_not_locked_ones() {
        let primary = Fake::with(&[("att/a", b"cat")]);
        let mut backup = Fake::with(&[("att/a", b"cat"), ("att/old", b"x"), ("att/young", b"y")]);
        backup.locked.insert("att/young".into());
        let r = run(&[rec("att/a", b"cat")], &primary, &backup, Options { prune: true, ..Options::default() })
            .await
            .expect("run");
        assert_eq!((r.pruned, r.held_by_lock), (1, 1));
        assert_eq!(backup.keys(), ["att/a", "att/young"]);
    }

    /// An empty or wrongly restored database looks like "everything was
    /// deleted". Pruning must not follow it.
    #[tokio::test]
    async fn prune_refuses_to_follow_a_database_that_lost_most_files() {
        let items: Vec<(String, Vec<u8>)> = (0..30).map(|i| (format!("att/{i:02}"), vec![i])).collect();
        let refs: Vec<(&str, &[u8])> = items.iter().map(|(k, v)| (k.as_str(), v.as_slice())).collect();
        let primary = Fake::with(&refs);
        let backup = Fake::with(&refs);
        let prune = Options { prune: true, ..Options::default() };

        let r = run(&[], &primary, &backup, prune).await.expect("run");
        assert!(r.prune_skipped.is_some() && r.pruned == 0, "{r:?}");
        let db: Vec<_> = items[..5].iter().map(|(k, v)| rec(k, v)).collect();
        let r = run(&db, &primary, &backup, prune).await.expect("run");
        assert!(r.prune_skipped.is_some() && r.pruned == 0, "25 of 30 is too many: {r:?}");
        assert_eq!(backup.keys().len(), 30);
    }
}
