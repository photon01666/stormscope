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
    Status(String),
    Error(String),
}

/// Commands UI -> background data threads.
pub enum DataCmd {
    SetSite(String),
    LoadArchiveDate(chrono::NaiveDate),
    RefreshAlerts,
    Shutdown,
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
    /// changes — never every frame.
    pub radar_tex: Option<(egui::TextureHandle, u64)>,
    /// Last time the radar overlay was rasterized — throttles re-rasters
    /// during drags so panning stays smooth (the full-viewport splat is the
    /// app's most expensive operation).
    pub radar_last_raster: std::time::Instant,
    /// Signature awaiting a re-raster once the drag throttle expires.
    pub radar_pending: Option<u64>,
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
            radar_last_raster: std::time::Instant::now(),
            radar_pending: None,
        }
    }

    /// Select a radar site from the map (or controls) and tell the data
    /// thread to load it. Mirrors what the controls panel does on change.
    pub fn select_site(&mut self, id: &str) {
        if self.state.site_id == id {
            return;
        }
        self.state.site_id = id.to_string();
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
                DataMsg::Status(s) => self.state.status_text = s,
                DataMsg::Error(e) => {
                    log::warn!("data: {e}");
                    self.state.status_text = format!("error: {e}");
                }
            }
        }
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
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
