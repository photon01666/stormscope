//! Disk cache for downloaded volumes (and shared tile cache location).
//! Root: ~/.cache/stormscope (XDG_CACHE_HOME aware).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default size cap: 10 GiB (radar volumes plus the shared tile cache).
pub const DEFAULT_MAX_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// One radar volume present in the on-disk cache.
#[derive(Debug, Clone)]
pub struct CachedVolume {
    pub start: chrono::DateTime<chrono::Utc>,
    pub path: std::path::PathBuf,
    pub bytes: u64,
}

/// Every cached Level II volume for one site, oldest first.
pub fn list_cached_volumes(site: &str) -> Vec<CachedVolume> {
    list_cached_volumes_in(&DiskCache::default_root(), site)
}

pub fn list_cached_volumes_in(root: &std::path::Path, site: &str) -> Vec<CachedVolume> {
    let dir = root.join("l2").join(site);
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(start) = parse_cached_name(site, &name) else {
                continue;
            };
            let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push(CachedVolume {
                start,
                path: entry.path(),
                bytes,
            });
        }
    }
    out.sort_by_key(|v| v.start);
    out
}

/// `KTWX20261016_074236_V06.gz` -> UTC timestamp. Only ordinary Level II
/// volume names are accepted.
fn parse_cached_name(site: &str, name: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let rest = name.strip_prefix(site)?;
    if rest.len() < 16 || !name.ends_with(".gz") {
        return None;
    }
    if rest.as_bytes().get(8) != Some(&b'_') {
        return None;
    }
    let stamp = format!("{}{}", &rest[0..8], &rest[9..15]);
    let naive = chrono::NaiveDateTime::parse_from_str(&stamp, "%Y%m%d%H%M%S").ok()?;
    Some(chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
        naive,
        chrono::Utc,
    ))
}

#[derive(Clone)]
pub struct DiskCache {
    root: PathBuf,
    max_bytes: u64,
}

