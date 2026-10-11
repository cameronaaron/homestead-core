//! Files attached to records: photos, vet records, receipts, manuals.
//!
//! One mechanism for every kind of record. What a file is attached to
//! decides who may see it: an attachment is exactly as private as its
//! owner, so a vet record needs `animals.medical` and a receipt
//! `purchases.read`.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::ValidationError;
use crate::auth::Capability;

string_enum!(
    /// The kind of record a file hangs off. Literals match the CHECK on
    /// `attachments.resource_type`.
    AttachmentOwner, "attached to" {
    Animal => "animal",
    AnimalCare => "animal_care",
    Purchase => "purchase",
    Gift => "gift",
    GrantCampaign => "grant_campaign",
    SeedLot => "seed_lot",
    Equipment => "equipment",
    EquipmentMaintenance => "equipment_maintenance",
    Incident => "incident",
    SoilTest => "soil_test",
    CropObservation => "crop_observation",
    Obligation => "obligation",
    EmergencyPlan => "emergency_plan",
});

impl AttachmentOwner {
    /// Needed to see or download the file.
    pub fn read_cap(self) -> Capability {
        match self {
            AttachmentOwner::Animal => Capability::AnimalsRead,
            AttachmentOwner::AnimalCare => Capability::AnimalsMedical,
            AttachmentOwner::Purchase => Capability::PurchasesRead,
            AttachmentOwner::Gift => Capability::GiftsRead,
            AttachmentOwner::GrantCampaign => Capability::GrantsRead,
            AttachmentOwner::SeedLot => Capability::GardenRead,
            AttachmentOwner::Equipment | AttachmentOwner::EquipmentMaintenance => Capability::EquipmentRead,
            AttachmentOwner::Incident => Capability::IncidentsRead,
            AttachmentOwner::SoilTest | AttachmentOwner::CropObservation => Capability::GardenRead,
            AttachmentOwner::Obligation | AttachmentOwner::EmergencyPlan => Capability::TasksRead,
        }
    }

    /// Needed to attach or remove a file.
    pub fn write_cap(self) -> Capability {
        match self {
            AttachmentOwner::Animal => Capability::AnimalsWrite,
            AttachmentOwner::AnimalCare => Capability::AnimalsMedical,
            AttachmentOwner::Purchase => Capability::PurchasesWrite,
            AttachmentOwner::Gift => Capability::GiftsWrite,
            AttachmentOwner::GrantCampaign => Capability::GrantsWrite,
            AttachmentOwner::SeedLot => Capability::GardenWrite,
            AttachmentOwner::Equipment | AttachmentOwner::EquipmentMaintenance => Capability::EquipmentWrite,
            AttachmentOwner::Incident => Capability::IncidentsWrite,
            AttachmentOwner::SoilTest | AttachmentOwner::CropObservation => Capability::GardenWrite,
            AttachmentOwner::Obligation | AttachmentOwner::EmergencyPlan => Capability::TasksWrite,
        }
    }
}

/// Largest file accepted unless configured otherwise: a phone photo or a
/// scanned multi-page receipt, not a video.
pub const DEFAULT_MAX_BYTES: usize = 10 * 1024 * 1024;

/// What the bytes are, judged from the bytes. The browser's declared type
/// is ignored: it's whatever the uploader's machine guessed, or claimed.
/// Only formats a browser renders without running code are accepted (no
/// SVG, HTML or office documents).
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    let starts = |p: &[u8]| bytes.starts_with(p);
    if starts(b"\xFF\xD8\xFF") {
        Some("image/jpeg")
    } else if starts(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" && matches!(&bytes[8..12], b"heic" | b"heix" | b"mif1") {
        Some("image/heic")
    } else if starts(b"%PDF-") {
        Some("application/pdf")
    } else {
        None
    }
}

/// Every media type [`sniff`] can return; the database CHECK matches.
pub const MEDIA_TYPES: &[&str] =
    &["image/jpeg", "image/png", "image/gif", "image/webp", "image/heic", "application/pdf"];

