//! The append-only trace store, with programs and tool descriptions stored
//! alongside by hash.
//!
//! Layout under the store directory:
//! - `traces/<trace_id>.json`
//! - `programs/<program_hash>.lua`
//! - `descriptions/<description_hash>.txt`

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::trace::Trace;

pub struct TraceStore {
    traces: PathBuf,
    programs: PathBuf,
    descriptions: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("invalid store key {0:?}: expected [0-9a-f-]+")]
    InvalidKey(String),
    #[error("trace {0} already exists")]
    TraceExists(String),
    #[error("{kind} {key} already exists with different content")]
    Conflict { kind: &'static str, key: String },
    #[error("trace JSON: {0}")]
    Json(#[from] serde_json::Error),
}

impl TraceStore {
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        let store = TraceStore {
            traces: dir.join("traces"),
            programs: dir.join("programs"),
            descriptions: dir.join("descriptions"),
        };
        fs::create_dir_all(&store.traces)?;
        fs::create_dir_all(&store.programs)?;
        fs::create_dir_all(&store.descriptions)?;
        Ok(store)
    }

    /// Traces are append-only: writing an existing `trace_id` is an error.
    pub fn put_trace(&self, trace: &Trace) -> Result<(), StoreError> {
        let trace_id = &trace.header.trace_id;
        let path = key_path(&self.traces, trace_id, "json")?;
        if path.exists() {
            return Err(StoreError::TraceExists(trace_id.clone()));
        }
        write_atomic(&path, &serde_json::to_vec(trace)?)
    }

    pub fn get_trace(&self, trace_id: &str) -> Result<Trace, StoreError> {
        let path = key_path(&self.traces, trace_id, "json")?;
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }

    pub fn put_program(&self, program_hash: &str, source: &str) -> Result<(), StoreError> {
        let path = key_path(&self.programs, program_hash, "lua")?;
        put_content_addressed(&path, "program", program_hash, source)
    }

    pub fn get_program(&self, program_hash: &str) -> Result<String, StoreError> {
        let path = key_path(&self.programs, program_hash, "lua")?;
        Ok(fs::read_to_string(path)?)
    }

    pub fn put_description(&self, description_hash: &str, text: &str) -> Result<(), StoreError> {
        let path = key_path(&self.descriptions, description_hash, "txt")?;
        put_content_addressed(&path, "description", description_hash, text)
    }

    pub fn get_description(&self, description_hash: &str) -> Result<String, StoreError> {
        let path = key_path(&self.descriptions, description_hash, "txt")?;
        Ok(fs::read_to_string(path)?)
    }
}

/// Only `[0-9a-f-]+` is accepted, so a key can never name a path outside `dir`.
fn key_path(dir: &Path, key: &str, ext: &str) -> Result<PathBuf, StoreError> {
    let valid = !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-');
    if !valid {
        return Err(StoreError::InvalidKey(key.to_string()));
    }
    Ok(dir.join(format!("{key}.{ext}")))
}

/// Same content under the same key is a no-op; different content is an error.
fn put_content_addressed(
    path: &Path,
    kind: &'static str,
    key: &str,
    content: &str,
) -> Result<(), StoreError> {
    match fs::read(path) {
        Ok(existing) if existing == content.as_bytes() => Ok(()),
        Ok(_) => Err(StoreError::Conflict {
            kind,
            key: key.to_string(),
        }),
        Err(e) if e.kind() == ErrorKind::NotFound => write_atomic(path, content.as_bytes()),
        Err(e) => Err(e.into()),
    }
}

