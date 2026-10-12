# stormscope — a RadarScope-inspired Linux radar app

Working title. A full-featured NEXRAD weather radar viewer for Linux built in
Rust with egui/eframe. Goal: give Linux users a fast, modern, RadarScope-like
experience using **only free/public data** (NOAA AWS Open Data + api.weather.gov).

## Stack

- Rust (stable, 2021 edition), `eframe`/`egui` **0.36** for the UI
- Data: **Unidata `unidata-nexrad-level2-chunks`** (primary, anonymous HTTPS —
  verified 200 today). NOTE: `noaa-nexrad-level2` 403s anonymous ListObjects/GET
  as of 2026-10-11, so it is fallback-only. NWS alerts via
  `https://api.weather.gov/alerts/active`
- Decoding: wrap the `nexrad-data` / `nexrad-decode` / `nexrad-model` crates
  (1.0.0-rc line, implements ICD 2620010H) rather than reimplementing the
  binary format. Verify against real data early; fall back to a hand-rolled
  parser only if they fail.
- Map: raster tiles (Carto `https://{s}.basemaps.cartocdn.com/dark_all/{z}/{x}/{y}.png`
  or OSM as fallback) fetched anonymously, cached on disk + in memory.
- Prior art worth reading (ideas only, do NOT copy code — license check first):
  `rustywx` crate / github.com/kerryhatcher/rustywx.

## Platform: Wayland / Hyprland + Vulkan (verified on this dev box)

This machine is Omarchy (Arch) running Hyprland on Wayland, so Wayland
support is a hard requirement, not a nice-to-have. Findings, verified against
the resolved crate sources on 2026-10-11:

- **eframe features are pinned explicitly** in `stormscope-app/Cargo.toml`:
  `default-features = false` + `["default_fonts", "wgpu", "wayland", "x11"]`.
  Reason: eframe 0.36's `default` = `[accesskit, default_fonts, links,
  wayland, web_screen_reader, wgpu, winit/default, x11]` — it ships BOTH the
  `wgpu` and `glow` (OpenGL) renderers. We drop `glow` so the app is
  wgpu-only (Vulkan on Linux); keeping `wayland` gives native Wayland
  windows (no XWayland round-trip) and `x11` stays as a fallback backend.
- **Vulkan comes via wgpu**: on Linux `wgpu` selects the Vulkan backend, so
  "use Vulkan" is satisfied without hand-rolling `ash`. Pin `wgpu = "30"` /
  `egui-wgpu = "0.36"` to match eframe's resolved wgpu line so there is a
  single GPU device/queue, not two competing contexts.
- **Hand the GPU device to the renderer**: `CreationContext.wgpu_render_state:
  Option<egui_wgpu::RenderState>` (confirmed present in eframe 0.36.2
  `epi.rs`). `StormApp` clones and stores it; the render workstream attaches
  its polar-warp pipeline to this device/queue instead of creating its own.
- **egui 0.36 layout API change**: `TopBottomPanel`/`SidePanel` no longer
  exist — use the unified `egui::Panel::top/bottom/left/right(...)` with
  `.show_inside(...)`; `CentralPanel` is unchanged. `App::ui(&mut self, ui:
  &mut Ui, frame)` is the frame callback (not `update`).
- HiDPI: Hyprland commonly runs fractional scale; egui handles device-pixel
  ratio, but confirm the radar overlay texture uses the render surface's
  physical size (not logical points) so gates don't blur under scaling.

## Crate layout

```
crates/
  radar-core/       # data model contract (DONE — treat as stable; extend additively)
  radar-decode/     # L2 file/bytes -> radar_core::Volume mapping
  radar-data/       # S3 fetch/cache/live poll + NWS alerts + site catalog
  radar-render/     # projection, colormaps, rasterization, egui painting, tiles
  stormscope-app/   # eframe app: state, timeline, panels, wiring (owned by lead)
