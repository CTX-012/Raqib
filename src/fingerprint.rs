//! Model fingerprinting (latest.md Tier 3.1).
//!
//! Hashes a partial view of a weight file — `len_le_bytes ||
//! head[0..1MiB] || tail[len-64KiB..len]` — into a SHA-256, prefixed
//! with `sha256-head1m-tail64k:` so the format is self-describing for
//! downstream consumers.
//!
//! **Why partial.** A full hash of a 40 GB Llama-70B is too slow to
//! run on every process exit. Head+tail is enough to distinguish
//! quantization variants (the GGUF metadata header changes) and
//! distinct fine-tunes (last KB of tensor data differs), at a cost
//! of <50 ms even on slow disks. The known false-equivalence case
//! (two files with identical head+tail but different middle bytes)
//! is documented and accepted as a tradeoff in the spec.
//!
//! **Cache.** The fingerprint stays the same as long as
//! `(device_inode, mtime, len)` is stable, so a JSON cache keyed on
//! that tuple lets us avoid rehashing on every run. Cache lives at
//! `~/.cache/edge_monitor/fingerprints.json` by default; missing-
//! cache, malformed-cache, and read-only-cache cases all degrade
//! gracefully (just compute and skip persistence).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const HEAD_BYTES: usize = 1_048_576; // 1 MiB
const TAIL_BYTES: usize = 65_536; // 64 KiB

/// Compute the partial-hash fingerprint of `path`. Returns `None`
/// (with a warn log) on read errors — fingerprinting is best-effort
/// telemetry, not a load-bearing security primitive.
pub fn fingerprint_model_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut hasher = Sha256::new();
    hasher.update(len.to_le_bytes());

    let mut head = vec![0u8; HEAD_BYTES.min(len as usize)];
    let n = file.read(&mut head)?;
    hasher.update(&head[..n]);

    // Only hash the tail when the file is bigger than head+tail,
    // otherwise head already covers everything.
    if len > (HEAD_BYTES + TAIL_BYTES) as u64 {
        let mut tail = [0u8; TAIL_BYTES];
        file.seek(SeekFrom::End(-(TAIL_BYTES as i64)))?;
        file.read_exact(&mut tail)?;
        hasher.update(tail);
    }

    Ok(format!("sha256-head1m-tail64k:{:x}", hasher.finalize()))
}

/// Cache key — `(device, inode, mtime_secs, len)` uniquely identifies
/// a file content version on a single machine. We include `dev` so
/// the same inode number on different filesystems doesn't collide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct CacheKey {
    dev: u64,
    inode: u64,
    mtime_secs: i64,
    len: u64,
}

/// On-disk cache shape. JSON for hand-editability and easy migration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct OnDiskCache {
    /// Schema version. Bump if the fingerprint algorithm changes so
    /// stale caches get invalidated automatically.
    version: u32,
    /// `key_json -> fingerprint`. We can't use `CacheKey` directly as
    /// a JSON map key because serde_json requires String keys; we
    /// encode the key as `dev:inode:mtime:len`.
    entries: HashMap<String, String>,
}

const CACHE_VERSION: u32 = 1;

fn key_str(k: CacheKey) -> String {
    format!("{}:{}:{}:{}", k.dev, k.inode, k.mtime_secs, k.len)
}

fn parse_key_str(s: &str) -> Option<CacheKey> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 4 {
        return None;
    }
    Some(CacheKey {
        dev: parts[0].parse().ok()?,
        inode: parts[1].parse().ok()?,
        mtime_secs: parts[2].parse().ok()?,
        len: parts[3].parse().ok()?,
    })
}

/// Stateful fingerprinter with a JSON-backed cache.
pub struct Fingerprinter {
    cache_path: Option<PathBuf>,
    in_memory: HashMap<CacheKey, String>,
    /// Set true on every cache miss; persisted to disk via
    /// [`Self::persist`] (or implicit Drop).
    dirty: bool,
}

impl Fingerprinter {
    /// Open or create a fingerprinter rooted at `cache_path`. An
    /// empty path disables the on-disk cache entirely (in-memory
    /// only). A malformed or wrong-version cache file is silently
    /// reset; persistence still works on the next save.
    pub fn open(cache_path: Option<PathBuf>) -> Self {
        let in_memory = match &cache_path {
            Some(p) => load_cache(p).unwrap_or_default(),
            None => HashMap::new(),
        };
        Self {
            cache_path,
            in_memory,
            dirty: false,
        }
    }

    /// Fingerprint `path`, consulting the cache by (dev, inode,
    /// mtime, len). Returns `None` on read failure. Cache is updated
    /// in memory; call [`Self::persist`] to write to disk (or rely on
    /// Drop).
    pub fn fingerprint(&mut self, path: &Path) -> Option<String> {
        let meta = std::fs::metadata(path).ok()?;
        let key = CacheKey {
            dev: meta.dev(),
            inode: meta.ino(),
            mtime_secs: meta.mtime(),
            len: meta.len(),
        };
        if let Some(hit) = self.in_memory.get(&key) {
            return Some(hit.clone());
        }
        let fp = match fingerprint_model_file(path) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "fingerprint failed");
                return None;
            }
        };
        self.in_memory.insert(key, fp.clone());
        self.dirty = true;
        Some(fp)
    }

    /// Write the in-memory cache to disk if anything changed.
    /// No-op when no cache_path is configured or nothing was modified.
    pub fn persist(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(path) = &self.cache_path else {
            return;
        };
        if let Some(parent) = path.parent()
            && !parent.exists()
            && std::fs::create_dir_all(parent).is_err()
        {
            return; // can't write the cache; that's fine
        }
        let entries: HashMap<String, String> = self
            .in_memory
            .iter()
            .map(|(k, v)| (key_str(*k), v.clone()))
            .collect();
        let on_disk = OnDiskCache {
            version: CACHE_VERSION,
            entries,
        };
        let body = match serde_json::to_string(&on_disk) {
            Ok(s) => s,
            Err(_) => return,
        };
        let _ = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .and_then(|mut f| f.write_all(body.as_bytes()));
        self.dirty = false;
    }
}