/// Write to a temp file in the target's directory, then rename over the target,
/// so a reader never sees a partial file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().expect("store paths always have a parent");
    let tmp = dir.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    Ok(result?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{Provenance, sample_trace};

    fn store() -> (TraceStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (TraceStore::open(dir.path()).unwrap(), dir)
    }

    #[test]
    fn open_creates_the_layout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("nested");
        TraceStore::open(&root).unwrap();
        for sub in ["traces", "programs", "descriptions"] {
            assert!(root.join(sub).is_dir(), "{sub}");
        }
        // Opening again over an existing layout is fine.
        TraceStore::open(&root).unwrap();
    }

    #[test]
    fn trace_with_err_record_and_every_provenance_round_trips() {
        let (store, dir) = store();
        let mut trace = sample_trace();
        trace.header.trace_id = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b".into();
        let template = trace.entries[0].clone();
        trace.entries[0].provenance = Provenance::Signed {
            by: "market".into(),
            sig: "ab".into(),
        };
        let mut onchain = template.clone();
        onchain.provenance = Provenance::Onchain {
            chain: "base".into(),
            block: 42,
            reference: "0xdead".into(),
        };
        let mut notarized = template;
        notarized.provenance = Provenance::Notarized {
            scheme: "tlsnotary".into(),
            reference: "r1".into(),
        };
        trace.entries.push(onchain);
        trace.entries.push(notarized);
        assert!(matches!(
            trace.entries[1].record.status,
            proveno::ToolCallStatus::Error
        ));
        assert_eq!(trace.entries[1].provenance, Provenance::Unsigned);
        let key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        trace.sign(&key);

        store.put_trace(&trace).unwrap();
        assert!(
            dir.path()
                .join("traces/0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b.json")
                .is_file()
        );
        let back = store.get_trace(&trace.header.trace_id).unwrap();

        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(&trace).unwrap()
        );
        let err = &back.entries[1].record;
        assert_eq!(err.error_message, "amount exceeds 50");
        assert!(matches!(err.status, proveno::ToolCallStatus::Error));
        back.verify(&key.verifying_key()).unwrap();
    }

    #[test]
    fn duplicate_put_trace_fails() {
        let (store, _dir) = store();
        let trace = sample_trace();
        store.put_trace(&trace).unwrap();
        assert!(matches!(
            store.put_trace(&trace),
            Err(StoreError::TraceExists(_))
        ));
    }

    #[test]
    fn traversal_and_invalid_keys_are_rejected() {
        let (store, dir) = store();
        let mut trace = sample_trace();
        for bad in ["../x", "..", "", "a/b", "ABCD", "00.json", "/etc", "0\0"] {
            trace.header.trace_id = bad.into();
            assert!(
                matches!(store.put_trace(&trace), Err(StoreError::InvalidKey(_))),
                "{bad:?}"
            );
            assert!(matches!(
                store.get_trace(bad),
                Err(StoreError::InvalidKey(_))
            ));
            assert!(matches!(
                store.put_program(bad, "return 1"),
                Err(StoreError::InvalidKey(_))
            ));
            assert!(matches!(
                store.get_program(bad),
                Err(StoreError::InvalidKey(_))
            ));
            assert!(matches!(
                store.put_description(bad, "text"),
                Err(StoreError::InvalidKey(_))
            ));
            assert!(matches!(
                store.get_description(bad),
                Err(StoreError::InvalidKey(_))
            ));
        }
        assert!(!dir.path().join("x").exists());
        assert!(!dir.path().join("x.json").exists());
    }

    #[test]
    fn content_addressed_puts_are_idempotent() {
        let (store, dir) = store();
        let hash = "ab".repeat(32);
        store.put_program(&hash, "return 1").unwrap();
        store.put_program(&hash, "return 1").unwrap();
        assert_eq!(store.get_program(&hash).unwrap(), "return 1");
        store.put_description(&hash, "tools").unwrap();
        store.put_description(&hash, "tools").unwrap();
        assert_eq!(store.get_description(&hash).unwrap(), "tools");
        assert!(dir.path().join(format!("programs/{hash}.lua")).is_file());
        assert!(
            dir.path()
                .join(format!("descriptions/{hash}.txt"))
                .is_file()
        );
    }

    #[test]
    fn conflicting_content_addressed_puts_fail() {
        let (store, _dir) = store();
        let hash = "cd".repeat(32);
        store.put_program(&hash, "return 1").unwrap();
        assert!(matches!(
            store.put_program(&hash, "return 2"),
            Err(StoreError::Conflict { .. })
        ));
        assert_eq!(store.get_program(&hash).unwrap(), "return 1");

        store.put_description(&hash, "tools").unwrap();
        assert!(matches!(
            store.put_description(&hash, "other"),
            Err(StoreError::Conflict { .. })
        ));
        assert_eq!(store.get_description(&hash).unwrap(), "tools");
    }

    #[test]
    fn writes_leave_no_temp_files() {
        let (store, dir) = store();
        store.put_trace(&sample_trace()).unwrap();
        store.put_program("01", "return 1").unwrap();
        store.put_description("02", "tools").unwrap();
        for sub in ["traces", "programs", "descriptions"] {
            let names: Vec<_> = fs::read_dir(dir.path().join(sub))
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            assert_eq!(names.len(), 1, "{sub}: {names:?}");
            assert!(!names[0].starts_with(".tmp-"), "{sub}: {names:?}");
        }
    }
}
