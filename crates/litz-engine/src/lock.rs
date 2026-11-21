//! `litz.lock`: which catalog entries and firmware bytes produced a result.
//!
//! This is the reproducibility boundary from `docs/formats.md` §5; the caller
//! owns when to write, when to refuse and what `--update-lock` means.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

/// The schema version this build understands and writes.
const LOCK_VERSION: u32 = 1;

/// One `[[entry]]`: a catalog manifest a run loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    /// The catalog key (`part.id` / `board.id`), written as `type`.
    #[serde(rename = "type")]
    pub type_key: String,
    pub version: String,
    /// SHA-256 of the manifest text (see [`Lock::from_run`]).
    pub sha256: String,
}

/// The `[firmware]` table: the image the run booted, plus what it flashed
/// alongside it when the project supplied those.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Firmware {
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rom_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootloader_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_table_sha256: Option<String>,
}

/// Everything a run consumed that can move between one green CI run and the
/// next: the catalog manifests and the firmware bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lock {
    /// Always [`LOCK_VERSION`]; private so every `Lock` is one this module
    /// knows how to write.
    #[serde(default = "version_one")]
    version: u32,
    /// Written as `[[entry]]`, one table per manifest.
    #[serde(rename = "entry", default)]
    pub entries: Vec<LockEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware: Option<Firmware>,
}

fn version_one() -> u32 {
    LOCK_VERSION
}

impl Lock {
    /// Build the lock from what a run loaded: `catalog` holds one
    /// `(type_key, version, manifest_text)` per entry; the remaining arguments
    /// are the bytes it flashed (application image, mask ROM, bootloader,
    /// partition table), `None` when absent.
    ///
    /// The manifest *text* is hashed, not its path: text is what behaviour
    /// derives from, while a path is a checkout location that would make the
    /// lock machine-specific. A builtin `include_str!` manifest and a
    /// directory manifest with identical bytes therefore hash identically.
    ///
    /// Entries are sorted by catalog id so the file is canonical.
    pub fn from_run(
        catalog: &[(String, String, String)],
        firmware: Option<&[u8]>,
        rom: Option<&[u8]>,
        bootloader: Option<&[u8]>,
        partition_table: Option<&[u8]>,
    ) -> Lock {
        let mut entries: Vec<LockEntry> = catalog
            .iter()
            .map(|(key, version, text)| LockEntry {
                type_key: key.clone(),
                version: version.clone(),
                sha256: sha256_hex(text.as_bytes()),
            })
            .collect();
        entries.sort_by(|a, b| a.type_key.cmp(&b.type_key));
        Lock {
            version: LOCK_VERSION,
            entries,
            firmware: firmware.map(|app| Firmware {
                sha256: sha256_hex(app),
                rom_sha256: rom.map(sha256_hex),
                bootloader_sha256: bootloader.map(sha256_hex),
                partition_table_sha256: partition_table.map(sha256_hex),
            }),
        }
    }

    /// Read a lock file. A missing file is an `Err` like any I/O failure;
    /// absence is not a mismatch, so the caller decides whether to write one.
    pub fn load(path: &Path) -> Result<Lock, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let lock: Lock = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if lock.version != LOCK_VERSION {
            return Err(format!(
                "{}: lock version {} is not supported; this litz writes version {LOCK_VERSION}",
                path.display(),
                lock.version
            ));
        }
        Ok(lock)
    }

    /// Write the shape `docs/formats.md` §5 specifies.
    pub fn write(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string_pretty(self).map_err(|e| format!("{}: {e}", path.display()))?;
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Compare the committed lock (`self`) against what this run loaded
    /// (`actual`). Every difference is named with its full hashes, and the
    /// error points at `--update-lock`.
    pub fn check(&self, actual: &Lock) -> Result<(), String> {
        let mut notes = Vec::new();
        diff(&mut notes, &entries(self), &entries(actual));
        diff(
            &mut notes,
            &images(&self.firmware),
            &images(&actual.firmware),
        );
        if notes.is_empty() {
            return Ok(());
        }
        Err(format!(
            "litz.lock does not match this run:\n  {}\nrun with --update-lock to rewrite litz.lock",
            notes.join("\n  ")
        ))
    }
}