impl Drop for Fingerprinter {
    fn drop(&mut self) {
        self.persist();
    }
}

fn load_cache(path: &Path) -> std::io::Result<HashMap<CacheKey, String>> {
    let s = std::fs::read_to_string(path)?;
    let parsed: OnDiskCache = serde_json::from_str(&s).unwrap_or_default();
    if parsed.version != CACHE_VERSION {
        return Ok(HashMap::new());
    }
    let mut out = HashMap::new();
    for (k, v) in parsed.entries {
        if let Some(key) = parse_key_str(&k) {
            out.insert(key, v);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn same_file_same_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.bin");
        fs::write(&path, b"hello world").unwrap();
        let a = fingerprint_model_file(&path).unwrap();
        let b = fingerprint_model_file(&path).unwrap();
        assert_eq!(a, b);
        assert!(a.starts_with("sha256-head1m-tail64k:"));
    }

    #[test]
    fn modified_byte_zero_changes_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.bin");
        fs::write(&path, b"AAAA").unwrap();
        let a = fingerprint_model_file(&path).unwrap();
        fs::write(&path, b"BAAA").unwrap();
        let b = fingerprint_model_file(&path).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn modified_tail_byte_changes_fingerprint() {
        // For a >1MiB+64KiB file, modifying the last byte hits the
        // tail window and changes the fingerprint.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");
        let total = HEAD_BYTES + TAIL_BYTES + 1024;
        let mut data = vec![0xAB; total];
        fs::write(&path, &data).unwrap();
        let a = fingerprint_model_file(&path).unwrap();
        // Bump the very last byte.
        *data.last_mut().unwrap() = 0xCD;
        fs::write(&path, &data).unwrap();
        let b = fingerprint_model_file(&path).unwrap();
        assert_ne!(a, b);
    }

    /// Documented limitation: middle bytes (between head and tail)
    /// are NOT covered. Two files with same head+tail but different
    /// middles produce the same fingerprint. The spec accepts this
    /// as a deliberate tradeoff for speed.
    #[test]
    fn middle_only_change_collides_documented() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("middle.bin");
        let total = HEAD_BYTES + TAIL_BYTES + 8192;
        let mut data = vec![0u8; total];
        fs::write(&path, &data).unwrap();
        let a = fingerprint_model_file(&path).unwrap();
        // Modify a byte deep in the middle (after head, before tail).
        let mid_idx = HEAD_BYTES + 4096;
        data[mid_idx] = 0xFF;
        fs::write(&path, &data).unwrap();
        let b = fingerprint_model_file(&path).unwrap();
        assert_eq!(
            a, b,
            "spec accepts middle-only collisions for speed; if this test\
             starts failing we accidentally fixed it"
        );
    }

    #[test]
    fn cache_hit_avoids_recomputation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cached.bin");
        fs::write(&path, b"contents").unwrap();
        let cache_file = dir.path().join("fp_cache.json");

        let mut fp = Fingerprinter::open(Some(cache_file.clone()));
        let a = fp.fingerprint(&path).unwrap();
        // Force persist so the second instance reads from disk.
        fp.persist();
        // Mutate the file but DON'T touch mtime — this is technically
        // possible only with futimens; instead we instantiate a fresh
        // fingerprinter and verify that the on-disk cache survives.
        drop(fp);

        let mut fp2 = Fingerprinter::open(Some(cache_file));
        let b = fp2.fingerprint(&path).unwrap();
        assert_eq!(a, b);
        // The fresh fingerprinter shouldn't have computed anything —
        // dirty stays false because the cache had a hit.
        assert!(!fp2.dirty);
    }

    #[test]
    fn cache_invalidates_on_mtime_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ch.bin");
        fs::write(&path, b"v1").unwrap();
        let cache_file = dir.path().join("fp_cache.json");
        let mut fp = Fingerprinter::open(Some(cache_file.clone()));
        let a = fp.fingerprint(&path).unwrap();
        // Sleep enough for mtime resolution (typically ≥1 s on ext4
        // without nanosecond timestamps).
        std::thread::sleep(std::time::Duration::from_secs(1));
        let mut f = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        f.write_all(b"v2_different").unwrap();
        drop(f);
        let b = fp.fingerprint(&path).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn cache_handles_missing_file_gracefully() {
        let mut fp = Fingerprinter::open(None);
        let result = fp.fingerprint(Path::new("/nonexistent/nope.gguf"));
        assert!(result.is_none());
    }

    #[test]
    fn malformed_cache_file_is_reset_silently() {
        let dir = tempfile::tempdir().unwrap();
        let cache_file = dir.path().join("bad.json");
        std::fs::write(&cache_file, b"{ not valid json").unwrap();
        // Constructor must not panic.
        let fp = Fingerprinter::open(Some(cache_file));
        assert!(fp.in_memory.is_empty());
    }
}