/// A filename safe to store and to put back in a download header: no path,
/// no control or quoting characters, at most 120 characters.
pub fn safe_filename(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if matches!(c, '"' | '\'' | ';' | '<' | '>' | '`' | '%') { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_start_matches('.').trim();
    let short: String = trimmed.chars().take(120).collect();
    if short.is_empty() { "file".to_owned() } else { short }
}

/// Plain ASCII stand-in for a filename, for the `filename=` parameter of
/// `Content-Disposition` (the UTF-8 name goes in `filename*`).
pub fn ascii_filename(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { '_' }).collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: Uuid,
    pub owner: AttachmentOwner,
    pub owner_id: Uuid,
    pub original_name: String,
    pub media_type: String,
    pub byte_size: i64,
    pub sha256: String,
    /// Where the bytes live in object storage. Never shown to people.
    #[serde(skip_serializing)]
    pub storage_key: String,
    pub created_by: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl Attachment {
    pub fn is_image(&self) -> bool {
        self.media_type.starts_with("image/")
    }
}

/// A file whose bytes are already safely stored under `storage_key`.
#[derive(Debug, Clone)]
pub struct NewAttachment {
    pub owner: AttachmentOwner,
    pub owner_id: Uuid,
    pub original_name: String,
    pub media_type: String,
    pub byte_size: i64,
    pub sha256: String,
    pub storage_key: String,
}

impl NewAttachment {
    pub fn validated(mut self) -> Result<Self, ValidationError> {
        self.original_name = safe_filename(&self.original_name);
        if !MEDIA_TYPES.contains(&self.media_type.as_str()) {
            return Err(ValidationError::Malformed("file type"));
        }
        if self.byte_size <= 0 {
            return Err(ValidationError::NotPositive("file size"));
        }
        let hex64 = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
        if !hex64(&self.sha256) {
            return Err(ValidationError::Malformed("sha256"));
        }
        if !self.storage_key.strip_prefix("att/").is_some_and(hex64) {
            return Err(ValidationError::Malformed("storage key"));
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_recognised_by_their_bytes_not_their_name() {
        assert_eq!(sniff(b"\xFF\xD8\xFF\xE0rest"), Some("image/jpeg"));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nIHDR"), Some("image/png"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(b"\0\0\0\x18ftypheic\0\0"), Some("image/heic"));
        assert_eq!(sniff(b"%PDF-1.7\n"), Some("application/pdf"));
        for bad in [&b"<svg onload=alert(1)>"[..], b"<!doctype html>", b"PK\x03\x04docx", b"", b"RIFF\0\0\0\0WAVE"] {
            assert_eq!(sniff(bad), None, "{bad:?}");
        }
        for t in [sniff(b"\xFF\xD8\xFF"), sniff(b"GIF89a")] {
            assert!(MEDIA_TYPES.contains(&t.expect("known")));
        }
    }

    #[test]
    fn filenames_lose_paths_and_header_breaking_characters() {
        assert_eq!(safe_filename("../../etc/passwd"), "passwd");
        assert_eq!(safe_filename("C:\\Users\\kiki\\receipt.pdf"), "receipt.pdf");
        assert_eq!(safe_filename("a\"b;c\r\nd.jpg"), "a_b_cd.jpg");
        assert_eq!(safe_filename(".htaccess"), "htaccess");
        assert_eq!(safe_filename("   "), "file");
        assert_eq!(safe_filename(&"x".repeat(500)).chars().count(), 120);
        assert_eq!(ascii_filename("Mochi – vet.pdf"), "Mochi _ vet.pdf");
    }

    #[test]
    fn metadata_must_describe_a_stored_object() {
        let ok = NewAttachment {
            owner: AttachmentOwner::Animal,
            owner_id: Uuid::nil(),
            original_name: "cat.jpg".into(),
            media_type: "image/jpeg".into(),
            byte_size: 10,
            sha256: "a".repeat(64),
            storage_key: format!("att/{}", "b".repeat(64)),
        };
        assert!(ok.clone().validated().is_ok());
        assert!(NewAttachment { media_type: "image/svg+xml".into(), ..ok.clone() }.validated().is_err());
        assert!(NewAttachment { byte_size: 0, ..ok.clone() }.validated().is_err());
        assert!(NewAttachment { sha256: "A".repeat(64), ..ok.clone() }.validated().is_err());
        assert!(NewAttachment { storage_key: "animals/1.jpg".into(), ..ok.clone() }.validated().is_err());
    }

    #[test]
    fn literals_match_the_migration() {
        // The newest migration that (re)defines the owner list is the truth.
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../migrations");
        let mut files: Vec<_> = std::fs::read_dir(dir).expect("migrations").map(|e| e.expect("entry").path()).collect();
        files.sort();
        let latest = files
            .iter()
            .rev()
            .map(|f| std::fs::read_to_string(f).expect("read"))
            .find(|sql| sql.contains("add constraint attachments_resource_type_check"))
            .expect("some migration defines the owner list");
        let owners = &latest[latest.find("add constraint attachments_resource_type_check").expect("check")..];
        crate::tests::assert_check_list(owners, "resource_type", AttachmentOwner::ALL, AttachmentOwner::as_str);
        let sql = include_str!("../../../migrations/0012_attachments.sql");
        crate::tests::assert_check_list(sql, "media_type", MEDIA_TYPES, |s| s);
    }

    /// A vet record is as private as the vet history it belongs to.
    #[test]
    fn owners_share_their_records_privacy() {
        assert_eq!(AttachmentOwner::AnimalCare.read_cap(), Capability::AnimalsMedical);
        for o in AttachmentOwner::ALL {
            let (r, w) = (o.read_cap().as_str(), o.write_cap().as_str());
            assert_eq!(r.split('.').next(), w.split('.').next(), "{o}: read and write are the same domain");
        }
    }
}
