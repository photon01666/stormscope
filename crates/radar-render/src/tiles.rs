//! Raster basemap tile layer with on-disk cache, an LRU of decoded egui
//! textures, and background fetching.
//!
//! Providers (both keyless, both require attribution):
//!   1. Wikimedia `maps.wikimedia.org/osm-intl` — generous limits, no key.
//!   2. OpenStreetMap `tile.openstreetmap.org` — fallback.
//! NOTE: Carto `basemaps.cartocdn.com` was dropped: it now requires an API
//! key and serves "API KEY REQUIRED" watermark tiles with HTTP 200, which
//! poisons the on-disk cache with ads. `validate_tile` guards against this
//! class of failure for any provider.
//!
//! Tile placement is derived from the exact same [`MapView`] origin the radar
//! rasterizer uses, so basemap and overlay line up sub-pixel exactly. See
//! [`tile_view_rect`] / [`tile_range`] — they are pure functions so the math
//! is unit-testable without a network or an egui context.

use std::collections::{HashMap, HashSet, VecDeque};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use lru::LruCache;

use crate::projection::MapView;

/// Tile edge in pixels (Carto/OSM standard raster tiles).
pub const TILE_PX: f64 = 256.0;
/// Highest integer zoom requested.
pub const MAX_TILE_ZOOM: u8 = 19;

/// Wikimedia's keyless OSM-based raster basemap (primary).
/// Attribution required: "© OpenStreetMap contributors".
const WIKIMEDIA_URL: &str = "https://maps.wikimedia.org/osm-intl/{z}/{x}/{y}.png";
/// OpenStreetMap standard tiles (fallback; light-usage policy — the disk
/// cache keeps request volume low).
const OSM_URL: &str = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";
/// Descriptive User-Agent (OSM tile usage policy asks for one).
const USER_AGENT: &str =
    "stormscope/0.1 (Linux NEXRAD radar viewer; github.com/stormscope/stormscope)";
/// Concurrent tile downloads. Deliberately small: be polite to the servers.
/// Concurrent tile *worker threads*. Disk-cache hits finish in ~2ms, so
/// this mainly bounds thread count while a warm viewport loads — it must NOT
/// pace the queue, otherwise a warm 100-tile cache takes seconds to trickle
/// in and looks like re-downloading.
const MAX_IN_FLIGHT: usize = 8;
/// Decoded textures kept alive. 192 x 256x256 RGBA = 48 MB of GPU/system
/// memory — this is a *cache*, not a working set (a full viewport is ~30-90
/// tiles), and on shared-memory GPUs every tile here competes with system
/// RAM: an oversized cap under memory pressure is what makes the GPU driver
/// stall on buffer eviction (whole-desktop stutter).
const TEXTURE_CACHE_TILES: usize = 192;
const TILE_TIMEOUT: Duration = Duration::from_secs(10);
/// Minimum gap between actual *network* requests (enforced inside workers by
/// `NetGate`, so cached tiles never wait on it). Wikimedia rate-limits
/// anonymous clients (403) on bursts.
const NET_SPACING: Duration = Duration::from_millis(60);
/// Concurrent *network* fetches across all workers.
const MAX_NET_IN_FLIGHT: usize = 3;
/// After every provider rate-limits us, pause network fetches this long
/// (disk-cache reads keep flowing).
const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(15);

/// How long a provider that answered 403/429 is skipped entirely.
///
/// Wikimedia 403s anonymous clients for long stretches; without a breaker the
/// app re-asks it for *every tile* (one wasted request + a global fetch pause
/// each), so panning looks like the map is broken. 30 minutes, because a
/// provider that blocks anonymous clients does it for hours, not seconds.
const PROVIDER_BLOCK: Duration = Duration::from_secs(30 * 60);
/// How long a failed tile stays suppressed before a retry is allowed (so a
/// transient 403/timeout isn't a permanent blank spot).
const FAILED_RETRY: Duration = Duration::from_secs(180);
/// On-disk cache policy: tiles older than 30 days are evicted...
const CACHE_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// ...and the cache never exceeds 2 GB.
const CACHE_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// How many zoom levels up the ancestor fallback searches for a cached tile
/// (parent, grand-parent, great-grand-parent).
const ANCESTOR_MAX_LEVELS: usize = 3;

/// Shared network politeness gate: spaces request starts, caps network
/// concurrency across all worker threads, and carries the rate-limit pause.
/// Disk-cache reads never touch it.
struct NetGate {
    last_start: std::sync::Mutex<Instant>,
    paused_until: std::sync::Mutex<Instant>,
    active: std::sync::atomic::AtomicUsize,
}

impl NetGate {
    fn new() -> Self {
        let past = Instant::now() - Duration::from_secs(3600);
        Self {
            last_start: std::sync::Mutex::new(past),
            paused_until: std::sync::Mutex::new(past),
            active: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Wait for a network slot + spacing. `Err(())` means a rate-limit pause is in
    /// rate-limit pause — caller should skip the network entirely.
    fn enter(&self) -> Result<NetSlot<'_>, ()> {
        use std::sync::atomic::Ordering;
        if Instant::now() < *self.paused_until.lock().unwrap_or_else(|e| e.into_inner()) {
            return Err(());
        }
        // Concurrency cap.
        loop {
            if self.active.fetch_add(1, Ordering::Relaxed) < MAX_NET_IN_FLIGHT {
                break;
            }
            self.active.fetch_sub(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(25));
        }
        // Start-of-request spacing.
        loop {
            let mut last = self.last_start.lock().unwrap_or_else(|e| e.into_inner());
            let elapsed = last.elapsed();
            if elapsed >= NET_SPACING {
                *last = Instant::now();
                break;
            }
            let wait = NET_SPACING - elapsed;
            drop(last);
            std::thread::sleep(wait);
        }
        Ok(NetSlot { gate: self })
    }

    fn pause(&self) {
        let mut p = self.paused_until.lock().unwrap_or_else(|e| e.into_inner());
        *p = Instant::now() + RATE_LIMIT_PAUSE;
    }
}

/// RAII network slot; released on drop even on error paths.
struct NetSlot<'a> {
    gate: &'a NetGate,
}

