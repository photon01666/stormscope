//! The eframe App: owns AppState, spawns background data threads, polls
//! their channels each frame, and drives the UI.

use crate::state::{AppState, LoopState};
use eframe::egui;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

/// Messages from background data threads -> UI.
pub enum DataMsg {
    Volume(Arc<radar_core::Volume>),
    Alerts(Vec<radar_core::warnings::Warning>),
    /// A day's categorical SPC convective outlook (Day 1/2/3).
    Outlook(radar_core::outlooks::Outlook),
    Status(String),
    Error(String),
    /// Drop every ring frame (mode switch / archive window change).
    ClearFrames,
    /// Archive window position: `total` scans in the day, `[start, start+len)`
    /// currently loaded (indices into newest-first order).
    ArchiveInfo { total: usize, start: usize, len: usize },
    /// On-disk cache sizes in bytes, per section: `radar` is every downloaded
    /// volume (`l2/`), `tiles` the basemap cache (`tiles/`). The coordinator
    /// re-sends this about once a second so Settings stays live.
    CacheInfo { radar: u64, tiles: u64 },
}

/// Commands UI -> background data threads.
pub enum DataCmd {
    SetSite(String),
    LoadArchiveDate(String, chrono::NaiveDate),
    /// Load the newest ~1 hour of scans into the ring for immediate looping.
    LoadLastHour(String),
    /// Page the loaded archive day earlier (-1) or later (+1).
    ArchiveWindowShift(i32),
    /// Leave archive mode: clear the ring and refresh with the newest
    /// available scan immediately.
    EnterLive,
    RefreshAlerts,
    /// Fetch the SPC categorical outlook for a day (1..=3).
    LoadOutlook(u8),
    /// Delete every downloaded radar volume from disk (`l2/`).
    ClearRadar,
    /// Delete every cached basemap tile from disk (`tiles/`).
    ClearTiles,
    Shutdown,
}

/// Which cache section a pending "clear" is asking to delete.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ClearTarget {
    Radar,
    Tiles,
}

pub struct StormApp {
    pub state: AppState,
    pub tx: Sender<DataCmd>,
    pub rx: Receiver<DataMsg>,
    pub data: Arc<Mutex<SharedData>>,
    /// eframe's wgpu (Vulkan) render state — the render workstream attaches
    /// its polar-warp pipeline to this single GPU device/queue. `None` only
    /// if the wgpu feature isn't active (it is: see Cargo.toml).
    pub render_state: Option<egui_wgpu::RenderState>,
    /// Basemap tile layer (fetches + caches Carto/OSM tiles, paints under
    /// every other map layer). Fetches happen on background threads.
    pub tiles: radar_render::TileLayer,
    /// Cached radar overlay texture for the current (frame, moment, tilt,
    /// view, opacity) combination. Re-rasterized only when the signature
    /// changes — never every frame. Reused across re-rasters via
    /// `TextureHandle::set` (one GPU texture, not one per frame).
    pub radar_tex: Option<RadarTex>,
    /// Cached per-gate screen geometry for the current sweep layout + zoom.
    /// Gate geolocation is ~half the raster cost and depends only on the
    /// layout, site, and zoom — so pans, moment switches, Min dBZ tweaks and
    /// playback frames (same VCP) all reuse it.
    pub radar_geom: Option<radar_render::GateGeometry>,
    /// Persistent RGBA scratch buffer for the radar overlay (avoids an ~8 MB
    /// alloc per re-raster).
    pub radar_buf: Vec<u8>,
    /// Last time the radar overlay was rasterized — throttles re-rasters
    /// during drags so panning stays smooth (the full-viewport splat is the
    /// app's most expensive operation).
    pub radar_last_raster: std::time::Instant,
    /// Signature awaiting a re-raster once the drag throttle expires.
    pub radar_pending: Option<u64>,
    /// Last time the *view* changed (pan/zoom/resize) — a rapid repeat
    /// (wheel-zoom emits a burst of zoom steps) is treated as motion too, so
    /// gestures rasterize at half resolution and the full-res pass happens
    /// once when they stop. Keyed to view changes only: the raster scale is
    /// itself part of the raster signature, and updating this on every
    /// signature change would flap the scale forever.
    pub radar_view_change_at: std::time::Instant,
    /// View signature (zoom/center/size) the timestamp above refers to.
    pub radar_last_view_sig: u64,
    /// Derived storm tracks for the (prev, cur) frame pair at the current
    /// tilt: cached by signature, recomputed only when the frames change.
    pub tracks_cache: Option<(u64, Vec<radar_render::StormTrack>)>,
    /// Previous smoothed track set: fresh velocities blend 50/50 with these
    /// (matched by proximity), so cell motion — and marker ETAs — settle
    /// instead of jumping scan to scan.
    pub prev_tracks: Vec<radar_render::StormTrack>,
    /// Per-marker arrival estimates, aligned with `state.markers` by index
    /// (`None` = no cell incoming). Recomputed whenever tracks refresh.
    pub marker_etas: Vec<Option<MarkerEta>>,
    /// Cache section awaiting the user's "yes" in the clear-cache dialog.
    /// Set by the Settings menu, cleared when the dialog is dismissed.
    pub confirm_clear: Option<ClearTarget>,
    /// Auto-hide target for the controls sidebar (true = should be visible).
    /// Driven each frame from pointer position + `sidebar_pinned`; the slide
    /// animation itself is handled by egui's animation manager.
    pub sidebar_open: bool,
    /// High-res auto-scale currently applied to the sidebar (1.0 = none).
    /// Recomputed each frame from the monitor's physical height in
    /// `sidebar_overlay`; shown as a footer note while active.
    pub sidebar_scale: f32,
}