```

## Cross-cutting conventions

- No blocking I/O or heavy decode on the UI thread. Background work runs on
  std threads (or a small pool) and communicates via `std::sync::mpsc` or
  shared `Arc<Mutex<_>>`/`arc-swap`. The app polls state each frame.
- Errors: `thiserror` in library crates, `anyhow` in the app and binaries.
- Logging: `log` facade; the app initializes `env_logger`. Log S3/decode
  failures at `warn!`, not `panic!`.
- All times UTC (`chrono`). All coordinates WGS84 degrees unless noted.
- Radar moments are stored as raw i16 DNs in `radar_core::MomentData`; convert
  to physical units with its `calibration`. DN 0 = no data (folding/threshold).
- Keep `cargo check --workspace` green at every hand-off.

## Key public APIs (contract)

### radar-decode
```rust
/// Decode a full gzipped NEXRAD Archive II volume file.
pub fn decode_volume_file(bytes: &[u8]) -> anyhow::Result<radar_core::Volume>;

/// Decode an *assembled* real-time volume (chunks concatenated in order,
/// without the archive CTM/header — see nexrad-data `volume::File` realtime
/// constructor).
pub fn decode_realtime_volume(bytes: &[u8]) -> anyhow::Result<radar_core::Volume>;
```

### radar-data
```rust
pub struct S3Source { /* anon reqwest client + cache dir */ }
impl S3Source {
    /// Newest-first listing of archive volume keys for site/date.
    pub fn list_archive(&self, site: &str, date: chrono::NaiveDate)
        -> anyhow::Result<Vec<ArchiveVolumeRef>>;
    pub fn fetch_archive(&self, r: &ArchiveVolumeRef)
        -> anyhow::Result<std::sync::Arc<radar_core::Volume>>;   // cache hit or download+decode

    /// Real-time: background poller emitting fully assembled volumes.
    pub fn start_realtime(&self, site: &str)
        -> anyhow::Result<std::sync::mpsc::Receiver<radar_core::Volume>>;
}

pub struct AlertSource { /* api.weather.gov */ }
impl AlertSource {
    /// Active alerts near a lat/lon (client-side distance filter ok).
    pub fn active_near(&self, lat: f64, lon: f64, radius_km: f64)
        -> anyhow::Result<Vec<radar_core::warnings::Warning>>;
}

/// Disk cache root under ~/.cache/stormscope, LRU-ish size cap (~2 GB).
```

### radar-render
```rust
/// View state shared by map + radar layers (Web Mercator, zoom 3..=12).
pub struct MapView { pub center_lat: f64, pub center_lon: f64, pub zoom: f64,
                     pub width_px: f32, pub height_px: f32 }

pub fn lonlat_to_merc_px(lon: f64, lat: f64, zoom: f64) -> (f64, f64);
pub fn merc_px_to_lonlat(x: f64, y: f64, zoom: f64) -> (f64, f64);

/// Standard NWS-ish color tables keyed by Moment (value -> RGBA).
pub struct ColorTable { /* ... */ }
impl ColorTable { pub fn for_moment(m: Moment) -> Self; pub fn map(&self, v: f32) -> [u8; 4]; }

/// Rasterize one sweep/moment into an RGBA overlay for the current view.
/// Uses 4/3-earth refraction model for gate -> lat/lon.
pub fn rasterize_sweep(sweep: &Sweep, moment: Moment, view: &MapView,
                       radar_lat: f64, radar_lon: f64, opacity: f32)
    -> radar_render::RadarImage;  // { width, height, rgba: Vec<u8> } covering the viewport