impl Drop for NetSlot<'_> {
    fn drop(&mut self) {
        self.gate
            .active
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Identity of one tile in the standard XYZ scheme.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct TileKey {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

/// Integer zoom used for tile math: the view zoom rounded to the nearest
/// integer. Tiles are then scaled by `2^(view.zoom - z)` so fractional zooms
/// still align exactly with the projection.
pub fn tile_zoom(view: &MapView) -> u8 {
    view.zoom.round().clamp(0.0, MAX_TILE_ZOOM as f64) as u8
}

/// Scale factor from tile-pixel space (integer zoom) to view-pixel space.
pub fn tile_scale(view: &MapView) -> f64 {
    2f64.powf(view.zoom - tile_zoom(view) as f64)
}

/// Inclusive tile index range covering the viewport, clamped to the valid
/// Web Mercator grid. May be empty (view entirely off-grid).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TileRange {
    pub z: u8,
    pub x0: i64,
    pub x1: i64,
    pub y0: i64,
    pub y1: i64,
}

impl TileRange {
    pub fn is_empty(&self) -> bool {
        self.x0 > self.x1 || self.y0 > self.y1
    }

    pub fn keys(&self) -> impl Iterator<Item = TileKey> + '_ {
        let z = self.z;
        (self.y0..=self.y1).flat_map(move |y| {
            (self.x0..=self.x1).map(move |x| TileKey {
                z,
                x: x as u32,
                y: y as u32,
            })
        })
    }
}

/// Integer tile range covering the viewport.
pub fn tile_range(view: &MapView) -> TileRange {
    let z = tile_zoom(view);
    let ts = TILE_PX * tile_scale(view);
    let (ox, oy) = view.origin_px();
    let (w, h) = (view.width() as f64, view.height() as f64);
    let n = 1i64 << z;

    let x0 = (ox / ts).floor() as i64;
    let x1 = ((ox + w) / ts).floor() as i64;
    let y0 = (oy / ts).floor() as i64;
    let y1 = ((oy + h) / ts).floor() as i64;

    let (cx0, cx1) = if x1 < 0 || x0 > n - 1 {
        (1, 0) // empty
    } else {
        (x0.max(0), x1.min(n - 1))
    };
    let (cy0, cy1) = if y1 < 0 || y0 > n - 1 {
        (1, 0)
    } else {
        (y0.max(0), y1.min(n - 1))
    };
    TileRange {
        z,
        x0: cx0,
        x1: cx1,
        y0: cy0,
        y1: cy1,
    }
}

/// Viewport-pixel rect `[x0, y0, x1, y1]` (top-left origin, f64, sub-pixel
/// exact) that one tile occupies, or `None` when `key.z` doesn't match the
/// view's integer zoom.
pub fn tile_view_rect(key: TileKey, view: &MapView) -> Option<[f64; 4]> {
    if key.z != tile_zoom(view) {
        return None;
    }
    let ts = TILE_PX * tile_scale(view);
    let (ox, oy) = view.origin_px();
    let x0 = key.x as f64 * ts - ox;
    let y0 = key.y as f64 * ts - oy;
    Some([x0, y0, x0 + ts, y0 + ts])
}

/// UV sub-rectangle of `child` inside the cached ancestor tile `anc`, for
/// painting the ancestor cropped to the child's quadrant.
///
/// Pure math (unit-testable): the ancestor covers a 2^k × 2^k block of
/// children where `k = child.z - anc.z`; the child's offset within that block
/// selects its UV window. Returns `None` unless `anc` is a true ancestor.
pub fn ancestor_uv(child: &TileKey, anc: &TileKey) -> Option<egui::Rect> {
    if child.z <= anc.z || child.z - anc.z > 16 {
        return None;
    }
    let k = child.z - anc.z;
    let ax = child.x >> k;
    let ay = child.y >> k;
    if ax != anc.x || ay != anc.y {
        return None;
    }
    let n = (1u64 << k) as f32;
    let cx = (child.x & ((1 << k) - 1)) as f32;
    let cy = (child.y & ((1 << k) - 1)) as f32;
    Some(egui::Rect::from_min_max(
        egui::pos2(cx / n, cy / n),
        egui::pos2((cx + 1.0) / n, (cy + 1.0) / n),
    ))
}

