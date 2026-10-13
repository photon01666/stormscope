//! Anonymous S3 access to the NOAA NEXRAD buckets + real-time polling. (W2)
//!
//! Wraps [`nexrad_data::aws`] (`archive` + `realtime`) against the Unidata
//! mirrors of the NOAA Open Data buckets:
//!
//! * archive volumes — `unidata-nexrad-level2`
//!   (`{YYYY}/{MM}/{DD}/{SITE}/{SITE}{YYYYMMDD}_{HHMMSS}_{V06}`), and
//! * live chunks — `unidata-nexrad-level2-chunks`
//!   (`{SITE}/{volume}/{YYYYMMDD-HHMMSS}-{seq}-{S|I|E}`).
//!
//! All requests are **plain anonymous HTTPS GETs**: `nexrad-data` performs
//! no SigV4 signing and runs no AWS credential chain, so no credentials are
//! ever required and `AWS_*` environment variables cannot leak into (or
//! break) these calls. `S3Source` drives the async `nexrad-data` API from
//! dedicated current-thread tokio runtimes behind blocking methods, matching
//! the crate's std-thread/`mpsc` conventions.

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use std::collections::BTreeMap;
use std::io::Read;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::cache::DiskCache;

/// Archive bucket used by `nexrad-data` (Unidata mirror of `noaa-nexrad-level2`).
pub const ARCHIVE_BUCKET: &str = "unidata-nexrad-level2";
/// Real-time chunk bucket.
pub const REALTIME_BUCKET: &str = "unidata-nexrad-level2-chunks";

/// Poll interval for the live chunk bucket. Chunks land every ~4-12 s.
const REALTIME_POLL: Duration = Duration::from_secs(5);

