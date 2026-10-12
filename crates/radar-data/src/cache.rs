//! Disk cache for downloaded volumes (and shared tile cache location).
//! Root: ~/.cache/stormscope (XDG_CACHE_HOME aware). (W2)

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default size cap: ~2 GiB.
pub const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

pub struct DiskCache {
    root: PathBuf,
    max_bytes: u64,
}

impl DiskCache {
    pub fn default_root() -> PathBuf {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".cache"))
            .join("stormscope")
    }

    /// Cache rooted at `root` with the default ~2 GB cap.
    pub fn with_default_max(root: PathBuf) -> Self {
        Self::new(root, DEFAULT_MAX_BYTES)
    }

    pub fn new(root: PathBuf, max_bytes: u64) -> Self {
        Self { root, max_bytes }
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Resolve a relative cache key (e.g. `l2/KTLX/KTLX20261011_000356_V06.gz`)
    /// to an absolute path under the root. Rejects absolute paths and `..`
    /// traversal so callers can't escape the cache directory.
    pub fn path_for(&self, rel: &str) -> Option<PathBuf> {
        use std::path::Component;
        let mut path = self.root.clone();
        let mut parts = 0;
        for comp in Path::new(rel).components() {
            match comp {
                Component::Normal(p) => {
                    path.push(p);
                    parts += 1;
                }
                _ => return None, // absolute, `..`, `.`, prefix — not allowed
            }
        }
        if parts == 0 {
            return None;
        }
        Some(path)
    }

    /// Read cached bytes; `Ok(None)` on a miss.
    pub fn read(&self, rel: &str) -> std::io::Result<Option<Vec<u8>>> {
        let path = self
            .path_for(rel)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad cache key"))?;
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Atomically store bytes (write to a temp file in the same directory,
    /// then rename). Returns the resulting path.
    pub fn write(&self, rel: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
        let path = self
            .path_for(rel)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad cache key"))?;
        let dir = path
            .parent()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent dir"))?;
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(
            ".{}.{}.tmp",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("cache"),
            std::process::id()
        ));
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// Total size in bytes of all files under the root.
    pub fn total_bytes(&self) -> std::io::Result<u64> {
        Ok(self
            .files_recursive()?
            .into_iter()
            .map(|(_, size, _)| size)
            .sum())
    }

    /// Remove oldest files (by mtime) until the cache is under `max_bytes`.
    /// Call occasionally; also called after writes when useful.
    pub fn sweep(&self) -> std::io::Result<()> {
        let mut files = self.files_recursive()?; // (mtime, size, path)
        if files.is_empty() {
            return Ok(());
        }
        let mut total: u64 = files.iter().map(|(_, size, _)| *size).sum();
        if total <= self.max_bytes {
            return Ok(());
        }
        // Oldest first; tie-break on path for determinism.
        files.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
        for (mtime, size, path) in files {
            if total <= self.max_bytes {
                break;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    log::debug!(
                        "cache sweep: removed {} ({} bytes, mtime {:?})",
                        path.display(),
                        size,
                        mtime
                    );
                    total -= size;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => total = total.saturating_sub(size),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// All files under the root as (mtime, size, path), oldest-first unordered.
    fn files_recursive(&self) -> std::io::Result<Vec<(SystemTime, u64, PathBuf)>> {
        let mut out = Vec::new();
        if !self.root.exists() {
            return Ok(out);
        }
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue, // raced with a removal; skip
            };
            for entry in entries.flatten() {
                let path = entry.path();
                match entry.file_type() {
                    Ok(ft) if ft.is_dir() => stack.push(path),
                    Ok(_) => {
                        let meta = match std::fs::metadata(&path) {
                            Ok(m) => m,
                            Err(_) => continue,
                        };
                        let mtime = meta.modified().unwrap_or(UNIX_EPOCH);
                        out.push((mtime, meta.len(), path));
                    }
                    Err(_) => {}
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(rel: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "stormscope-cache-test-{}-{}",
            std::process::id(),
            rel
        ))
    }

    #[test]
    fn write_read_roundtrip_and_traversal_guard() {
        let root = scratch("roundtrip");
        let _ = std::fs::remove_dir_all(&root);
        let cache = DiskCache::new(root.clone(), 1 << 20);
        assert!(cache.path_for("/etc/passwd").is_none());
        assert!(cache.path_for("../escape").is_none());
        assert!(cache.path_for("a/../../escape").is_none());
        assert!(cache.read("l2/KTLX/x.gz").unwrap().is_none(), "miss");
        let p = cache.write("l2/KTLX/x.gz", b"hello").unwrap();
        assert!(p.starts_with(&root));
        assert_eq!(cache.read("l2/KTLX/x.gz").unwrap().unwrap(), b"hello");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sweep_removes_oldest_first() {
        let root = scratch("sweep");
        let _ = std::fs::remove_dir_all(&root);
        let cache = DiskCache::new(root.clone(), 30);
        cache.write("old.bin", &[1u8; 20]).unwrap();
        // Make "old" clearly older than the next write.
        let old = cache.path_for("old.bin").unwrap();
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        let f = std::fs::File::options().write(true).open(&old).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(past)).unwrap();
        cache.write("new.bin", &[2u8; 20]).unwrap();
        assert!(cache.total_bytes().unwrap() >= 40);
        cache.sweep().unwrap();
        let total = cache.total_bytes().unwrap();
        assert!(total <= 30, "total {total} should be <= 30");
        assert!(cache.read("old.bin").unwrap().is_none(), "oldest removed");
        assert!(cache.read("new.bin").unwrap().is_some(), "newer kept");
        let _ = std::fs::remove_dir_all(&root);
    }
}