/// On-disk cache policy: delete tiles older than `max_age`, then (if still
/// over `max_bytes`) delete oldest-first until under the cap. Runs on a
/// background thread at startup; all errors are best-effort.
fn sweep_cache(dir: &Path, max_age: Duration, max_bytes: u64) {
    fn walk(d: &Path, out: &mut Vec<(std::path::PathBuf, std::time::SystemTime, u64)>, depth: u8) {
        if depth > 6 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out, depth + 1);
            } else if p.extension().is_some_and(|x| x == "tile") {
                if let Ok(md) = e.metadata() {
                    let mtime = md.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    out.push((p, mtime, md.len()));
                }
            }
        }
    }
    let mut entries: Vec<(std::path::PathBuf, std::time::SystemTime, u64)> = Vec::new();
    walk(dir, &mut entries, 0);
    if entries.is_empty() {
        return;
    }
    let now = std::time::SystemTime::now();
    let mut total: u64 = entries.iter().map(|e| e.2).sum();
    let mut removed = 0usize;

    // 1) Expiry pass: drop tiles older than max_age.
    entries.retain(|(p, mtime, len)| {
        let age = now.duration_since(*mtime).unwrap_or_default();
        if age > max_age {
            let _ = std::fs::remove_file(p);
            total -= len;
            removed += 1;
            false
        } else {
            true
        }
    });

    // 2) Size cap pass: oldest-first until under the cap.
    if total > max_bytes {
        entries.sort_by_key(|(_, mtime, _)| *mtime);
        for (p, _, len) in &entries {
            if total <= max_bytes {
                break;
            }
            let _ = std::fs::remove_file(p);
            total -= len;
            removed += 1;
        }
    }

    log::info!(
        "tile cache sweep: removed {removed} tiles, {:.1} MB remain",
        total as f64 / (1024.0 * 1024.0)
    );
}

fn tile_path(cache_dir: &Path, key: TileKey) -> PathBuf {
    cache_dir
        .join(key.z.to_string())
        .join(key.x.to_string())
        .join(format!("{}.tile", key.y))
}

fn tile_url(template: &str, key: TileKey) -> String {
    template
        .replace("{z}", &key.z.to_string())
        .replace("{x}", &key.x.to_string())
        .replace("{y}", &key.y.to_string())
}

/// Decode PNG/JPEG bytes into an egui color image (straight alpha input),
/// applying the dark-basemap transform.
///
/// No free dark raster basemap exists anymore (Carto gated dark_all behind an
/// API key and serves watermarks). Instead a dark theme is synthesized from the
/// real Wikimedia/OSM light tiles: invert luminance while keeping each pixel's
/// original hue, then lift the floor so land isn't pure black and push the
/// contrast so road/water labels stay legible as light ink on a dark ground.
fn decode_tile(bytes: &[u8]) -> Option<egui::ColorImage> {
    let mut img = image::load_from_memory(bytes).ok()?.to_rgba8();
    if img.width() == 0 || img.height() == 0 {
        return None;
    }
    for px in img.chunks_exact_mut(4) {
        let [r, g, b, a] = [px[0], px[1], px[2], px[3]];
        if a == 0 {
            continue;
        }
        // Luminance in 0..1.
        let l = (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0;
        // Invert: bright land/water becomes dark; dark ink becomes bright.
        let inv = 1.0 - l;
        // RadarScope-style near-black: land sits at ~2.5% luminance and even
        // the brightest features (roads, labels) are capped at ~58% — a dim
        // gray map, not a bright one.
        let out_l = 0.025 + 0.55 * inv;
        // Gain toward the source hue: scale the original channel by how far
        // the target luminance moved, so tints (blue water, green land) survive.
        let scale = if l > 0.001 { out_l / l } else { out_l };
        let ch = |c: u8| -> u8 {
            let v = (c as f32 * scale).round();
            v.clamp(0.0, 255.0) as u8
        };
        let (sr, sg, sb) = (ch(r), ch(g), ch(b));
        // Desaturate 55% toward the scaled luminance: RadarScope's basemap is
        // essentially monochrome gray-on-black.
        let gray = (0.2126 * sr as f32 + 0.7152 * sg as f32 + 0.0722 * sb as f32).round();
        let mix = |c: u8| -> u8 {
            let v = c as f32 + (gray - c as f32) * 0.55;
            v.round().clamp(0.0, 255.0) as u8
        };
        px[0] = mix(sr);
        px[1] = mix(sg);
        px[2] = mix(sb);
    }
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [img.width() as usize, img.height() as usize],
        img.as_raw(),
    ))
}

/// Providers in order. Wikimedia first (no key, generous anonymous limits),
/// OSM second. Carto is deliberately absent — it now serves "API KEY
/// REQUIRED" watermark tiles with HTTP 200.
const PROVIDERS: [&str; 2] = [WIKIMEDIA_URL, OSM_URL];

/// Per-provider circuit-breaker state: millis (since process start) until
/// which the provider is skipped because it answered 403/429.
static PROVIDER_BLOCKED_MS: [std::sync::atomic::AtomicU64; PROVIDERS.len()] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];
/// Provider that last served a tile successfully: tried first next time, so
/// the app follows whichever server is actually answering today.
static LAST_GOOD_PROVIDER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// Monotonic base for the millisecond stamps above.
static BASE: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

/// Which providers to try, in order, right now: the last good one first,
/// then the rest, skipping blocked ones. Pure, so it is unit-tested.
fn provider_candidates(
    blocked_until_ms: [u64; PROVIDERS.len()],
    last_good: usize,
    now_ms: u64,
) -> Vec<usize> {
    let n = blocked_until_ms.len();
    let start = last_good.min(n.saturating_sub(1));
    (0..n)
        .map(|off| (start + off) % n)
        .filter(|&i| blocked_until_ms[i] <= now_ms)
        .collect()
}

/// Reject responses that are not a real basemap tile.
///
/// Providers signal errors in ways that defeat naive `status().is_success()`
/// checks: Carto served watermark ads with HTTP 200, and a 404 body was a
/// valid 1x1 PNG. Anything that is not a decodable 256x256 image is dropped
/// here, before it can reach the disk cache.
fn validate_tile(key: TileKey, bytes: &[u8]) -> Option<egui::ColorImage> {
    let image = decode_tile(bytes)?;
    if image.size != [TILE_PX as usize, TILE_PX as usize] {
        log::warn!(
            "tile {key:?}: rejecting {}x{} image (expected {TILE_PX}x{TILE_PX}) — error/placeholder tile",
            image.size[0],
            image.size[1]
        );
        return None;
    }
    Some(image)
}