/// The comparable form of a catalog side: type to `version hash`.
fn entries(lock: &Lock) -> BTreeMap<&str, String> {
    lock.entries
        .iter()
        .map(|e| (e.type_key.as_str(), format!("{} {}", e.version, e.sha256)))
        .collect()
}

/// The comparable form of a firmware side, only for images that were supplied.
fn images(firmware: &Option<Firmware>) -> BTreeMap<&'static str, String> {
    let mut images = BTreeMap::new();
    if let Some(f) = firmware {
        images.insert("firmware", f.sha256.clone());
        if let Some(h) = &f.rom_sha256 {
            images.insert("rom", h.clone());
        }
        if let Some(h) = &f.bootloader_sha256 {
            images.insert("bootloader", h.clone());
        }
        if let Some(h) = &f.partition_table_sha256 {
            images.insert("partition table", h.clone());
        }
    }
    images
}

/// Name every key whose value differs, is missing or is new, in sorted order.
fn diff(notes: &mut Vec<String>, locked: &BTreeMap<&str, String>, run: &BTreeMap<&str, String>) {
    let keys: BTreeSet<&str> = locked.keys().chain(run.keys()).copied().collect();
    for key in keys {
        match (locked.get(key), run.get(key)) {
            (Some(lock), Some(run)) if lock != run => {
                notes.push(format!("changed: {key} (lock {lock}, run {run})"))
            }
            (Some(lock), None) => notes.push(format!("removed: {key} {lock}")),
            (None, Some(run)) => notes.push(format!("added: {key} {run}")),
            _ => {}
        }
    }
}

/// Lowercase hex SHA-256, 64 characters.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let digest: &[u8] = digest.as_ref();
    let mut hex = String::with_capacity(64);
    for &byte in digest {
        // Writing into a `String` cannot fail.
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_file(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("litz-lock-{}-{n}-{tag}.lock", std::process::id()))
    }

    fn catalog() -> Vec<(String, String, String)> {
        ["litz-led", "litz-resistor"]
            .map(|id| {
                (
                    id.to_string(),
                    "1.0.0".to_string(),
                    format!("[part]\nid = {id:?}\nversion = \"1.0.0\"\n"),
                )
            })
            .to_vec()
    }

    fn sample() -> Lock {
        Lock::from_run(&catalog(), Some(b"application image"), None, None, None)
    }

    #[test]
    fn a_lock_round_trips_through_its_file() {
        let path = temp_file("roundtrip");
        let lock = sample();
        lock.write(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("version = 1")
                && text.contains("[[entry]]")
                && text.contains("[firmware]"),
            "{text}"
        );
        assert_eq!(Lock::load(&path).unwrap(), lock);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn check_accepts_a_match_and_names_every_difference() {
        let lock = sample();
        assert!(lock.check(&sample()).is_ok());

        let mut catalog = catalog();
        catalog[0].2.push_str("# the LED was repainted\n");
        let changed = Lock::from_run(&catalog, Some(b"application image"), None, None, None);
        let err = lock.check(&changed).unwrap_err();
        assert!(
            err.contains("changed: litz-led") && err.contains("--update-lock"),
            "{err}"
        );

        let mut actual = sample();
        actual.entries.retain(|e| e.type_key != "litz-led");
        actual.entries.push(LockEntry {
            type_key: "litz-buzzer".into(),
            version: "1.0.0".into(),
            sha256: sha256_hex(b"buzz"),
        });
        let err = lock.check(&actual).unwrap_err();
        assert!(
            err.contains("removed: litz-led") && err.contains("added: litz-buzzer"),
            "{err}"
        );
    }

    #[test]
    fn sha256_of_nothing_is_the_known_value() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(sha256_hex(b"abc").len(), 64);
    }
}