impl DiskCache {
    pub fn default_root() -> PathBuf {
        // Windows keeps per-user app data in %LOCALAPPDATA%, not $HOME/.cache
        // (an unset HOME would otherwise drop the cache next to the exe).
        #[cfg(windows)]
        {
            if let Some(base) = std::env::var_os("LOCALAPPDATA") {
                return PathBuf::from(base).join("stormscope");
            }
        }
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".cache")
            })
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
        let path = self.path_for(rel).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad cache key")
        })?;
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Atomically store bytes (write to a temp file in the same directory,
    /// then rename). Returns the resulting path.
    pub fn write(&self, rel: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
        let path = self.path_for(rel).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad cache key")
        })?;
        let dir = path.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent dir")
        })?;
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
            .files_under(&self.root)?
            .into_iter()
            .map(|(_, size, _)| size)
            .sum())
    }

    /// Total size in bytes of everything under `<root>/<section>` — e.g. `l2`
    /// for radar volumes, `tiles` for the basemap. Zero if the section doesn't
    /// exist yet.
    pub fn section_bytes(&self, section: &str) -> std::io::Result<u64> {
        Ok(self
            .files_under(&self.root.join(section))?
            .into_iter()
            .map(|(_, size, _)| size)
            .sum())
    }

    /// Remove every cached file under `<root>/<section>` (radar volumes or the
    /// tile cache) and prune the emptied directories, including the section
    /// directory itself. Returns the number of bytes freed. Best-effort: files
    /// that vanish mid-walk (raced with a concurrent write) are ignored.
    pub fn clear_section(&self, section: &str) -> std::io::Result<u64> {
        let dir = self.root.join(section);
        let freed = self.clear_under(&dir)?;
        // Drop the (now empty) section dir too, so `l2/`/`tiles/` don't linger.
        let _ = std::fs::remove_dir(&dir);
        Ok(freed)
    }

    /// Remove every cached file under the root (all sections). Returns the
    /// number of bytes freed.
    pub fn clear(&self) -> std::io::Result<u64> {
        self.clear_under(&self.root)
    }

    /// Delete files under `dir` (recursively) and prune the emptied
    /// subdirectories below it. `dir` itself is left in place.
    fn clear_under(&self, dir: &Path) -> std::io::Result<u64> {
        let files = self.files_under(dir)?;
        let mut freed: u64 = 0;
        for (_, size, path) in &files {
            match std::fs::remove_file(path) {
                Ok(()) => freed += *size,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        // Deepest-first so parents become empty before removal is attempted
        // (`remove_dir` only succeeds on an empty directory).
        let mut dirs = self.dirs_under(dir)?;
        dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
        for d in dirs {
            let _ = std::fs::remove_dir(&d);
        }
        Ok(freed)
    }

    /// All subdirectories under `dir` (excluding `dir` itself), unordered.
    fn dirs_under(&self, dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        if !dir.exists() {
            return Ok(out);
        }
        let mut stack = vec![dir.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue, // raced with a removal; skip
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    stack.push(path.clone());
                    out.push(path);
                }
            }
        }
        Ok(out)
    }

    /// Remove oldest files (by mtime) until the cache is under `max_bytes`.
    /// Call occasionally; also called after writes when useful.
    pub fn sweep(&self) -> std::io::Result<()> {
        let mut files = self.files_under(&self.root)?; // (mtime, size, path)
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
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    total = total.saturating_sub(size)
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// All files under `dir` as (mtime, size, path), oldest-first unordered.
    fn files_under(&self, dir: &Path) -> std::io::Result<Vec<(SystemTime, u64, PathBuf)>> {
        let mut out = Vec::new();
        if !dir.exists() {
            return Ok(out);
        }
        let mut stack = vec![dir.to_path_buf()];
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

    #[test]
    fn cached_names_parse_only_real_volumes() {
        let t = parse_cached_name("KTWX", "KTWX20261016_074236_V06.gz").expect("a volume");
        assert_eq!(t.to_string(), "2026-10-16 07:42:36 UTC");
        assert!(parse_cached_name("KTWX", "KTLX20261016_074236_V06.gz").is_none());
        assert!(parse_cached_name("KTWX", "KTWX20261016_074236_V06").is_none());
        assert!(parse_cached_name("KTWX", "KTWX_nonsense.gz").is_none());
    }

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
    fn clear_removes_everything_and_reports_freed() {
        let root = scratch("clear");
        let _ = std::fs::remove_dir_all(&root);
        let cache = DiskCache::new(root.clone(), 1 << 20);
        cache.write("l2/KTLX/a.gz", &[1u8; 100]).unwrap();
        cache.write("l2/KTWX/b.gz", &[2u8; 50]).unwrap();
        cache.write("tiles/8/1/2.tile", &[3u8; 25]).unwrap();
        assert_eq!(cache.total_bytes().unwrap(), 175);

        let freed = cache.clear().unwrap();
        assert_eq!(freed, 175);
        assert_eq!(cache.total_bytes().unwrap(), 0);
        assert!(cache.read("l2/KTLX/a.gz").unwrap().is_none());
        // Subdirectories are pruned, but the root stays usable for new writes.
        assert!(!root.join("l2").exists());
        cache.write("l2/KTLX/c.gz", b"x").unwrap();
        assert!(cache.read("l2/KTLX/c.gz").unwrap().is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn section_bytes_and_clear_section_are_independent() {
        let root = scratch("section");
        let _ = std::fs::remove_dir_all(&root);
        let cache = DiskCache::new(root.clone(), 1 << 20);
        cache.write("l2/KTLX/a.gz", &[1u8; 100]).unwrap();
        cache.write("l2/KTWX/b.gz", &[2u8; 50]).unwrap();
        cache.write("tiles/8/1/2.tile", &[3u8; 25]).unwrap();
        assert_eq!(cache.section_bytes("l2").unwrap(), 150);
        assert_eq!(cache.section_bytes("tiles").unwrap(), 25);
        assert_eq!(cache.section_bytes("missing").unwrap(), 0);
        assert_eq!(cache.total_bytes().unwrap(), 175);

        // Clearing radar data must not touch the map tiles (and vice versa).
        let freed = cache.clear_section("l2").unwrap();
        assert_eq!(freed, 150);
        assert_eq!(cache.section_bytes("l2").unwrap(), 0);
        assert_eq!(cache.section_bytes("tiles").unwrap(), 25, "tiles untouched");
        assert!(!root.join("l2").exists(), "section dir pruned");
        assert!(root.join("tiles").exists());

        let freed = cache.clear_section("tiles").unwrap();
        assert_eq!(freed, 25);
        assert_eq!(cache.total_bytes().unwrap(), 0);
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
        f.set_times(std::fs::FileTimes::new().set_modified(past))
            .unwrap();
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