/// Returns `(bytes, all_throttled)`: `all_throttled` means every provider that
/// could try answered 403/429 — only then should the caller pause. A single
/// throttled provider is circuit-broken here instead (see [`PROVIDER_BLOCK`]),
/// which keeps the healthy provider serving.
fn http_get(client: &reqwest::blocking::Client, key: TileKey) -> (Option<Vec<u8>>, bool) {
    use std::sync::atomic::Ordering;
    let now_ms = BASE.elapsed().as_millis() as u64;
    let blocked: [u64; PROVIDERS.len()] =
        std::array::from_fn(|i| PROVIDER_BLOCKED_MS[i].load(Ordering::Relaxed));
    let candidates =
        provider_candidates(blocked, LAST_GOOD_PROVIDER.load(Ordering::Relaxed), now_ms);
    if candidates.is_empty() {
        return (None, true); // every provider is in cooldown
    }

    let mut throttled = 0usize;
    for i in candidates.iter().copied() {
        let url = tile_url(PROVIDERS[i], key);
        match client.get(&url).send() {
            Ok(resp) if resp.status().is_success() => match resp.bytes() {
                Ok(b) if !b.is_empty() => {
                    LAST_GOOD_PROVIDER.store(i, Ordering::Relaxed);
                    return (Some(b.to_vec()), false);
                }
                Ok(_) => log::warn!("tile {key:?}: empty body from {url}"),
                Err(e) => log::warn!("tile {key:?}: body read error from {url}: {e}"),
            },
            Ok(resp) => {
                let status = resp.status();
                if status.as_u16() == 403 || status.as_u16() == 429 {
                    // Circuit-break this provider: one log line per cooldown,
                    // not one per tile (a rate-limited server stays blocked).
                    PROVIDER_BLOCKED_MS[i].store(
                        now_ms + PROVIDER_BLOCK.as_millis() as u64,
                        Ordering::Relaxed,
                    );
                    throttled += 1;
                    log::warn!(
                        "tile {key:?}: HTTP {status} from {url} — skipping this provider for {} min",
                        PROVIDER_BLOCK.as_secs() / 60
                    );
                } else {
                    log::warn!("tile {key:?}: HTTP {status} from {url}");
                }
            }
            Err(e) => log::warn!("tile {key:?}: fetch error from {url}: {e}"),
        }
    }
    (None, throttled >= candidates.len())
}

/// Worker: disk cache first, then the provider list. Never runs on the UI
/// thread. Every byte path goes through [`validate_tile`] so a bad cached
/// tile (e.g. a pre-fix watermark) is evicted and refetched.
fn fetch_tile(
    client: &reqwest::blocking::Client,
    cache_dir: &Path,
    key: TileKey,
    net: &NetGate,
) -> TileMsg {
    // Disk cache first — never gated (this is what makes warm startups fast).
    let path = tile_path(cache_dir, key);
    if let Ok(bytes) = std::fs::read(&path) {
        if let Some(image) = validate_tile(key, &bytes) {
            return TileMsg {
                key,
                image: Some(image),
            };
        }
        log::warn!("tile {key:?}: evicting invalid cache file {path:?}");
        let _ = std::fs::remove_file(&path);
    }
    // Network path — gated for politeness (spacing + concurrency + pause).
    let _slot = match net.enter() {
        Ok(slot) => slot,
        Err(()) => {
            // Rate-limit pause: skip the network; the tile goes to the
            // failed set and retries after FAILED_RETRY.
            return TileMsg { key, image: None };
        }
    };
    let (bytes, throttled) = http_get(client, key);
    if throttled {
        net.pause();
    }
    let Some(bytes) = bytes else {
        return TileMsg { key, image: None };
    };
    if let Some(image) = validate_tile(key, &bytes) {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                log::debug!("tile {key:?}: mkdir {parent:?}: {e}");
            }
        }
        if let Err(e) = std::fs::write(&path, &bytes) {
            log::debug!("tile {key:?}: cache write {path:?}: {e}");
        }
        return TileMsg {
            key,
            image: Some(image),
        };
    }
    TileMsg { key, image: None }
}

struct TileMsg {
    key: TileKey,
    image: Option<egui::ColorImage>,
}

pub struct TileLayer {
    cache_dir: PathBuf,
    client: Option<reqwest::blocking::Client>,
    textures: LruCache<TileKey, egui::TextureHandle>,
    placeholder: Option<egui::TextureHandle>,
    /// Requested tiles whose worker hasn't reported back (queued + running).
    pending: HashSet<TileKey>,
    /// Subset of `pending` waiting for a free worker slot.
    queue: VecDeque<TileKey>,
    /// Tiles that failed this session (retried on a cooldown, see `pump`).
    failed: HashMap<TileKey, Instant>,
    /// Network politeness gate shared by all workers (spacing/concurrency/
    /// rate-limit pause). Disk reads never go through it.
    net: Arc<NetGate>,
    rx: Receiver<TileMsg>,
    tx: Sender<TileMsg>,
}