/// One marker's storm-arrival estimate: the earliest incoming cell.
pub struct MarkerEta {
    /// Minutes until the cell's closest approach.
    pub minutes: f64,
    /// Cell ground speed, mph (US convention, like the track labels).
    pub speed_mph: f64,
    /// Cell heading as a 16-wind compass point ("NE").
    pub compass: &'static str,
}

/// One cached radar overlay raster plus the view it was produced for, so a
/// drag can keep it georeferenced by translation instead of rubber-banding.
pub struct RadarTex {
    pub handle: egui::TextureHandle,
    pub sig: u64,
    /// Viewport origin (Mercator px) at raster time.
    pub ox: f64,
    pub oy: f64,
    /// Zoom at raster time (translation is only valid at equal zoom).
    pub zoom: f64,
}

/// Mutable shared state the coordinator thread writes and the UI reads.
#[derive(Default)]
pub struct SharedData {
    pub sites: Vec<&'static radar_core::sites::RadarSite>,
    pub tilts: Vec<f32>,
}

impl StormApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (tx_ui, rx_data): (Sender<DataMsg>, Receiver<DataMsg>) = std::sync::mpsc::channel();
        let (tx_data, rx_ui): (Sender<DataCmd>, Receiver<DataCmd>) = std::sync::mpsc::channel();

        // Background data thread. Real fetch/decode wired in once the
        // data/decode workstreams land; until then it just serves status.
        std::thread::Builder::new()
            .name("data".into())
            .spawn(move || crate::background::run(rx_ui, tx_ui))
            .expect("spawn data thread");

        let data = Arc::new(Mutex::new(SharedData {
            sites: radar_core::sites::SITES.iter().collect(),
            tilts: vec![0.5, 0.9, 1.3, 1.8, 2.4, 3.1, 4.0, 5.1, 6.4, 8.0, 10.0, 12.0, 19.5],
        }));

        let mut state = AppState::default();
        if let Some(fonts) = Self::configure_fonts(&cc.egui_ctx) {
            cc.egui_ctx.set_fonts(fonts);
        }

        // Basemap tiles share the disk cache root with radar volumes.
        let cache_root = radar_data::DiskCache::default_root();
        let tiles = radar_render::TileLayer::new(cache_root.join("tiles"));

        Self {
            state,
            tx: tx_data,
            rx: rx_data,
            data,
            render_state: cc.wgpu_render_state.clone(),
            tiles,
            radar_tex: None,
            radar_geom: None,
            radar_buf: Vec::new(),
            radar_last_raster: std::time::Instant::now(),
            radar_pending: None,
            radar_view_change_at: std::time::Instant::now(),
            radar_last_view_sig: 0,
            tracks_cache: None,
            prev_tracks: Vec::new(),
            marker_etas: Vec::new(),
            confirm_clear: None,
            sidebar_open: false,
            sidebar_scale: 1.0,
        }
    }

    /// Select a radar site from the map (or controls) and tell the data
    /// thread to load it. Mirrors what the controls panel does on change.
    pub fn select_site(&mut self, id: &str) {
        if self.state.site_id == id {
            return;
        }
        self.state.site_id = id.to_string();
        // Tracks from the old site must not smooth into the new site's.
        self.prev_tracks.clear();
        self.marker_etas.clear();
        let _ = self.tx.send(crate::app::DataCmd::SetSite(id.to_string()));
    }

    fn configure_fonts(ctx: &egui::Context) -> Option<egui::FontDefinitions> {
        let mut fonts = egui::FontDefinitions::default();
        // Prefer a mono font for readouts if present on the system.
        for candidate in ["DejaVuSansMono", "LiberationMono", "monospace"] {
            if let Some(font) =
                find_system_font(candidate)
            {
                fonts
                    .families
                    .entry(egui::FontFamily::Monospace)
                    .or_default()
                    .insert(0, font);
                let _ = ctx;
                return Some(fonts);
            }
        }
        Some(fonts)
    }

    /// Drain messages from the background thread.
    fn pump(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                DataMsg::Volume(v) => self.state.push_volume(v),
                DataMsg::Alerts(a) => self.state.alerts = a,
                DataMsg::Outlook(o) => {
                    let label = o
                        .issue
                        .map(|t| crate::timefmt::fmt_central(&t))
                        .unwrap_or_else(|| "?".into());
                    log::debug!("outlook day {} loaded (issued {label})", o.day);
                    self.state.outlook = Some(o);
                }
                DataMsg::Status(s) => self.state.status_text = s,
                DataMsg::Error(e) => {
                    log::warn!("data: {e}");
                    self.state.status_text = format!("error: {e}");
                }
                DataMsg::ClearFrames => {
                    self.state.ring.clear();
                    self.state.playhead = 0;
                    self.radar_tex = None;
                    self.radar_pending = None;
                    // Tracks are georeferenced to the old site/mode; don't
                    // let the smoothing blend across the switch.
                    self.prev_tracks.clear();
                    self.marker_etas.clear();
                }
                DataMsg::ArchiveInfo { total, start, len } => {
                    self.state.archive_total = total;
                    self.state.archive_start = start;
                    self.state.archive_len = len;
                }
                DataMsg::CacheInfo { radar, tiles } => {
                    self.state.cache_radar_bytes = Some(radar);
                    self.state.cache_tile_bytes = Some(tiles);
                }
            }
        }
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        // Typing in a text field (e.g. the marker-name popup) must not drive
        // the map: Space would toggle play/pause and ArrowLeft/Right would
        // step frames, so the radar appears to "bounce" and can land on a
        // frame without the current tilt/moment (radar vanishes).
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Space)) {
            self.state.loop_state = match self.state.loop_state {
                LoopState::Playing => LoopState::Paused,
                LoopState::Paused => LoopState::Playing,
            };
        }
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
            self.state.step_forward();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
            self.state.step_back();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && self.state.measure_mode {
            self.state.measure_mode = false;
            self.state.measure_drag = None;
        }
    }
}

/// Minimal system font lookup (Linux fontconfig paths).
fn find_system_font(name: &str) -> Option<String> {
    let dirs = ["/usr/share/fonts", "/usr/local/share/fonts"];
    let want = name.to_lowercase();
    for dir in dirs {
        if let Ok(walker) = std::fs::read_dir(dir) {
            for entry in walker.flatten() {
                let path = entry.path();
                let fname = path.file_name()?.to_string_lossy().to_lowercase();
                if fname.contains(&want) && fname.ends_with(".ttf") {
                    return Some(path.to_string_lossy().to_string());
                }
            }
        }
    }
    None
}

impl eframe::App for StormApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pump();
        self.handle_input(ui.ctx());

        let dt = ui.input(|i| i.stable_dt);
        self.state.tick(std::time::Duration::from_secs_f32(dt));

        // Keep viewport size in sync so rasterization matches the panel.
        let avail = ui.available_size();
        self.state.view.width_px = avail.x;
        self.state.view.height_px = avail.y;

        crate::ui::layout(ui, self);
    }
}