/// Handle to a running realtime poller. Dropping it (or calling
/// [`RealtimePoller::stop`]) cancels the poller thread at its next loop
/// iteration — deterministic teardown on site switches, instead of waiting
/// for the old poller to notice a dropped receiver minutes later (which
/// piled up poller threads and got us rate-limited by S3).
#[must_use = "dropping the handle cancels the poller"]
pub struct RealtimePoller {
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl RealtimePoller {
    pub fn stop(&self) {
        self.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Drop for RealtimePoller {
    fn drop(&mut self) {
        self.stop();
    }
}
/// Give up on a volume if no new chunk shows up for this long (site down /
/// gap in coverage); advance to the next volume index.
const REALTIME_STALL_TIMEOUT: Duration = Duration::from_secs(180);
/// Give up entirely after this long of *continuous* listing failures (or a
/// site with no data at all) — also bounds how long a poller thread can
/// outlive a dropped receiver while the stream is silent.
const REALTIME_GIVE_UP: Duration = Duration::from_secs(30 * 60);
/// Volumes buffered toward the consumer before the poller blocks.
const REALTIME_CHANNEL_DEPTH: usize = 4;
/// Hard timeout for S3 list requests (nexrad-data's HTTP client sets no
/// timeout of its own; a stalled socket would otherwise hang a caller
/// forever).
const LIST_TIMEOUT: Duration = Duration::from_secs(60);
/// Hard timeout for S3 object downloads.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// One archive volume file in the archive bucket.
#[derive(Clone, Debug)]
pub struct ArchiveVolumeRef {
    pub site: String,
    pub date: NaiveDate,
    /// Full S3 key, e.g. `2026/10/11/KTLX/KTLX20261011_000356_V06`.
    pub key: String,
    pub volume_start: DateTime<Utc>,
    /// Object size in bytes; 0 because S3 listings via `nexrad-data` do not
    /// carry sizes (a HEAD per object would double the request count).
    pub size_bytes: u64,
}

impl ArchiveVolumeRef {
    /// File-name portion of the key, e.g. `KTLX20261011_000356_V06`.
    pub fn file_name(&self) -> &str {
        self.key.rsplit('/').next().unwrap_or(&self.key)
    }

    /// Cache-relative path for this volume's gzipped bytes
    /// (`l2/<site>/<name>.gz` under the [`DiskCache`] root).
    pub fn cache_rel(&self) -> String {
        format!("l2/{}/{}.gz", self.site, self.file_name())
    }
}

pub struct S3Source {
    cache: DiskCache,
    /// Current-thread runtime used for `nexrad-data` archive calls.
    rt: tokio::runtime::Runtime,
    /// Serializes `block_on` entries so concurrent callers don't contend on
    /// the single-threaded driver.
    rt_lock: Mutex<()>,
    /// In-memory decoded volumes keyed by S3 key (the `Arc` short-circuit
    /// for `fetch_archive` cache hits).
    mem: Mutex<BTreeMap<String, Arc<radar_core::Volume>>>,
}

impl S3Source {
    pub fn new(cache: DiskCache) -> Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("building tokio runtime for nexrad-data")?;
        Ok(Self {
            cache,
            rt,
            rt_lock: Mutex::new(()),
            mem: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn cache(&self) -> &DiskCache {
        &self.cache
    }

    /// Run a `nexrad-data` future with a hard timeout (its HTTP client has
    /// none of its own, and a stalled socket would otherwise hang forever).
    fn block_on_timeout<F: std::future::Future>(
        &self,
        limit: Duration,
        fut: F,
    ) -> Result<F::Output> {
        let _guard = self.rt_lock.lock().unwrap_or_else(|e| e.into_inner());
        // NOTE: `tokio::time::timeout` must be constructed inside the async
        // block — its timer is created eagerly and needs the runtime context.
        match self.rt.block_on(async move {
            tokio::time::timeout(limit, fut).await
        }) {
            Ok(out) => Ok(out),
            Err(_) => Err(anyhow!("S3 operation timed out after {}s", limit.as_secs())),
        }
    }

    /// Newest-first listing of archive volumes for a site/date.
    pub fn list_archive(&self, site: &str, date: NaiveDate) -> Result<Vec<ArchiveVolumeRef>> {
        let site = site.trim().to_ascii_uppercase();
        if site.len() != 4 {
            return Err(anyhow!("site id must be 4 characters, got {site:?}"));
        }
        // S3 listings are rate-limited under load; a couple of quick retries
        // beat failing the whole archive request.
        let identifiers = {
            let mut last_err = None;
            let mut out = None;
            for attempt in 0..3 {
                match self
                    .block_on_timeout(LIST_TIMEOUT, nexrad_data::aws::archive::list_files(&site, &date))
                {
                    Ok(Ok(ids)) => {
                        out = Some(ids);
                        break;
                    }
                    Ok(Err(e)) => last_err = Some(anyhow!("S3 list error: {e}")),
                    Err(e) => last_err = Some(anyhow!("S3 list failed: {e:#}")),
                }
                if attempt < 2 {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                }
            }
            out.with_context(|| {
                format!(
                    "listing {ARCHIVE_BUCKET} for {site} on {date}: {}",
                    last_err.map(|e| format!("{e:#}")).unwrap_or_default()
                )
            })?
        };

        let mut refs: Vec<ArchiveVolumeRef> = identifiers
            .into_iter()
            .filter_map(|id| {
                let name = id.name().to_string();
                if name.is_empty() {
                    return None;
                }
                // `..._V06_MDM` keys are MesoData Manager sidecar products,
                // NOT Archive II volumes — decoding them fails with
                // "truncated record". Only plain `_V06` volumes are usable.
                if name.ends_with("_MDM") || !name.contains("_V06") {
                    return None;
                }
                // The identifier carries the volume start time when the file
                // name is well-formed; skip junk keys defensively.
                let volume_start = id.date_time()?;
                Some(ArchiveVolumeRef {
                    site: site.clone(),
                    date,
                    key: format!("{}/{}/{}", date.format("%Y/%m/%d"), site, name),
                    volume_start,
                    size_bytes: 0,
                })
            })
            .collect();
        refs.sort_by(|a, b| b.volume_start.cmp(&a.volume_start));
        Ok(refs)
    }

    /// Download a volume's raw (gzipped) Archive II bytes without decoding.
    /// Served from the disk cache when present; otherwise fetched anonymously
    /// and stored under `<cache>/l2/<site>/<name>.gz`. Useful on its own
    /// (e.g. fetching decode fixtures) and as the decode-independent half of
    /// [`S3Source::fetch_archive`].
    pub fn fetch_archive_bytes(&self, r: &ArchiveVolumeRef) -> Result<Vec<u8>> {
        let rel = r.cache_rel();
        if let Some(bytes) = self.cache.read(&rel)? {
            log::debug!("cache hit: {rel}");
            return Ok(bytes);
        }
        let ident = nexrad_data::aws::archive::Identifier::new(r.file_name().to_string());
        let file = self
            .block_on_timeout(DOWNLOAD_TIMEOUT, nexrad_data::aws::archive::download_file(ident))
            .with_context(|| format!("downloading {ARCHIVE_BUCKET}/{}", r.key))?
            .with_context(|| format!("downloading {ARCHIVE_BUCKET}/{}", r.key))?;
        let bytes = file.data().to_vec();
        if bytes.is_empty() {
            return Err(anyhow!("downloaded object {} is empty", r.key));
        }
        // Normalize the cache to gzip regardless of how the bucket stores the
        // object (the Unidata mirror serves raw Archive II `AR2V` bytes).
        let gz = gzip(&bytes).context("gzip-compressing volume for cache")?;
        self.cache.write(&rel, &gz)?;
        let _ = self.cache.sweep(); // opportunistic size cap
        Ok(gz)
    }

    /// Fetch (memory-cache hit, disk-cache hit, or download) + decode a
    /// volume.
    pub fn fetch_archive(&self, r: &ArchiveVolumeRef) -> Result<Arc<radar_core::Volume>> {
        if let Some(v) = self.mem.lock().unwrap().get(&r.key) {
            return Ok(Arc::clone(v));
        }
        let gz = self.fetch_archive_bytes(r)?;
        let volume = decode_archive_bytes(&gz)?;
        self.mem.lock().unwrap().insert(r.key.clone(), Arc::clone(&volume));
        Ok(volume)
    }

    /// Start a background real-time poller for a site; emits fully
    /// assembled + decoded volumes as they become complete. The returned
    /// channel is bounded (a few volumes) so a slow consumer applies
    /// backpressure instead of unbounded buffering; the background thread
    /// exits when the receiver is dropped.
    pub fn start_realtime(&self, site: &str) -> Result<(Receiver<radar_core::Volume>, RealtimePoller)> {
        let site = site.trim().to_ascii_uppercase();
        if site.len() != 4 {
            return Err(anyhow!("site id must be 4 characters, got {site:?}"));
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(REALTIME_CHANNEL_DEPTH);
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        // A dedicated runtime per poller thread keeps `nexrad-data`'s async
        // calls off the UI thread entirely.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("building tokio runtime for realtime poller")?;
        let cancel_in_thread = std::sync::Arc::clone(&cancel);
        let cache = self.cache.clone();
        std::thread::Builder::new()
            .name(format!("nexrad-live-{site}"))
            .spawn(move || realtime_poll_loop(&rt, &site, cache, tx, &cancel_in_thread))
            .context("spawning realtime poller thread")?;
        Ok((rx, RealtimePoller { cancel }))
    }
}

/// Run a future on the poller's runtime with a hard timeout; `None` means
/// the request stalled (nexrad-data's HTTP client configures no timeout).
fn block_on_timed<F: std::future::Future>(
    rt: &tokio::runtime::Runtime,
    limit: Duration,
    fut: F,
) -> Option<F::Output> {
    // NOTE: `tokio::time::timeout` must be constructed inside the async
    // block — its timer is created eagerly and needs the runtime context.
    match rt.block_on(async move { tokio::time::timeout(limit, fut).await }) {
        Ok(out) => Some(out),
        Err(_) => None,
    }
}

/// Background loop: find the latest live volume, then follow the chunk
/// stream, assembling each volume in sequence order and emitting decoded
/// results. Never panics on data errors — logs at `warn!` and continues.
/// Exits when the receiver is dropped (detected on send) or after
/// [`REALTIME_GIVE_UP`] of continuous hard failures.
fn realtime_poll_loop(
    rt: &tokio::runtime::Runtime,
    site: &str,
    cache: DiskCache,
    tx: std::sync::mpsc::SyncSender<radar_core::Volume>,
    cancel: &std::sync::atomic::AtomicBool,
) {
    use nexrad_data::aws::realtime::{download_chunk, get_latest_volume, list_chunks_in_volume};
    use std::sync::atomic::Ordering;
    let cancelled = || cancel.load(Ordering::Relaxed);

    // Tracks continuous hard network/empty-site failures for the give-up bound.
    fn failing_too_long(failing_since: &mut Option<Instant>) -> bool {
        let start = *failing_since.get_or_insert_with(Instant::now);
        start.elapsed() > REALTIME_GIVE_UP
    }
    let mut failing_since: Option<Instant> = None;

    // Phase 1: binary-search the newest volume with data.
    let mut volume = loop {
        if cancelled() {
            log::debug!("realtime {site}: cancelled");
            return;
        }
        match block_on_timed(rt, LIST_TIMEOUT, get_latest_volume(site)) {
            None => {
                log::warn!("realtime {site}: get_latest_volume stalled >{}s", LIST_TIMEOUT.as_secs());
                if failing_too_long(&mut failing_since) {
                    log::warn!("realtime {site}: giving up after continuous failures");
                    return;
                }
            }
            Some(Ok(res)) => {
                if let Some(v) = res.volume {
                    log::info!(
                        "realtime {site}: latest volume {} (found in {} listings)",
                        v.as_number(),
                        res.calls
                    );
                    failing_since = None;
                    break v;
                }
                log::warn!("realtime {site}: no volume with data yet; retrying");
                if failing_too_long(&mut failing_since) {
                    log::warn!("realtime {site}: giving up (site has no live data)");
                    return;
                }
            }
            Some(Err(e)) => {
                log::warn!("realtime {site}: get_latest_volume failed: {e}; retrying");
                if failing_too_long(&mut failing_since) {
                    log::warn!("realtime {site}: giving up after continuous failures");
                    return;
                }
            }
        }
        std::thread::sleep(REALTIME_POLL);
    };

    // Phase 2: follow the chunk stream.
    loop {
        if cancelled() {
            log::debug!("realtime {site}: cancelled");
            return;
        }
        let mut assembled: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
        let mut epoch: Option<NaiveDateTime> = None;
        let mut start_seq: Option<usize> = None;
        let mut saw_end = false;
        let mut last_progress = Instant::now();

        loop {
            if cancelled() {
                log::debug!("realtime {site}: cancelled");
                return;
            }
            let chunks = match block_on_timed(rt, LIST_TIMEOUT, list_chunks_in_volume(site, volume, 200))
            {
                None => {
                    log::warn!(
                        "realtime {site}: listing volume {} stalled >{}s",
                        volume.as_number(),
                        LIST_TIMEOUT.as_secs()
                    );
                    if failing_too_long(&mut failing_since) {
                        log::warn!("realtime {site}: giving up after continuous failures");
                        return;
                    }
                    std::thread::sleep(REALTIME_POLL);
                    continue;
                }
                Some(Ok(c)) => {
                    failing_since = None;
                    c
                }
                Some(Err(e)) => {
                    log::warn!(
                        "realtime {site}: listing volume {} failed: {e}",
                        volume.as_number()
                    );
                    if failing_too_long(&mut failing_since) {
                        log::warn!("realtime {site}: giving up after continuous failures");
                        return;
                    }
                    std::thread::sleep(REALTIME_POLL);
                    continue;
                }
            };
            let mut chunks = chunks;
            chunks.sort_by_key(|c| c.sequence());

            // Volume directories are recycled round-robin: if a Start chunk
            // for a NEW epoch appears (or the first one arrives late), reset
            // the assembly so stale chunks from the previous rotation can't
            // contaminate this volume.
            if let Some(start) = chunks
                .iter()
                .find(|c| c.chunk_type() == nexrad_data::aws::realtime::ChunkType::Start)
            {
                let prefix = *start.date_time_prefix();
                if epoch != Some(prefix) {
                    if epoch.is_some() {
                        log::warn!(
                            "realtime {site}: volume {} changed epoch; resetting assembly",
                            volume.as_number()
                        );
                    }
                    epoch = Some(prefix);
                    // Chunk sequences are 1-based ("001-S" is the first chunk).
                    start_seq = Some(start.sequence());
                    assembled.clear();
                    saw_end = false;
                    last_progress = Instant::now();
                }
            }
            // Once the epoch is known, ignore any foreign chunks.
            if let Some(prefix) = epoch {
                chunks.retain(|c| c.date_time_prefix() == &prefix);
            }

            let mut downloaded = 0usize;
            for chunk in &chunks {
                if chunk.chunk_type() == nexrad_data::aws::realtime::ChunkType::End {
                    saw_end = true;
                }
                if assembled.contains_key(&chunk.sequence()) {
                    continue;
                }
                match block_on_timed(rt, DOWNLOAD_TIMEOUT, download_chunk(site, chunk)) {
                    None => log::warn!(
                        "realtime {site}: chunk {} download stalled >{}s",
                        chunk.name(),
                        DOWNLOAD_TIMEOUT.as_secs()
                    ),
                    Some(Ok((_, data))) => {
                        assembled.insert(chunk.sequence(), data.data().to_vec());
                        downloaded += 1;
                        last_progress = Instant::now();
                    }
                    Some(Err(e)) => {
                        // 404 while a volume is still being filled is normal.
                        log::debug!(
                            "realtime {site}: chunk {} not downloadable yet: {e}",
                            chunk.name()
                        );
                    }
                }
            }
            log::debug!(
                "realtime {site}: volume {} listing: {} chunks, {} downloaded, {} assembled, end={saw_end}",
                volume.as_number(),
                chunks.len(),
                downloaded,
                assembled.len()
            );

            if saw_end && !assembled.is_empty() {
                // Concatenate strictly in sequence order, starting at the
                // Start chunk's (1-based) sequence; gaps mean we can't trust
                // the assembly.
                let have: Vec<usize> = assembled.keys().copied().collect();
                let contiguous_from_start = match (have.first(), start_seq) {
                    (Some(&first), Some(seq)) => {
                        first == seq
                            && have.windows(2).all(|w| w[1] == w[0] + 1)
                    }
                    _ => false,
                };
                if !contiguous_from_start {
                    log::warn!(
                        "realtime {site}: volume {} has chunk gaps ({have:?}, start {start_seq:?}); skipping",
                        volume.as_number()
                    );
                } else {
                    let mut bytes = Vec::new();
                    for data in assembled.values() {
                        bytes.extend_from_slice(data);
                    }
                    // Persist the assembled volume under the same cache key the
                    // archive path uses, so every scan the live feed delivers is
                    // saved too — and the freshness/catch-up fetches of this
                    // same scan become cache hits instead of re-downloads.
                    persist_volume(&cache, site, epoch, &bytes);
                    // Guard the decode: a panic in the decoder must not kill
                    // the poller thread silently.
                    let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        radar_decode::decode_realtime_volume(&bytes)
                    }));
                    match decoded {
                        Ok(Ok(v)) => {
                            log::info!(
                                "realtime {site}: volume {} decoded ({} sweeps, {} bytes)",
                                volume.as_number(),
                                v.sweeps.len(),
                                bytes.len()
                            );
                            if tx.send(v).is_err() {
                                return; // receiver dropped
                            }
                        }
                        Ok(Err(e)) => log::warn!(
                            "realtime {site}: decoding volume {} failed: {e}",
                            volume.as_number()
                        ),
                        Err(_) => log::warn!(
                            "realtime {site}: decode panicked on volume {}",
                            volume.as_number()
                        ),
                    }
                }
                break; // advance to next volume
            }

            if last_progress.elapsed() > REALTIME_STALL_TIMEOUT {
                log::warn!(
                    "realtime {site}: volume {} stalled for {}s; advancing",
                    volume.as_number(),
                    REALTIME_STALL_TIMEOUT.as_secs()
                );
                break;
            }
            std::thread::sleep(REALTIME_POLL);
        }

        volume = volume.next();
    }
}

/// Store a freshly assembled live volume in the disk cache under the same
/// `l2/<site>/<name>.gz` key the archive path uses (`<SITE>YYYYMMDD_HHMMSS_V06`,
/// derived from the volume's start time `epoch`), so live- and archive-received
/// scans share one cache and the catch-up fetches don't re-download them.
///
/// The bytes are normalized to the exact representation the archive path
/// stores — a *single* gzip member wrapping the raw Archive II stream — because
/// the live concatenation may arrive raw (`AR2V…`) or as a possibly
/// multi-member gzip stream, and the archive decoder reads only one member.
/// Anything that isn't Archive II is left alone rather than poisoning the key.
/// Best-effort: failures are logged and the live pipeline continues.
fn persist_volume(cache: &DiskCache, site: &str, epoch: Option<chrono::NaiveDateTime>, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let Some(prefix) = epoch else {
        log::debug!("realtime {site}: no volume epoch; skipping cache write");
        return;
    };
    let raw: std::borrow::Cow<[u8]> = if bytes.starts_with(b"AR2V") {
        std::borrow::Cow::Borrowed(bytes)
    } else if is_gzip(bytes) {
        let mut out = Vec::new();
        match flate2::read::MultiGzDecoder::new(bytes).read_to_end(&mut out) {
            Ok(_) => std::borrow::Cow::Owned(out),
            Err(e) => {
                log::warn!("realtime {site}: gunzip live volume for cache failed: {e}");
                return;
            }
        }
    } else {
        log::debug!("realtime {site}: volume not in Archive II form; not caching");
        return;
    };
    if !raw.starts_with(b"AR2V") {
        log::debug!("realtime {site}: live volume missing AR2V header; not caching");
        return;
    }
    let gz = match gzip(&raw) {
        Ok(g) => g,
        Err(e) => {
            log::warn!("realtime {site}: gzip volume for cache failed: {e:#}");
            return;
        }
    };
    let name = format!("{site}{}_V06", prefix.format("%Y%m%d_%H%M%S"));
    let rel = format!("l2/{site}/{name}.gz");
    match cache.write(&rel, &gz) {
        Ok(_) => {
            log::debug!("realtime {site}: cached volume {name} ({} bytes gz)", gz.len());
            let _ = cache.sweep(); // opportunistic size cap
        }
        Err(e) => log::warn!("realtime {site}: caching volume {name} failed: {e:#}"),
    }
}

/// Decode an archive volume file's bytes into an `Arc<Volume>`.
///
/// The Unidata archive mirror stores raw Archive II (`AR2V…`) objects while
/// the historical NOAA bucket stores gzip-compressed ones, and
/// `radar_decode::decode_volume_file`'s contract is "gzipped volume file".
/// Try each plausible representation until one decodes: as-given,
/// decompressed (NOAA-style gz input), then re-compressed (strict-gz input).
fn decode_archive_bytes(bytes: &[u8]) -> Result<Arc<radar_core::Volume>> {
    if let Ok(v) = radar_decode::decode_volume_file(bytes) {
        return Ok(Arc::new(v));
    }
    if is_gzip(bytes) {
        let raw = gunzip(bytes).context("decompressing gzip volume")?;
        if let Ok(v) = radar_decode::decode_volume_file(&raw) {
            return Ok(Arc::new(v));
        }
    } else {
        let gz = gzip(bytes).context("compressing raw volume")?;
        if let Ok(v) = radar_decode::decode_volume_file(&gz) {
            return Ok(Arc::new(v));
        }
    }
    // Fall through: re-run once to surface the real decode error.
    radar_decode::decode_volume_file(bytes)
        .map(Arc::new)
        .map_err(Into::into)
}

fn is_gzip(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b
}

fn gzip(bytes: &[u8]) -> Result<Vec<u8>> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(bytes)?;
    Ok(enc.finish()?)
}

fn gunzip(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes).read_to_end(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gzip_roundtrip_and_magic() {
        let payload = b"AR2V06.0 some volume bytes 0123456789";
        let gz = gzip(payload).unwrap();
        assert!(is_gzip(&gz));
        assert!(!is_gzip(payload));
        assert_eq!(gunzip(&gz).unwrap(), payload);
    }

    #[test]
    fn archive_ref_key_shape() {
        let date = NaiveDate::from_ymd_opt(2026, 10, 11).unwrap();
        let r = ArchiveVolumeRef {
            site: "KTLX".into(),
            date,
            key: "2026/10/11/KTLX/KTLX20261011_000356_V06".into(),
            volume_start: Utc::now(),
            size_bytes: 0,
        };
        assert_eq!(r.file_name(), "KTLX20261011_000356_V06");
        assert_eq!(r.cache_rel(), "l2/KTLX/KTLX20261011_000356_V06.gz");
    }

    #[test]
    fn persist_volume_normalizes_to_single_gzip_member() {
        let root = std::env::temp_dir().join(format!("stormscope-persist-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cache = DiskCache::new(root.clone(), 1 << 20);
        let prefix = NaiveDate::from_ymd_opt(2026, 10, 13)
            .unwrap()
            .and_hms_opt(13, 15, 0)
            .unwrap();
        // Not a real volume — persist_volume only inspects the "AR2V" header.
        let raw = b"AR2V0001synthetic-volume-body".repeat(20);

        // Raw Archive II bytes are stored as a single gzip member.
        persist_volume(&cache, "KTWX", Some(prefix), &raw);
        let gz = cache.read("l2/KTWX/KTWX20261013_131500_V06.gz").unwrap().unwrap();
        assert!(is_gzip(&gz));
        assert_eq!(gunzip(&gz).unwrap(), raw);

        // Multi-member gzip input is normalized to one member too (the archive
        // decoder reads only the first member, so this is the correctness bit).
        let mut multi = gzip(&raw[..11]).unwrap();
        multi.extend_from_slice(&gzip(&raw[11..]).unwrap());
        persist_volume(&cache, "KTLX", Some(prefix), &multi);
        let gz2 = cache.read("l2/KTLX/KTLX20261013_131500_V06.gz").unwrap().unwrap();
        assert_eq!(gunzip(&gz2).unwrap(), raw);

        // Non-Archive II bytes are never cached.
        persist_volume(&cache, "KOAX", Some(prefix), b"not-a-volume");
        assert!(cache.read("l2/KOAX/KOAX20261013_131500_V06.gz").unwrap().is_none());

        let _ = std::fs::remove_dir_all(&root);
    }
}