impl TileLayer {
    pub fn new(cache_dir: PathBuf) -> Self {
        // Apply the cache policy (30-day expiry, 2 GB cap) off-thread so
        // startup isn't blocked walking the directory.
        {
            let dir = cache_dir.clone();
            let _ = std::thread::Builder::new()
                .name("tile-cache-sweep".into())
                .spawn(move || sweep_cache(&dir, CACHE_MAX_AGE, CACHE_MAX_BYTES));
        }
        let client = match reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(TILE_TIMEOUT)
            .connect_timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => Some(c),
            Err(e) => {
                log::warn!("tile http client build failed: {e}");
                None
            }
        };
        Self::with_client(cache_dir, client)
    }

    fn with_client(cache_dir: PathBuf, client: Option<reqwest::blocking::Client>) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            cache_dir,
            client,
            textures: LruCache::new(NonZeroUsize::new(TEXTURE_CACHE_TILES).unwrap()),
            placeholder: None,
            pending: HashSet::new(),
            queue: VecDeque::new(),
            failed: HashMap::new(),
            net: Arc::new(NetGate::new()),
            rx,
            tx,
        }
    }

    /// Allow previously failed tiles to be requested again (call e.g. every
    /// 30 s if the network was down at startup).
    pub fn retry_failed(&mut self) {
        self.failed.clear();
    }

    /// Drop every decoded tile from memory (textures, in-flight bookkeeping,
    /// failure cooldowns) but leave the disk cache alone — used when the user
    /// clears map data, so nothing keeps painting from memory after the
    /// on-disk files are deleted. The next `paint` re-requests everything it
    /// needs.
    pub fn clear_in_memory(&mut self) {
        self.textures.clear();
        self.pending.clear();
        self.queue.clear();
        self.failed.clear();
        log::debug!("tile layer: in-memory tile cache cleared");
    }

    /// Record a tile as failed, stamping the time so `pump` can retry it
    /// after [`FAILED_RETRY`] rather than leaving a permanent blank.
    fn mark_failed(&mut self, key: TileKey) {
        self.failed.insert(key, Instant::now());
    }

    /// Nearest cached ancestor of `key` (up to [`ANCESTOR_MAX_LEVELS`]
    /// levels up), as `(texture, uv sub-rect)`. Used to keep the basemap
    /// continuous across zoom retiles instead of flashing placeholders.
    fn ancestor_texture(&mut self, key: TileKey) -> Option<(egui::TextureId, egui::Rect)> {
        for k in 1..=ANCESTOR_MAX_LEVELS {
            if key.z < k as u8 {
                break;
            }
            let anc = TileKey {
                z: key.z - k as u8,
                x: key.x >> k,
                y: key.y >> k,
            };
            if let Some(rect) = ancestor_uv(&key, &anc) {
                if let Some(handle) = self.textures.get(&anc) {
                    return Some((handle.id(), rect));
                }
            }
        }
        None
    }

    fn in_flight(&self) -> usize {
        self.pending.len().saturating_sub(self.queue.len())
    }

    fn ensure_placeholder(&mut self, ui: &egui::Ui) -> egui::TextureId {
        if self.placeholder.is_none() {
            // 16x16 two-tone checker: reads as "still loading".
            let mut px = Vec::with_capacity(16 * 16 * 4);
            for y in 0..16 {
                for x in 0..16 {
                    let on = (x / 4 + y / 4) % 2 == 0;
                    let c: [u8; 4] = if on {
                        [34, 37, 45, 255]
                    } else {
                        [26, 28, 34, 255]
                    };
                    px.extend_from_slice(&c);
                }
            }
            let img = egui::ColorImage::from_rgba_unmultiplied([16, 16], &px);
            let handle = ui.ctx().load_texture(
                "stormscope tile placeholder",
                img,
                egui::TextureOptions::LINEAR,
            );
            self.placeholder = Some(handle);
        }
        self.placeholder.as_ref().unwrap().id()
    }

    fn spawn(&mut self, key: TileKey) {
        let Some(client) = self.client.clone() else {
            self.pending.remove(&key);
            self.mark_failed(key);
            return;
        };
        let tx = self.tx.clone();
        let dir = self.cache_dir.clone();
        let net = Arc::clone(&self.net);
        let res = std::thread::Builder::new()
            .name(format!("tile-{}/{}/{}", key.z, key.x, key.y))
            .spawn(move || {
                let msg = fetch_tile(&client, &dir, key, &net);
                let _ = tx.send(msg);
            });
        if let Err(e) = res {
            log::warn!("tile {key:?}: spawn failed: {e}");
            self.pending.remove(&key);
            self.mark_failed(key);
        }
    }

    /// Paint tiles for the current view; call before the radar overlay.
    ///
    /// Never blocks: missing tiles are fetched on background threads and a
    /// placeholder is painted until the decoded texture arrives (the layer
    /// requests a repaint so the frame updates). The returned
    /// [`egui::Response`]'s `rect.min` is the viewport origin — screen
    /// position of view pixel `(x, y)` is `response.rect.min + (x, y)`.
    /// Ensure a set of tiles is loaded, returning each one's texture as soon
    /// as it exists in memory (`None` = queued/failed, try again next frame).
    ///
    /// Used by the 3D pane's map-textured ground, which needs the same tiles
    /// the basemap uses without painting a 2D viewport. Drains worker
    /// results, queues misses (disk cache first, then the shared network
    /// politeness gate) and starts workers — the same tail as `paint`, just
    /// batched once per frame instead of per tile.
    pub fn ensure_tiles(
        &mut self,
        ui: &egui::Ui,
        keys: &[TileKey],
    ) -> Vec<Option<egui::TextureId>> {
        // 1) Collect finished tiles (capped, like `paint`: a warm burst of
        // uploads in one frame is a visible hitch).
        const MAX_TEXTURE_UPLOADS: usize = 16;
        let mut drained = 0usize;
        while drained < MAX_TEXTURE_UPLOADS {
            let Ok(msg) = self.rx.try_recv() else { break };
            drained += 1;
            self.pending.remove(&msg.key);
            match msg.image {
                Some(image) => {
                    let name = format!("tile {}/{}/{}", msg.key.z, msg.key.x, msg.key.y);
                    let handle = ui
                        .ctx()
                        .load_texture(name, image, egui::TextureOptions::LINEAR);
                    self.textures.put(msg.key, handle);
                }
                None => {
                    self.mark_failed(msg.key);
                    log::warn!("tile {:?} failed", msg.key);
                }
            }
        }

        // 2) Resolve each requested key.
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(handle) = self.textures.get(key) {
                out.push(Some(handle.id()));
                continue;
            }
            if self.client.is_some()
                && !self.failed.contains_key(key)
                && !self.pending.contains(key)
            {
                self.pending.insert(*key);
                self.queue.push_back(*key);
            }
            out.push(None);
        }

        // 3) Failure cooldowns + start workers, exactly like `paint`.
        let now = Instant::now();
        self.failed.retain(|key, at| {
            if now.duration_since(*at) >= FAILED_RETRY {
                self.queue.push_back(*key);
                false
            } else {
                true
            }
        });
        while self.in_flight() < MAX_IN_FLIGHT {
            let Some(key) = self.queue.pop_front() else {
                break;
            };
            self.spawn(key);
        }
        if self.in_flight() > 0 || !self.queue.is_empty() {
            ui.ctx().request_repaint();
        }
        out
    }

    pub fn paint(&mut self, ui: &egui::Ui, view: &MapView) -> egui::Response {
        // 1) Collect finished tiles (disk/HTTP work happened off-thread).
        // Capped per frame: a warm startup can deliver ~150 tiles at once and
        // uploading them all in one frame causes a visible hitch.
        const MAX_TEXTURE_UPLOADS: usize = 16;
        let mut got_new_texture = false;
        let mut drained = 0usize;
        while drained < MAX_TEXTURE_UPLOADS {
            let Ok(msg) = self.rx.try_recv() else { break };
            drained += 1;
            self.pending.remove(&msg.key);
            match msg.image {
                Some(image) => {
                    let name = format!("tile {}/{}/{}", msg.key.z, msg.key.x, msg.key.y);
                    let handle = ui
                        .ctx()
                        .load_texture(name, image, egui::TextureOptions::LINEAR);
                    self.textures.put(msg.key, handle);
                    got_new_texture = true;
                }
                None => {
                    self.mark_failed(msg.key);
                    log::warn!("tile {:?} failed", msg.key);
                }
            }
        }

        // 2) Viewport rect: same origin every layer must use.
        // (`paint` takes `&egui::Ui` per the API contract, so the widget rect
        // widget rect via `interact` without advancing the layout cursor.)
        let origin = ui.cursor().min;
        let (w, h) = (view.width(), view.height());
        let rect = egui::Rect::from_min_size(origin, egui::vec2(w as f32, h as f32));
        let id = ui.make_persistent_id("stormscope.tiles");
        let response = ui.interact(rect, id, egui::Sense::hover());
        if w == 0 || h == 0 {
            return response;
        }

        // 3) Paint every visible tile; queue the missing ones.
        let placeholder_id = self.ensure_placeholder(ui);
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        let painter = ui.painter();
        let range = tile_range(view);
        let ts = TILE_PX * tile_scale(view);
        let (ox, oy) = view.origin_px();

        for key in range.keys() {
            let x0 = key.x as f64 * ts - ox;
            let y0 = key.y as f64 * ts - oy;
            let (x1, y1) = (x0 + ts, y0 + ts);
            if x1 <= 0.0 || x0 >= w as f64 || y1 <= 0.0 || y0 >= h as f64 {
                continue; // not visible this frame
            }
            let screen = egui::Rect::from_min_max(
                egui::pos2(origin.x + x0 as f32, origin.y + y0 as f32),
                egui::pos2(origin.x + x1 as f32, origin.y + y1 as f32),
            );
            if let Some(handle) = self.textures.get(&key) {
                painter.image(handle.id(), screen, uv, egui::Color32::WHITE);
            } else {
                // Ancestor fallback: zooming past a half-level retiles the
                // whole viewport, and waiting for fresh tiles blanks the map.
                // Instead, draw the nearest cached ancestor (parent, grand-
                // parent, ...) cropped to this tile's quadrant, so the basemap
                // stays continuous while the sharper tiles stream in.
                if let Some((id, anc_uv)) = self.ancestor_texture(key) {
                    painter.image(id, screen, anc_uv, egui::Color32::WHITE);
                } else {
                    painter.image(placeholder_id, screen, uv, egui::Color32::WHITE);
                }
                if self.client.is_some()
                    && !self.failed.contains_key(&key)
                    && !self.pending.contains(&key)
                {
                    self.pending.insert(key);
                    self.queue.push_back(key);
                }
            }
        }

        // 4) Re-queue tiles whose failure cooldown has expired, so a
        // transient 403/timeout isn't a permanent blank spot.
        let now = Instant::now();
        self.failed.retain(|key, at| {
            if now.duration_since(*at) >= FAILED_RETRY {
                self.queue.push_back(*key);
                false
            } else {
                true
            }
        });

        // 5) Start workers for queued tiles. No spawn pacing here: workers
        // hit the disk cache first (fast) and only the network path is gated
        // by NetGate — pacing spawns made warm caches trickly-slow.
        while self.in_flight() < MAX_IN_FLIGHT {
            let Some(key) = self.queue.pop_front() else {
                break;
            };
            self.spawn(key);
        }

        // 5) Keep frames coming while tiles are outstanding.
        if got_new_texture || self.in_flight() > 0 || !self.queue.is_empty() {
            ui.ctx().request_repaint();
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::{gate_latlon, lonlat_to_merc_px, merc_px_to_lonlat};

    fn view() -> MapView {
        MapView {
            center_lat: 35.3331,
            center_lon: -97.2778,
            zoom: 9.0,
            width_px: 1024.0,
            height_px: 768.0,
        }
    }

    #[test]
    fn tile_range_covers_every_viewport_corner() {
        let view = view();
        let range = tile_range(&view);
        assert!(!range.is_empty(), "viewport should need tiles");
        assert_eq!(range.z, 9);
        let (w, h) = (view.width() as f64, view.height() as f64);
        for (cx, cy) in [
            (0.0, 0.0),
            (w - 1e-6, 0.0),
            (0.0, h - 1e-6),
            (w - 1e-6, h - 1e-6),
            (w / 2.0, h / 2.0),
        ] {
            let covered = range.keys().any(|key| {
                tile_view_rect(key, &view)
                    .map(|r| cx >= r[0] && cx < r[2] && cy >= r[1] && cy < r[3])
                    .unwrap_or(false)
            });
            assert!(
                covered,
                "viewport point ({cx}, {cy}) is not covered by any tile"
            );
        }
        // Adjacent tiles must be exactly contiguous (no gaps/overlaps).
        let keys: Vec<TileKey> = range.keys().collect();
        let a = tile_view_rect(keys[0], &view).unwrap();
        let right = TileKey {
            z: keys[0].z,
            x: keys[0].x + 1,
            y: keys[0].y,
        };
        let b = tile_view_rect(right, &view).unwrap();
        assert!(
            (a[2] - b[0]).abs() < 1e-9,
            "horizontal tile gap: {:?} {:?}",
            a[2],
            b[0]
        );
        let down = TileKey {
            z: keys[0].z,
            x: keys[0].x,
            y: keys[0].y + 1,
        };
        let c = tile_view_rect(down, &view).unwrap();
        assert!((a[3] - c[1]).abs() < 1e-9, "vertical tile gap");
    }

    /// CRITICAL: the tile raster path and the radar raster path must produce
    /// the same pixel for the same lon/lat.
    #[test]
    fn tile_rect_and_view_mapping_agree_on_radar_site_pixel() {
        let view = view();
        let (lat, lon) = (35.3331, -97.2778); // radar site at the view center

        // Path A: shared MapView mapping (what the radar rasterizer uses).
        let (px, py) = view.lonlat_to_view_px(lon, lat);
        assert!((px - view.width() as f64 / 2.0).abs() < 1e-9);
        assert!((py - view.height() as f64 / 2.0).abs() < 1e-9);

        // Path B: tile-space math (integer tile indices -> screen rect).
        let range = tile_range(&view);
        let key = range
            .keys()
            .find(|k| {
                tile_view_rect(*k, &view)
                    .map(|r| px >= r[0] && px < r[2] && py >= r[1] && py < r[3])
                    .unwrap_or(false)
            })
            .expect("no tile contains the radar site");
        let rect = tile_view_rect(key, &view).unwrap();

        // Reconstruct the site's pixel from the tile rect + mercator position.
        let (mx, my) = lonlat_to_merc_px(lon, lat, view.zoom);
        let ts = rect[2] - rect[0]; // tile size in view px
        let px_from_tile = rect[0] + (mx - key.x as f64 * ts);
        let py_from_tile = rect[1] + (my - key.y as f64 * ts);
        assert!(
            (px - px_from_tile).abs() < 1e-9,
            "tile x path disagrees: {px} vs {px_from_tile}"
        );
        assert!(
            (py - py_from_tile).abs() < 1e-9,
            "tile y path disagrees: {py} vs {py_from_tile}"
        );

        // The tile's NW corner must decode to the same lon/lat through the
        // view mapping as through plain XYZ tile math (ties z, scale and
        // origin together).
        let (lon_a, lat_a) = view.view_px_to_lonlat(rect[0], rect[1]);
        let (lon_b, lat_b) =
            merc_px_to_lonlat(key.x as f64 * 256.0, key.y as f64 * 256.0, key.z as f64);
        assert!(
            (lon_a - lon_b).abs() < 1e-9,
            "tile NW lon mismatch: {lon_a} vs {lon_b}"
        );
        assert!(
            (lat_a - lat_b).abs() < 1e-9,
            "tile NW lat mismatch: {lat_a} vs {lat_b}"
        );
    }

    /// A geolocated synthetic gate projects to the same pixel no matter
    /// which layer asks — i.e. radar echo and basemap share one mapping.
    #[test]
    fn synthetic_gate_pixel_is_layer_independent() {
        let view = view();
        let radar_lat = 35.3331;
        let radar_lon = -97.2778;

        // Geolocate a gate east of the site (4/3 earth) ...
        let (glat, glon) = gate_latlon(radar_lat, radar_lon, 90.0, 0.5, 40_000.0);
        assert!(glon > radar_lon);

        // ... project via the shared view mapping (raster path) ...
        let (px, py) = view.lonlat_to_view_px(glon, glat);

        // ... and locate it purely through tile math (tile path).
        let ts = TILE_PX * tile_scale(&view);
        let (mx, my) = lonlat_to_merc_px(glon, glat, view.zoom);
        let tx = (mx / ts).floor();
        let ty = (my / ts).floor();
        let rect = tile_view_rect(
            TileKey {
                z: tile_zoom(&view),
                x: tx as u32,
                y: ty as u32,
            },
            &view,
        )
        .expect("gate must fall on-grid at this zoom");
        assert!(
            px >= rect[0] && px < rect[2] && py >= rect[1] && py < rect[3],
            "gate pixel ({px},{py}) outside its tile rect {rect:?}"
        );
        // Tile-local reconstruction matches to sub-nanopixel.
        assert!((rect[0] + (mx - tx * ts) - px).abs() < 1e-9);
        assert!((rect[1] + (my - ty * ts) - py).abs() < 1e-9);
        // Sanity: ~40 km east at zoom 9 is ~160 px right of center.
        let expected_px = view.width() as f64 / 2.0
            + 40_000.0 / crate::projection::meters_per_pixel(view.center_lat, view.zoom);
        assert!(
            (px - expected_px).abs() < 5.0,
            "px={px} expected≈{expected_px}"
        );
    }

    /// Ancestor UV math: a child tile must map to exactly its quadrant of the
    /// parent, and non-ancestors must be rejected.
    #[test]
    fn ancestor_uv_quadrants() {
        let z9 = |x, y| TileKey { z: 9, x, y };
        // Parent (z8) of child (9, 101, 199): 101>>1=50, 199>>1=99.
        let anc = TileKey { z: 8, x: 50, y: 99 };
        // Child is in the lower-right quadrant of that parent (odd/odd).
        let uv = ancestor_uv(&z9(101, 199), &anc).expect("should be a descendant");
        assert_eq!(uv.min.x, 0.5);
        assert_eq!(uv.min.y, 0.5);
        assert_eq!(uv.max.x, 1.0);
        assert_eq!(uv.max.y, 1.0);
        // Upper-left quadrant.
        let uv = ancestor_uv(&z9(100, 198), &anc).expect("should be a descendant");
        assert_eq!(
            (uv.min.x, uv.min.y, uv.max.x, uv.max.y),
            (0.0, 0.0, 0.5, 0.5)
        );
        // Two levels up: one sixteenth of the grand-parent's UV space.
        // (101,199) low-2-bits are (0b01, 0b11) -> col 1, row 3 of 4x4.
        let gp = TileKey { z: 7, x: 25, y: 49 };
        let uv = ancestor_uv(&z9(101, 199), &gp).expect("should be a grand-descendant");
        assert_eq!((uv.min.x, uv.min.y), (0.25, 0.75));
        assert_eq!((uv.max.x, uv.max.y), (0.5, 1.0));
        // Not an ancestor (different branch).
        assert!(ancestor_uv(&z9(101, 199), &TileKey { z: 8, x: 51, y: 99 }).is_none());
        // Same level is not an ancestor.
        assert!(ancestor_uv(&z9(101, 199), &z9(50, 99)).is_none());
    }

    #[test]
    fn tile_zoom_and_scale() {
        let mut v = view();
        assert_eq!(tile_zoom(&v), 9);
        assert!((tile_scale(&v) - 1.0).abs() < 1e-12);
        v.zoom = 8.4;
        assert_eq!(tile_zoom(&v), 8);
        assert!((tile_scale(&v) - 2f64.powf(0.4)).abs() < 1e-12);
        v.zoom = 8.5;
        assert_eq!(tile_zoom(&v), 9);
        // A key at the wrong zoom has no rect for this view.
        assert!(tile_view_rect(TileKey { z: 8, x: 0, y: 0 }, &v).is_none());
    }

    /// Smoke test: paint() works inside a real egui frame without a network
    /// client (placeholders only), never blocks, and the returned response
    /// rect is exactly the viewport.
    #[test]
    fn paint_offline_smoke() {
        let dir = std::env::temp_dir().join(format!("stormscope-tile-test-{}", std::process::id()));
        let mut layer = TileLayer::with_client(dir, None);
        let view = view();
        let mut rect = egui::Rect::NOTHING;
        egui::__run_test_ui(|ui| {
            let response = layer.paint(ui, &view);
            rect = response.rect;
            assert!(!response.clicked()); // hover-only sense
        });
        assert_eq!(rect.width(), view.width_px);
        assert_eq!(rect.height(), view.height_px);
        assert!(rect.min.x.is_finite() && rect.min.y.is_finite());
        // No client => nothing queued, no worker threads spawned.
        assert!(layer.pending.is_empty() && layer.queue.is_empty());
    }

    /// Provider circuit breaking: a provider answering 403/429 is skipped
    /// (until its cooldown expires), the last provider that worked is tried
    /// first, and a globally throttled state is reported only when *nothing*
    /// can be tried.
    #[test]
    fn provider_candidates_prefer_last_good_and_skip_blocked() {
        let now = 1_000_000u64;
        // Nothing blocked: last-good first, then the rest.
        assert_eq!(provider_candidates([0, 0], 0, now), vec![0, 1]);
        assert_eq!(provider_candidates([0, 0], 1, now), vec![1, 0]);
        // Wikimedia (index 0) is in cooldown: OSM is the only candidate.
        assert_eq!(provider_candidates([now + 1, 0], 0, now), vec![1]);
        // ...and it stays preferred after the cooldown expires.
        assert_eq!(provider_candidates([0, 0], 1, now), vec![1, 0]);
        // Both blocked => nothing to try (caller pauses).
        assert_eq!(
            provider_candidates([now + 1, now + 1], 0, now),
            Vec::<usize>::new()
        );
    }
}