/// Tile layer: async-ish fetch (thread + channel), in-memory LRU of decoded
/// tile textures (via egui), painting helper:
pub struct TileLayer { /* ... */ }
impl TileLayer {
    pub fn new(cache_dir: std::path::PathBuf) -> Self;
    pub fn paint(&mut self, ui: &egui::Ui, view: &MapView) -> egui::Response;
}
```

Projection details: radar gates use spherical geometry with effective earth
radius 4/3·6371 km:
`h = sqrt(r² + Re² + 2·r·Re·sin(elev)) − Re`,
`d = Re·asin(r·cos(elev)/(Re+h))`, then great-circle march from the radar
site along the ray azimuth.

## Workstreams

### W1 — decoder (`crates/radar-decode`) [worker: decode]
- Wrap `nexrad-data` (+`decode` feature) & `nexrad-decode`/`nexrad-model`.
- Map message-31 data blocks (REF/VEL/SW/ZDR/PHI/KDP/RHO + legacy CZ/VR/SV…)
  into `radar_core` sweeps with correct scale/offset calibration.
- Volume start time, VCP, sequence number from metadata.
- Real-time chunk volume assembly support (`decode_realtime_volume`).
- **Acceptance**: `cargo test` decodes a real downloaded volume (test fixture
  fetched once into `tests/fixtures/`, gz bytes, <30 MB) and prints sane
  stats: sweep count, tilts, REF gate values near the radar (high dBZ) and
  plausible max dBZ. Handle multiple fixture volumes (a calm day and a
  severe-weather day if convenient).

### W2 — data layer (`crates/radar-data`) [worker: data]
- Anonymous S3 ListObjectsV2/GET over HTTPS (plain `reqwest`; no AWS SDK).
  `nexrad-data::aws` already does this — prefer wrapping it; verify it works
  with no credentials in env (watch out for `AWS_*` env leakage).
- Real-time poller: Unidata chunked scheme (`unidata-nexrad-level2-chunks`),
  assemble chunks in sequence order into a volume, decode, emit on a channel.
  Use `nexrad-data::aws::realtime` (it has `poll_chunks`) if viable.
- Disk cache: `~/.cache/stormscope/l2/<site>/<key>.gz` + a size cap sweeper.
- NWS alerts: `api.weather.gov` active alerts, filter by polygon centroid
  distance to the selected site; require a descriptive User-Agent header.
- **Site catalog**: fetch the full list from `https://api.weather.gov/radars`
  (or similar) and regenerate `radar-core/src/sites.rs` (keep the macro
  format), so ALL sites have correct id/lat/lon/elev. Keep `site()`/`nearest()`.
- **Acceptance**: integration test listing today's volumes for KTLX,
  downloading one, receiving it from `decode_volume_file` (path via
  radar-decode), and fetching + parsing active alerts near OKC.

### W3 — rendering (`crates/radar-render`) [worker: render]
- MapView + Mercator math + tests (round-trips, known values).
- ColorTables for REF (NWS dBZ ramp incl. below-zero colors), VEL (red/green
  ramp w/ 8-level steps like GR2), SW, ZDR, PHI, KDP, CC.
- `rasterize_sweep`: gate projection (4/3 earth), splat into RGBA viewport
  image; antialias-ish by painting a gate as a small disc sized to its
  screen-space footprint; skip DN 0; support partial opacity; supersampling
  optional.
- TileLayer: Carto dark_all with OSM fallback, on-disk tile cache, per-tile
  egui textures in an LRU, placeholder for missing tiles, painted to match
  MapView (correct sub-pixel alignment between tiles and radar overlay is
  REQUIRED — test with a known radar site/overlay alignment).
- **Acceptance**: unit tests for projection round-trips; an integration test
  (or a small `examples/` binary) that rasterizes a fixture sweep (coordinate
  with W1 for the fixture path) over tiles for a view centered on the radar
  and saves a PNG to `target/example-render.png` for visual inspection.

### W4 — app shell (`crates/stormscope-app`) [lead, in this pane]
- eframe app: central map canvas (tiles + radar overlay + warning polygons),
  left panel (site search/select, product buttons, tilt selector, opacity,
  loop controls), bottom timeline scrubber with per-frame ticks, status bar
  (site, VCP, scan time, latency, active alert counts).
- Animation loop: ring buffer of decoded volumes (~12 frames), play/pause,
  fps control (2–15), frame stepping, time labels.
- Live mode: background poller thread feeding the ring buffer; archive mode:
  date picker + volume list.
- Warning overlay: tornado (magenta/red), severe T-storm (yellow), flash
  flood (green) polygons; hover tooltip with headline; click -> details.
- Site catalog completion, keyboard shortcuts (space = play/pause, arrows =
  frame step, +/- zoom, m = mute alerts?), settings persistence.

## Status

- [x] Workspace scaffold + radar-core contract (lead)
- [ ] W1 decoder green on real fixture
- [ ] W2 data layer green on real endpoints
- [ ] W3 renderer green + example PNG
- [ ] W4 app shell integrated, live radar on screen
- [ ] Polish: warnings UX, multi-site mosaic, perf pass
