//! Content-addressed thumbnail cache.
//!
//! Kept outside SQLite so it can be copied, rsynced or thrown away on its
//! own, and so identical pixels stored under many paths cost one file.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub struct ThumbStore {
    root: PathBuf,
}

pub fn hex32(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xF) as u32, 16).unwrap());
    }
    s
}

impl ThumbStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `ab/cd/<key>.jpg` — two levels of fan-out keeps directories small
    /// enough that a filesystem listing stays usable.
    pub fn path_for(&self, key: &str) -> PathBuf {
        let (a, b) = (&key[0..2], &key[2..4]);
        self.root.join(a).join(b).join(format!("{key}.jpg"))
    }

    /// Store the bytes and return their key. Writing the same thumbnail twice
    /// is a no-op.
    pub fn put(&self, jpeg: &[u8]) -> Result<String> {
        let key = hex32(&blake3_of(jpeg)[..16]);
        let path = self.path_for(&key);
        if path.exists() {
            return Ok(key);
        }
        let parent = path.parent().context("нет родительского каталога")?;
        fs::create_dir_all(parent).with_context(|| format!("не создать {}", parent.display()))?;
        // Write beside the target and rename, so a crash never leaves a
        // half-written thumbnail that later looks valid.
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, jpeg)?;
        fs::rename(&tmp, &path)?;
        Ok(key)
    }

    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        if key.len() < 4 {
            return None;
        }
        fs::read(self.path_for(key)).ok()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn blake3_of(bytes: &[u8]) -> [u8; 32] {
    // Duplicated rather than depending on pc-hash: this crate sits below it.
    let mut h = Hasher::new();
    h.update(bytes);
    h.finish()
}

/// Minimal FNV-style fallback is not good enough for addressing content, so
/// this is a thin shim over the same BLAKE3 the rest of the tool uses.
struct Hasher(blake3::Hasher);

impl Hasher {
    fn new() -> Self {
        Self(blake3::Hasher::new())
    }
    fn update(&mut self, b: &[u8]) {
        self.0.update(b);
    }
    fn finish(&self) -> [u8; 32] {
        *self.0.finalize().as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_reads_back() {
        let tmp = tempfile::tempdir().unwrap();
        let s = ThumbStore::new(tmp.path());
        let key = s.put(b"pretend jpeg").unwrap();
        assert_eq!(s.get(&key).unwrap(), b"pretend jpeg");
    }

    #[test]
    fn identical_content_shares_one_file() {
        let tmp = tempfile::tempdir().unwrap();
        let s = ThumbStore::new(tmp.path());
        let a = s.put(b"same").unwrap();
        let b = s.put(b"same").unwrap();
        assert_eq!(a, b);
        assert_ne!(a, s.put(b"different").unwrap());
    }

    #[test]
    fn a_missing_or_malformed_key_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let s = ThumbStore::new(tmp.path());
        assert!(s.get("deadbeefdeadbeef").is_none());
        assert!(s.get("x").is_none());
    }

    #[test]
    fn no_temporary_files_are_left_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let s = ThumbStore::new(tmp.path());
        s.put(b"content").unwrap();
        let leftovers = walkdir::WalkDir::new(tmp.path())
            .into_iter()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }
}
