//! The eframe App: owns AppState, spawns background data threads, polls
//! their channels each frame, and drives the UI.

use crate::brief::LookPointSample;
use crate::state::{AiChatMessage, AiRole, AppState, LoopState};
use eframe::egui;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

/// Messages from background data threads -> UI.
pub enum DataMsg {
    Volume(Arc<radar_core::Volume>),
    /// Alerts near the selected site (drawn on the map).
    Alerts(Vec<radar_core::warnings::Warning>),
    /// Alerts that apply exactly to the selected station (fast lane).
    AlertsFast(Vec<radar_core::warnings::Warning>),
    /// Every active alert nationwide (alerts browser only).
    AllAlerts(Vec<radar_core::warnings::Warning>),
    /// Preliminary Local Storm Reports (realtime spotter/public reports).
    Reports(Vec<radar_data::lsrs::Lsr>),
    /// Active SPC Mesoscale Discussions.
    Mcds(Vec<radar_data::mcds::Mcd>),
    /// NWS gridpoint forecast for a requested point.
    Forecast(radar_core::forecast::Forecast),
    /// Latest observation from the nearest reporting station.
    Observation(radar_core::forecast::Observation),
    /// Rain/snow totals for the stations around a requested point.
    Totals(Vec<radar_data::StationTotals>),
    /// A day's categorical SPC convective outlook (Day 1/2/3).
    Outlook(radar_core::outlooks::Outlook),
    /// Forecaster discussion text for one outlook day.
    OutlookDiscussion(u8, String),
    Status(String),
    Error(String),
    /// Drop every ring frame (mode switch / archive window change).
    ClearFrames,
    /// Warning-archive backfill status (crawler progress).
    ArchiveCrawl(radar_data::warnings_archive::CrawlStatus),
    /// Bulk radar load progress (counts only; the UI shows a pill while a
    /// live prime refill or archive window streams).
    LoadProgress {
        label: String,
        done: u32,
        total: u32,
    },
    /// A storm-relative velocity volume for one motion setting, built off the
    /// UI thread (the data thread's derive task).
    Srm {
        key: u64,
        volume: Arc<radar_core::Volume>,
    },
    /// The newest MRMS multi-radar composite: the CONUS-wide image plus the
    /// sharp window around the radar.
    Mrms {
        mosaics: Vec<Arc<radar_data::mrms::Mosaic>>,
    },
    /// GOES satellite frames (GIBS WMS images): CONUS-wide and the sharp
    /// window around the radar.
    Goes {
        images: Vec<Arc<radar_data::goes::GoesImage>>,
    },
    /// The AI meteorologist's reply, or the error to show in its window.
    AiBrief(Result<String, String>),
    /// Result of the Settings popup's connectivity test.
    AiTest(Result<String, String>),
    /// One historical warning's raw NWS product text (`None` = the fetch
    /// failed; an Error message accompanies it).
    ArchiveText {
        id: String,
        text: Option<String>,
    },
    /// Historical warnings for one Central day (the alerts archive tab).
    ArchiveWarnings {
        date: chrono::NaiveDate,
        warnings: Vec<radar_data::warnings_archive::HistoricWarning>,
    },
    /// Archive window position: `total` scans in the day, `[start, start+len)`
    /// currently loaded (indices into newest-first order).
    ArchiveInfo {
        total: usize,
        start: usize,
        len: usize,
    },
    /// On-disk cache sizes in bytes, per section: `radar` is every downloaded
    /// volume (`l2/`), `tiles` the basemap cache (`tiles/`). The coordinator
    /// re-sends this about once a second so Settings stays live.
    CacheInfo {
        radar: u64,
        tiles: u64,
    },
}

/// Commands UI -> background data threads.
pub enum DataCmd {
    SetSite(String),
    /// Load one archive listing day. `Some(minute)` targets the time of day
    /// (Central, minutes since midnight) — the loaded window covers it;
    /// `None` starts from the newest scan of that day.
    LoadArchiveDate(String, chrono::NaiveDate, Option<u32>),
    /// Turn the (rate-limited, newest-first) warning-archive backfill on or
    /// off. Persisted; only the user asks for it.
    SetArchiveCrawl(bool),
    /// Load the historical warnings for one Central day (cache-first, one
    /// or two small day-file requests; never polled).
    LoadArchiveWarnings(chrono::NaiveDate),
    /// Fetch one archived warning's NWS product text (cache-first; the
    /// request happens only when the user opens that warning's details).
    LoadArchiveText(String),
    /// Load the newest ~1 hour of scans into the ring for immediate looping.
    LoadLastHour(String),
    /// Page the loaded archive day earlier (-1) or later (+1).
    ArchiveWindowShift(i32),
    /// Leave archive mode: clear the ring and refresh with the newest
    /// available scan immediately.
    /// Leave archive/replay for live radar of the given site — the UI's site
    /// travels with the command so the prime and poller can never diverge
    /// from what the app is watching (a silent desync dropped every volume).
    EnterLive(String),
    /// Derive storm-relative velocity for one motion setting (the SRM
    /// product's inputs change with the sliders, so this can't be attached
    /// at decode like VIL/TOPS/MEHS).
    DeriveSrm {
        key: u64,
        volume: Arc<radar_core::Volume>,
        toward_deg: f32,
        speed_kt: f32,
    },
    /// Fetch the newest MRMS multi-radar composite (the overlay's data).
    LoadMrms,
    /// Fetch the latest GOES frame for the chosen layer/satellite.
    LoadGoes {
        layer: radar_data::goes::GoesLayer,
        west_satellite: bool,
    },
    RefreshAlerts,
    /// Fetch the SPC categorical outlook for a day (1..=3).
    LoadOutlook(u8, radar_core::outlooks::OutlookType),
    /// Rain/snow totals for the stations around (lat, lon).
    LoadTotals(f64, f64),
    /// Fetch the forecaster discussion text for a day (1..=3).
    LoadOutlookDiscussion(u8),
    /// Fetch the NWS gridpoint forecast for a coordinate.
    LoadForecast(f64, f64),
    /// Ask the AI meteorologist for a brief or an answer (spawned on its own
    /// thread so a slow model never holds up the data coordinator).
    AiBrief {
        config: radar_data::ai::AiConfig,
        system: String,
        history: Vec<(String, String)>,
        user: String,
        session: String,
    },
    /// Connectivity test for the Settings popup.
    AiTest {
        config: radar_data::ai::AiConfig,
    },
    /// Delete every downloaded radar volume from disk (`l2/`).
    ClearRadar,
    /// Delete every cached basemap tile from disk (`tiles/`).
    ClearTiles,
    Shutdown,
}

/// Id of the synthetic test polygon `STORMSCOPE_DEBUG_WARNING` injects.
const DEBUG_WARNING_ID: &str = "DEBUG-TEST-OVERLAY";

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
    /// eframe's wgpu (Vulkan) render state: any custom GPU work (pipelines
    /// drawing the radar overlay, etc.) shares this single device/queue.
    /// `None` only if the wgpu feature isn't active (it is: see Cargo.toml).
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
    /// When the in-flight SRM derivation was requested (staleness watchdog).
    pub srm_requested_at: Option<std::time::Instant>,
    /// Cached MRMS textures, one per window (Arc identity + egui handle).
    pub mrms_tex: Vec<(usize, egui::TextureHandle)>,
    /// Cached GOES textures, one per window.
    pub goes_tex: Vec<(usize, egui::TextureHandle)>,
    /// Geometry key of the cross-section curtain currently uploaded (the
    /// pane rebuilds it when the azimuth, product or volume changes).
    pub view3d_section_key: Option<u64>,
    /// Previous smoothed track set: fresh velocities blend 50/50 with these
    /// (matched by proximity), so cell motion — and marker ETAs — settle
    /// instead of jumping scan to scan.
    pub prev_tracks: Vec<radar_render::StormTrack>,
    /// Per-marker arrival estimates, aligned with `state.markers` by index
    /// (`None` = no cell incoming). Recomputed whenever tracks refresh.
    pub marker_etas: Vec<Option<MarkerEta>>,
    /// GPU resources of the 3D volume pane, created on first use (see
    /// [`crate::render3d`]).
    pub view3d: Option<crate::render3d::Volume3d>,
    /// Last frame's rects of the two panes (screen coords): the pane control
    /// windows anchor themselves near the matching pane's bottom-right.
    pub map_rect_at: Option<egui::Rect>,
    pub view3d_rect_at: Option<egui::Rect>,
    /// When the 3D camera last moved (drag/wheel/WASD). The map's raster gate
    /// treats this as motion: while the user is flying the 3D pane, the
    /// radar raster must not fire on every playback tick and spike the UI
    /// thread — it settles as soon as the 3D camera does.
    pub view3d_interact_at: std::time::Instant,
    /// Cache section awaiting the user's "yes" in the clear-cache dialog.
    /// Set by the Settings menu, cleared when the dialog is dismissed.
    pub confirm_clear: Option<ClearTarget>,
    /// Which top-center badge pill the pointer is over (brightens its
    /// border instead of drawing the hover ring).
    pub top_badge_hovered: Option<crate::state::BadgeKind>,
    /// The last warning click on the map: where it landed, the stack it hit
    /// (alert ids, in the order they were offered) and which member was
    /// chosen — so clicking the same spot again steps to the next one.
    pub map_alert_cycle: Option<(egui::Pos2, Vec<String>, usize)>,
    /// Frame-cadence telemetry (see `record_frame_stats`): back-to-back
    /// frames and their UI work, plus the longest sleep between frames —
    /// "the whole session felt half-rate" is only diagnosable with numbers,
    /// so they are always collected.
    pub frame_count: u32,
    pub frame_sum_ms: f64,
    pub frame_max_ms: f64,
    pub frame_gap_max_ms: f64,
    pub frame_stat_at: std::time::Instant,
    pub last_frame_at: Option<std::time::Instant>,
    /// Radar rasters since the last stats line (churn corroboration).
    pub radar_raster_count: u32,
    /// Cumulative 3D pane passes seen at the last stats line (telemetry).
    pub view3d_count_seen: u64,
    /// Recent raster timestamps for the churn window (see
    /// `ui::map::prune_raster_times`).
    pub radar_raster_times: std::collections::VecDeque<std::time::Instant>,
    pub radar_churn_warn_at: std::time::Instant,
    /// Last observed monitor scale (`native_pixels_per_point`), logged when it
    /// changes: a display reconfigure is the event behind the incident.
    pub last_native_ppp: Option<f32>,
    /// Throttle for the debug-level GPU texture accounting log (memory
    /// pressure in this app shows up as resident textures/GPU memory).
    pub tex_stats_at: std::time::Instant,
    /// Throttle for the periodic `malloc_trim` (see `main.rs` tuning).
    pub last_trim: std::time::Instant,
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

        // Background data thread: fetches, decodes and polls; results are
        // delivered to the UI over the message channel below.
        std::thread::Builder::new()
            .name("data".into())
            .spawn(move || crate::background::run(rx_ui, tx_ui))
            .expect("spawn data thread");

        let data = Arc::new(Mutex::new(SharedData {
            sites: radar_core::sites::SITES.iter().collect(),
            tilts: vec![
                0.5, 0.9, 1.3, 1.8, 2.4, 3.1, 4.0, 5.1, 6.4, 8.0, 10.0, 12.0, 19.5,
            ],
        }));

        // A window's resize band is only a few pixels wide by default; on a
        // HiDPI display it is easy to miss it and have the press fall through
        // to the map. Give every window a comfortably grabby edge.
        cc.egui_ctx.all_styles_mut(|style| {
            style.interaction.resize_grab_radius_side = 14.0;
            style.interaction.resize_grab_radius_corner = 18.0;
        });

        let state = AppState::default();
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
            srm_requested_at: None,
            mrms_tex: Vec::new(),
            goes_tex: Vec::new(),
            view3d_section_key: None,
            prev_tracks: Vec::new(),
            marker_etas: Vec::new(),
            view3d: None,
            map_rect_at: None,
            view3d_rect_at: None,
            view3d_interact_at: std::time::Instant::now() - std::time::Duration::from_secs(60),
            top_badge_hovered: None,
            map_alert_cycle: None,
            frame_count: 0,
            frame_sum_ms: 0.0,
            frame_max_ms: 0.0,
            frame_gap_max_ms: 0.0,
            frame_stat_at: std::time::Instant::now(),
            last_frame_at: None,
            radar_raster_count: 0,
            view3d_count_seen: 0,
            radar_raster_times: std::collections::VecDeque::new(),
            radar_churn_warn_at: std::time::Instant::now(),
            last_native_ppp: None,
            confirm_clear: None,
            tex_stats_at: std::time::Instant::now(),
            last_trim: std::time::Instant::now(),
            sidebar_open: false,
            sidebar_scale: 1.0,
        }
    }

    /// Leave archive/replay and return to live radar: the mode flips,
    /// playback stops, and the data thread clears the ring and refills from
    /// the newest scans. Shared by the timeline's "GO LIVE" pill and the L
    /// key.
    pub fn snap_to_live(&mut self) {
        self.state.mode = crate::state::Mode::Live;
        self.state.loop_state = crate::state::LoopState::Paused;
        self.state.archive_jump = None;
        let _ = self
            .tx
            .send(crate::app::DataCmd::EnterLive(self.state.site_id.clone()));
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
        self.tracks_cache = None;
        self.marker_etas.clear();
        let _ = self.tx.send(crate::app::DataCmd::SetSite(id.to_string()));
    }

    /// Feed the frame-cadence counters and emit the periodic stats line.
    ///
    /// `work` is how long the frame body ran; the *gap* since the previous
    /// frame is tracked separately, because idle sleep between timer wakes
    /// must not read as frame cost. Together with the raster count (churn
    /// corroboration) and the monitor scale (what a display reconfigure
    /// changes), these are the numbers a "the whole session feels half-rate"
    /// report needs — logged every 15 s at debug level.
    fn record_frame_stats(&mut self, work: std::time::Duration, ctx: &egui::Context) {
        let now = std::time::Instant::now();
        let work_ms = work.as_secs_f64() * 1000.0;
        let gap_ms = self
            .last_frame_at
            .map(|prev| (now - prev).as_secs_f64() * 1000.0);
        self.last_frame_at = Some(now);
        self.frame_gap_max_ms = self.frame_gap_max_ms.max(gap_ms.unwrap_or(0.0));
        // Back-to-back frames (no sleep between them) are the ones whose cost
        // matters; a longer gap is idle or a timer firing.
        if gap_ms.is_some_and(|g| g <= 50.0) {
            self.frame_count += 1;
            self.frame_sum_ms += work_ms;
            self.frame_max_ms = self.frame_max_ms.max(work_ms);
        }

        if let (Some(prev), Some(cur)) = (self.last_native_ppp, ctx.native_pixels_per_point())
            && (prev - cur).abs() > 0.01
        {
            log::info!("display scale changed: {prev:.2} -> {cur:.2}");
        }
        self.last_native_ppp = ctx.native_pixels_per_point();

        let elapsed = self.frame_stat_at.elapsed();
        if elapsed >= std::time::Duration::from_secs(15) {
            self.frame_stat_at = now;
            if log::log_enabled!(log::Level::Debug) {
                let secs = elapsed.as_secs_f64().max(1e-6);
                let avg = if self.frame_count > 0 {
                    self.frame_sum_ms / self.frame_count as f64
                } else {
                    0.0
                };
                let (v3_renders, v3_last) = match &self.view3d {
                    Some(v) => (v.render_count, v.render_count - self.view3d_count_seen),
                    None => (0, 0),
                };
                self.view3d_count_seen = v3_renders;
                log::debug!(
                    "frames: {:.1} active fps (ui work avg {avg:.1} ms, max {:.1} ms) | idle gap max {:.1} ms | radar rasters {} | 3d passes {v3_last} | screen {:?} ppp {:.2}",
                    self.frame_count as f64 / secs,
                    self.frame_max_ms,
                    self.frame_gap_max_ms,
                    self.radar_raster_count,
                    ctx.content_rect().size(),
                    ctx.pixels_per_point(),
                );
            }
            self.frame_count = 0;
            self.frame_sum_ms = 0.0;
            self.frame_max_ms = 0.0;
            self.frame_gap_max_ms = 0.0;
            self.radar_raster_count = 0;
        }
    }

    fn configure_fonts(ctx: &egui::Context) -> Option<egui::FontDefinitions> {
        let mut fonts = egui::FontDefinitions::default();
        // Prefer a mono font for readouts if present on the system.
        for candidate in ["DejaVuSansMono", "LiberationMono", "monospace"] {
            if let Some(font) = find_system_font(candidate) {
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
                DataMsg::Volume(v) => {
                    let site = v.meta.site_id.clone();
                    self.state.push_volume(v);
                    // A LOOK that needed a closer radar waits for its first
                    // volume, then runs against that volume.
                    if self.state.ai_pending_site.as_deref() == Some(site.as_str()) {
                        self.state.ai_pending_site_volumes =
                            self.state.ai_pending_site_volumes.saturating_add(1);
                        if self.state.ai_pending_site_volumes < 2 {
                            // Two scans are what makes a motion vector; the
                            // backfill usually delivers the previous one within
                            // seconds of the newest.
                            if let Some(looking) = self.state.ai_looking_at.take() {
                                self.state.ai_looking_at =
                                    Some(format!("{looking} — waiting for the previous scan"));
                            }
                        } else {
                            let points = self.state.ai_pending_look.take().unwrap_or_default();
                            self.state.ai_pending_site = None;
                            self.state.ai_pending_site_at = None;
                            self.state.ai_pending_site_volumes = 0;
                            if !points.is_empty() {
                                let samples: Vec<LookPointSample> = points
                                    .into_iter()
                                    .map(|(label, lat, lon, radius_mi)| LookPointSample {
                                        label,
                                        lat,
                                        lon,
                                        radius_mi,
                                    })
                                    .collect();
                                let names = samples
                                    .iter()
                                    .map(|p| p.label.clone())
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                log::info!("AI look continues on {site}: {names}");
                                self.state.ai_looking_at = Some(format!("{names} (from {site})"));
                                self.state.ai_pending = true;
                                self.send_ai(self.state.ai_question_ctx.clone(), samples);
                            }
                        }
                    }
                }
                DataMsg::Alerts(a) => {
                    self.state.alerts_slow = a;
                    self.state.alerts = crate::state::merge_alerts(
                        &self.state.alerts_slow,
                        &self.state.alerts_fast,
                    );
                    self.inject_debug_warning();
                }
                DataMsg::Reports(v) => self.state.lsrs = v,
                DataMsg::Mcds(v) => self.state.mcds = v,
                DataMsg::AlertsFast(a) => {
                    self.state.alerts_fast = a;
                    self.state.alerts = crate::state::merge_alerts(
                        &self.state.alerts_slow,
                        &self.state.alerts_fast,
                    );
                }
                DataMsg::AllAlerts(a) => self.state.all_alerts = a,
                DataMsg::Observation(o) => {
                    // Freshness is stamped by the forecast message that shares
                    // the same request; keep the older stamp if one is set.
                    self.state
                        .forecast_fetched_at
                        .get_or_insert_with(std::time::Instant::now);
                    log::debug!(
                        "observation {} ({}) at {:?}: {:.1}C {}",
                        o.station_id,
                        o.station_name,
                        o.timestamp,
                        o.temperature_c.unwrap_or(f32::NAN),
                        o.text
                    );
                    self.state.observation = Some(o);
                }
                DataMsg::Totals(v) => {
                    if let Some((lat, lon, _)) = self.state.totals_requested.take() {
                        self.state.totals_target = Some((lat, lon));
                    }
                    self.state.totals = v;
                    self.state.totals_fetched_at = Some(std::time::Instant::now());
                    log::debug!("totals loaded from {} stations", self.state.totals.len());
                    self.pump_ai_data();
                }
                DataMsg::Forecast(f) => {
                    log::debug!(
                        "forecast for {} ({} {}): {} periods",
                        f.place,
                        f.office,
                        f.grid,
                        f.periods.len()
                    );
                    self.state.forecast = Some(f);
                    self.state.forecast_fetched_at = Some(std::time::Instant::now());
                    // The request is done: without this the dedupe stamp would
                    // block the 15-minute refresh forever.
                    self.state.forecast_requested = None;
                    self.pump_ai_data();
                }
                DataMsg::OutlookDiscussion(day, text) => {
                    log::debug!("outlook day {day} discussion: {} bytes", text.len());
                    self.state.outlook_discussion = Some((day, text));
                }
                DataMsg::Outlook(o) => {
                    let label = o
                        .issue
                        .map(|t| crate::timefmt::fmt_central(&t))
                        .unwrap_or_else(|| "?".into());
                    log::debug!("outlook day {} loaded (issued {label})", o.day);
                    self.state.outlook = Some(o);
                    self.pump_ai_data();
                }
                DataMsg::Status(s) => self.state.status_text = s,
                DataMsg::Error(e) => {
                    log::warn!("data: {e}");
                    self.state.status_text = format!("error: {e}");
                }
                DataMsg::ClearFrames => {
                    self.state.ring.clear();
                    self.state.ring_bytes = 0;
                    self.state.playhead = 0;
                    self.radar_tex = None;
                    self.radar_pending = None;
                    // Tracks are georeferenced to the old site/mode; don't
                    // let the smoothing blend across the switch.
                    self.prev_tracks.clear();
                    self.marker_etas.clear();
                }
                DataMsg::ArchiveCrawl(status) => {
                    self.state.archive_crawl = Some(status);
                }
                DataMsg::LoadProgress { label, done, total } => {
                    self.state.load_progress = Some(crate::state::LoadProgress {
                        label,
                        done,
                        total,
                        at: std::time::Instant::now(),
                    });
                }
                DataMsg::Mrms { mosaics } => {
                    self.state.mrms_requested_at = None;
                    if let Some(wide) = mosaics.first() {
                        log::info!(
                            "MRMS mosaic: {} images, widest {}x{} at {} ({} dBZ max)",
                            mosaics.len(),
                            wide.width,
                            wide.height,
                            wide.time.format("%H:%MZ"),
                            wide.values.iter().copied().fold(f32::MIN, f32::max)
                        );
                        let at = wide.time.format("%H:%MZ").to_string();
                        self.state.status_text = format!("MRMS mosaic · {at}");
                    }
                    self.state.mrms = Some((mosaics, std::time::Instant::now()));
                    self.pump_ai_data();
                }
                DataMsg::Goes { images } => {
                    self.state.goes_pending = None;
                    if let Some(first) = images.first() {
                        log::info!(
                            "GOES {}: {} images ({}x{} wide) from {} at {}",
                            first.layer.label(),
                            images.len(),
                            first.width,
                            first.height,
                            if first.west_satellite {
                                "GOES-West"
                            } else {
                                "GOES-East"
                            },
                            first.time.format("%H:%MZ")
                        );
                        self.state.status_text = format!(
                            "GOES {} · {}",
                            first.layer.label(),
                            first.time.format("%H:%MZ")
                        );
                    }
                    self.state.goes = Some((images, std::time::Instant::now()));
                    self.pump_ai_data();
                }
                DataMsg::AiBrief(result) => match result {
                    Ok(reply) => {
                        let (prose, action, look, data) = crate::ai_actions::split_reply(&reply);
                        let can_look = self.state.ai_look_depth < 2;
                        let looked = can_look && look.is_some_and(|look| self.ai_look(look));
                        if !looked {
                            let can_fetch = self.state.ai_data_depth == 0;
                            let fetched =
                                can_fetch && data.is_some_and(|data| self.ai_request_data(data));
                            if !fetched {
                                self.ai_finish(prose, action);
                            }
                        }
                    }
                    Err(error) => {
                        log::warn!("AI meteorologist: {error}");
                        self.state.ai_pending = false;
                        self.state.ai_look_depth = 0;
                        self.state.ai_looking_at = None;
                        self.state.ai_pending_look = None;
                        self.state.ai_pending_site = None;
                        self.state.ai_pending_site_at = None;
                        self.state.ai_data_depth = 0;
                        self.state.ai_pending_data.clear();
                        self.state.ai_pending_data_at = None;
                        self.state.ai_error = None;
                        let hint = if error.to_ascii_lowercase().contains("unavailable") {
                            " — OpenCode Go may be having a temporary outage; press Retry."
                        } else {
                            ""
                        };
                        self.ai_note(format!("Request failed: {error}{hint}"));
                    }
                },
                DataMsg::AiTest(result) => {
                    self.state.ai_test_pending = false;
                    self.state.ai_test_status = Some(match result {
                        Ok(reply) => format!(
                            "Connected — {} replied: {}",
                            self.state.ai_model,
                            reply.chars().take(80).collect::<String>()
                        ),
                        Err(error) => format!("Failed: {error}"),
                    });
                }
                DataMsg::Srm { key, volume } => {
                    if self.state.srm_pending == Some(key) {
                        self.state.srm_pending = None;
                    }
                    let build_ms = volume
                        .sweeps
                        .first()
                        .map(|s| {
                            s.moment(radar_core::Moment::StormRelativeVelocity)
                                .is_some()
                        })
                        .unwrap_or(false);
                    log::debug!(
                        "SRM volume {key:x}: {} sweeps, velocity present: {build_ms}",
                        volume.sweeps.len()
                    );
                    self.state.srm_cache = Some((key, volume));
                }
                DataMsg::ArchiveText { id, text } => {
                    self.state.archive_text_requested.remove(&id);
                    self.state.archive_texts.insert(id, text);
                }
                DataMsg::ArchiveWarnings { date, warnings } => {
                    self.state.archive_warning_view =
                        warnings.iter().map(|w| w.as_warning()).collect();
                    self.state.archive_warnings = warnings;
                    self.state.archive_warnings_date = Some(date);
                    self.state.archive_warnings_requested = None;
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

        // Pending archive jump (set by clicking a historical warning): once
        // the ring holds a frame near the warning's time, park the playhead
        // there — alongside it the archive date/time it was loaded for.
        if let Some((target, armed)) = self.state.archive_jump {
            if armed.elapsed() > std::time::Duration::from_secs(60) {
                self.state.archive_jump = None;
            } else if let Some((idx, delta)) = self
                .state
                .ring
                .iter()
                .enumerate()
                .map(|(i, f)| (i, (f.volume_start - target).num_seconds().abs()))
                .min_by_key(|&(_, d)| d)
            {
                // Within 12 minutes of the warning's issuance (a scan or
                // two before/after is the right picture of the warning).
                if delta <= 12 * 60 {
                    self.state.playhead = idx;
                    self.state.archive_jump = None;
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
                LoopState::Playing => {
                    // Stopping means "show me now": jump to the newest scan,
                    // same as the timeline's Pause button.
                    self.state.playhead = self.state.ring.len().saturating_sub(1);
                    LoopState::Paused
                }
                LoopState::Paused => LoopState::Playing,
            };
        }
        if ctx.input(|i| i.key_pressed(egui::Key::L)) {
            // Same as the timeline's "GO LIVE" pill.
            self.snap_to_live();
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

    /// Screenshot aid for the overlay layers: `STORMSCOPE_DEBUG_WARNING=1`
    /// drops a **clearly labeled test polygon** around the selected site, so
    /// the warning layers (map polygons, the 3D walls, the browser rows) can
    /// be eyeballed and screenshotted on a clear day. Same spirit as
    /// `STORMSCOPE_3D_OPEN` — and, like it, never left on for a real session:
    /// the label says TEST, not a real warning.
    fn inject_debug_warning(&mut self) {
        if std::env::var_os("STORMSCOPE_DEBUG_WARNING").is_none() {
            return;
        }
        if self.state.alerts.iter().any(|w| w.id == DEBUG_WARNING_ID) {
            return;
        }
        let Some(site) = radar_core::sites::site(&self.state.site_id) else {
            return;
        };
        let (lat, lon) = (site.latitude, site.longitude);
        let d = 0.30; // ~33 km on a side: a storm-scale box
        let w = radar_core::warnings::Warning {
            id: DEBUG_WARNING_ID.into(),
            event: "TEST overlay (debug)".into(),
            severity: "Unknown".into(),
            urgency: "Unknown".into(),
            expires: chrono::Utc::now() + chrono::Duration::minutes(30),
            office: "DEBUG".into(),
            headline: "synthetic overlay for screenshots".into(),
            description: String::new(),
            instruction: String::new(),
            area: "test area".into(),
            polygon: Some(vec![vec![
                (lon - d, lat - d),
                (lon + d, lat - d),
                (lon + d, lat + d),
                (lon - d, lat + d),
            ]]),
        };
        self.state.alerts_fast.push(w.clone());
        self.state.alerts.push(w);
        log::info!(
            "STORMSCOPE_DEBUG_WARNING: test overlay polygon injected at {}",
            self.state.site_id
        );
    }

    /// Keep the MRMS mosaic fresh while its layer is on: fetch the newest
    /// composite on first enable and every ten minutes after (the product
    /// updates every two, but the overlay is context, not a warning feed).
    /// Build a fresh snapshot and ask the AI. `question` = None writes the
    /// marker brief; Some(_) answers the spotter's question against the same
    /// data. One request chain at a time (a LOOK round may add one more call).
    pub fn ai_request(&mut self, question: Option<String>) {
        if self.state.ai_pending {
            return;
        }
        let asked = question.filter(|q| !q.trim().is_empty());
        let shown = asked.clone().unwrap_or_else(|| {
            format!(
                "Write a brief for my {} marker{}.",
                self.state.markers.len(),
                if self.state.markers.len() == 1 {
                    ""
                } else {
                    "s"
                }
            )
        });
        self.ai_push(AiRole::User, shown);
        self.state.ai_question_ctx = asked.clone();
        self.state.ai_look_depth = 0;
        self.state.ai_looking_at = None;
        self.state.ai_pending_look = None;
        self.state.ai_pending_site = None;
        self.state.ai_pending_site_at = None;
        self.state.ai_pending_site_volumes = 0;
        self.state.ai_data_depth = 0;
        self.state.ai_pending_data.clear();
        self.state.ai_pending_data_at = None;
        self.state.ai_last_look.clear();
        self.send_ai(asked, Vec::new());
    }

    /// Re-send the last question after a failure, without duplicating it in
    /// the transcript.
    pub fn ai_retry(&mut self) {
        if self.state.ai_pending {
            return;
        }
        while matches!(self.state.ai_messages.last(), Some(m) if m.role == AiRole::Note) {
            self.state.ai_messages.pop();
        }
        // The question lives in the transcript (which survives restarts), so a
        // Retry after relaunch re-asks the same thing instead of writing a
        // fresh brief.
        let question = self
            .state
            .ai_messages
            .iter()
            .rev()
            .find(|m| m.role == AiRole::User)
            .map(|m| m.text.clone())
            .filter(|text| !text.starts_with("Write a brief for my "));
        self.state.ai_last_question = question.clone();
        self.state.ai_look_depth = 0;
        self.state.ai_looking_at = None;
        self.state.ai_pending_look = None;
        self.state.ai_pending_site = None;
        self.state.ai_pending_site_at = None;
        self.state.ai_pending_site_volumes = 0;
        self.state.ai_data_depth = 0;
        self.state.ai_pending_data.clear();
        self.state.ai_pending_data_at = None;
        self.send_ai(question, Vec::new());
    }

    fn ai_push(&mut self, role: AiRole, text: String) {
        self.state.ai_messages.push(AiChatMessage { role, text });
        if self.state.ai_messages.len() > 60 {
            let cut = self.state.ai_messages.len() - 60;
            self.state.ai_messages.drain(..cut);
        }
        self.state.persist_ai_chat();
    }

    fn ai_note(&mut self, text: String) {
        self.ai_push(AiRole::Note, text);
    }

    /// The first half of a LOOK round: sample the places the model named,
    /// centre the map on the first one, and ask again with the extra data.
    /// Returns false when the request had no usable points.
    fn ai_look(&mut self, look: crate::ai_actions::LookRequest) -> bool {
        let points: Vec<LookPointSample> = look
            .points
            .iter()
            .take(3)
            .filter(|p| {
                p.lat.is_finite()
                    && p.lon.is_finite()
                    && (-85.0..=85.0).contains(&p.lat)
                    && (-180.0..=180.0).contains(&p.lon)
            })
            .map(|p| LookPointSample {
                label: p
                    .label
                    .clone()
                    .filter(|l| !l.trim().is_empty())
                    .unwrap_or_else(|| format!("{:.2}, {:.2}", p.lat, p.lon)),
                lat: p.lat,
                lon: p.lon,
                radius_mi: p.radius_mi.unwrap_or(20.0).clamp(1.0, 60.0),
            })
            .collect();
        if points.is_empty() {
            return false;
        }
        let names = points
            .iter()
            .map(|p| p.label.clone())
            .collect::<Vec<_>>()
            .join(", ");
        log::info!("AI meteorologist looks at: {names}");
        // Fit the view to the whole searched area (plus its radius), not an
        // arbitrary zoom: the user must be able to see what the model saw.
        let (center_lat, center_lon, zoom) = self.fit_look_view(&points);
        let action = crate::ai_actions::MapAction {
            center_lat: Some(center_lat),
            center_lon: Some(center_lon),
            zoom: Some(zoom),
            product: look.product.clone(),
            ..Default::default()
        };
        let _ = crate::ai_actions::apply(&action, &mut self.state);
        self.state.ai_actions = Some(format!("looking at {names}"));

        // A station 140 mi away sees this area at 15k ft through a wide
        // beam. If another NEXRAD is materially closer, switch to it first
        // and run the look when its first volume lands.
        if let Some(best) = self.closest_site_for(&points) {
            let current_id = self.state.site_id.clone();
            if best != current_id {
                log::info!("AI look: switching {current_id} -> {best} for {names}");
                self.state.ai_pending_look = Some(
                    points
                        .iter()
                        .map(|p| (p.label.clone(), p.lat, p.lon, p.radius_mi))
                        .collect(),
                );
                self.state.ai_pending_site = Some(best.clone());
                self.state.ai_pending_site_at = Some(std::time::Instant::now());
                self.state.ai_looking_at = Some(format!("{names} (nearest radar {best})"));
                self.state.ai_pending = true;
                self.state.ai_error = None;
                self.ai_note(format!(
                    "Switching to {best} — the closest radar to {names} — for a sharper look."
                ));
                self.state.ai_look_depth = 1;
                // The question is about current weather; a site switch loads
                // live data, so leave archive mode with it.
                self.state.mode = crate::state::Mode::Live;
                self.state.loop_state = crate::state::LoopState::Paused;
                self.select_site(&best);
                return true;
            }
        }

        self.state.ai_looking_at = Some(names);
        self.state.ai_look_depth = 1;
        self.state.ai_pending = true;
        self.state.ai_error = None;
        let question = self.state.ai_question_ctx.clone();
        self.send_ai(question, points);
        true
    }

    fn last_look_samples(&self) -> Vec<LookPointSample> {
        self.state
            .ai_last_look
            .iter()
            .map(|(label, lat, lon, radius_mi)| LookPointSample {
                label: label.clone(),
                lat: *lat,
                lon: *lon,
                radius_mi: *radius_mi,
            })
            .collect()
    }

    /// Fetch whatever the model asked for and re-ask once it lands. Returns
    /// false when the request named nothing new (the caller then answers
    /// normally).
    fn ai_request_data(&mut self, request: crate::ai_actions::DataRequest) -> bool {
        let mut waiting: Vec<String> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        if request.mrms == Some(true) && self.state.mrms.is_none() {
            self.state.show_mrms = true;
            waiting.push("mrms".into());
            notes.push("MRMS".into());
        }
        if request.goes == Some(true) && self.state.goes.is_none() {
            self.state.show_goes = true;
            waiting.push("goes".into());
            notes.push("GOES".into());
        }
        if let Some(point) = &request.forecast {
            self.state.forecast = None;
            self.state.observation = None;
            self.state.forecast_point = Some((point.lat, point.lon));
            self.state.forecast_requested = Some((point.lat, point.lon));
            let _ = self.tx.send(DataCmd::LoadForecast(point.lat, point.lon));
            waiting.push("forecast".into());
            notes.push("NWS forecast".into());
        }
        if let Some(point) = &request.totals {
            self.state.totals.clear();
            self.state.totals_target = Some((point.lat, point.lon));
            self.state.totals_requested = Some((point.lat, point.lon, std::time::Instant::now()));
            let _ = self.tx.send(DataCmd::LoadTotals(point.lat, point.lon));
            waiting.push("totals".into());
            notes.push("station totals".into());
        }
        if let Some(spc) = &request.spc {
            let day = spc.day.unwrap_or(1).clamp(1, 3);
            let ty = match spc.kind.as_deref().map(str::to_ascii_lowercase).as_deref() {
                Some("tornado") => radar_core::outlooks::OutlookType::Tornado,
                Some("wind") => radar_core::outlooks::OutlookType::Wind,
                Some("hail") => radar_core::outlooks::OutlookType::Hail,
                _ => radar_core::outlooks::OutlookType::Categorical,
            };
            let outdated = self
                .state
                .outlook
                .as_ref()
                .is_none_or(|o| o.day != day || o.ty != ty);
            if outdated {
                self.state.outlook = None;
                self.state.outlook_day = day;
                self.state.outlook_type = ty;
                let _ = self.tx.send(DataCmd::LoadOutlook(day, ty));
                waiting.push("spc".into());
                notes.push(format!("SPC day {day} {}", ty.label()));
            }
        }
        if waiting.is_empty() {
            return false;
        }
        self.state.ai_data_depth = 1;
        self.state.ai_pending_data = waiting;
        self.state.ai_pending_data_at = Some(std::time::Instant::now());
        self.state.ai_pending = true;
        self.state.ai_looking_at = Some(format!("loading {}", notes.join(", ")));
        self.ai_note(format!("Fetching {} for the analysis…", notes.join(", ")));
        true
    }

    /// Re-ask when every requested layer has landed (or the wait timed out).
    fn pump_ai_data(&mut self) {
        if self.state.ai_pending_data.is_empty() {
            return;
        }
        let timed_out = self
            .state
            .ai_pending_data_at
            .is_some_and(|at| at.elapsed() > std::time::Duration::from_secs(45));
        let missing: Vec<String> = self
            .state
            .ai_pending_data
            .iter()
            .filter(|layer| !self.ai_data_ready(layer))
            .cloned()
            .collect();
        if !missing.is_empty() && !timed_out {
            return;
        }
        self.state.ai_pending_data.clear();
        self.state.ai_pending_data_at = None;
        self.state.ai_looking_at = None;
        self.state.ai_data_depth = 1;
        if !missing.is_empty() {
            self.ai_note(format!(
                "Some data did not load ({}); asking with what is available.",
                missing.join(", ")
            ));
        }
        self.state.ai_pending = true;
        let question = self.state.ai_question_ctx.clone();
        let look = self.last_look_samples();
        self.send_ai(question, look);
    }

    fn ai_data_ready(&self, layer: &str) -> bool {
        match layer {
            "mrms" => self.state.mrms.is_some(),
            "goes" => self.state.goes.is_some(),
            "forecast" => self.state.forecast.is_some(),
            "totals" => !self.state.totals.is_empty(),
            "spc" => self.state.outlook.is_some(),
            _ => true,
        }
    }

    /// The NEXRAD whose beam is materially closer to the area than the
    /// selected one (or None when the current site is already the closest).
    fn closest_site_for(&self, points: &[LookPointSample]) -> Option<String> {
        let (mut lat, mut lon, mut n) = (0.0f64, 0.0f64, 0.0f64);
        for point in points {
            lat += point.lat;
            lon += point.lon;
            n += 1.0;
        }
        if n == 0.0 {
            return None;
        }
        let (lat, lon) = (lat / n, lon / n);
        let current = radar_core::sites::site(&self.state.site_id)?;
        let current_dist =
            radar_render::surface_distance_m(current.longitude, current.latitude, lon, lat);
        let best = radar_core::sites::SITES.iter().min_by(|a, b| {
            let da = radar_render::surface_distance_m(a.longitude, a.latitude, lon, lat);
            let db = radar_render::surface_distance_m(b.longitude, b.latitude, lon, lat);
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })?;
        let best_dist = radar_render::surface_distance_m(best.longitude, best.latitude, lon, lat);
        // Only switch when it is clearly worth reloading: inside radar range
        // and at least 30 %/40 mi closer than what is on screen.
        let range = 250.0 * 1609.344;
        if best.id != current.id
            && best_dist < range
            && best_dist + 40.0 * 1609.344 < current_dist
            && best_dist < current_dist * 0.7
        {
            Some(best.id.to_string())
        } else {
            None
        }
    }

    /// Abandon a LOOK whose radar switch never delivered a volume.
    fn pump_ai_site(&mut self) {
        let Some(at) = self.state.ai_pending_site_at else {
            return;
        };
        if at.elapsed() < std::time::Duration::from_secs(30) {
            return;
        }
        let site = self.state.ai_pending_site.take().unwrap_or_default();
        self.state.ai_pending_site_at = None;
        let points = self.state.ai_pending_look.take().unwrap_or_default();
        self.state.ai_pending_site_volumes = 0;
        if self.state.site_id == site && self.state.current().is_some() && !points.is_empty() {
            // The scan loaded but the backfill never produced a second one:
            // answer with what is there and say motion is missing.
            let samples: Vec<LookPointSample> = points
                .into_iter()
                .map(|(label, lat, lon, radius_mi)| LookPointSample {
                    label,
                    lat,
                    lon,
                    radius_mi,
                })
                .collect();
            self.state.ai_looking_at = Some(format!("{site} (only one scan — no motion vectors)"));
            self.state.ai_pending = true;
            self.ai_note(format!(
                "Only one {site} scan was available, so there is no radar motion yet."
            ));
            self.send_ai(self.state.ai_question_ctx.clone(), samples);
            return;
        }
        self.state.ai_looking_at = None;
        self.state.ai_pending = false;
        self.ai_note(format!(
            "Could not load {site} for a closer look; press Retry."
        ));
    }

    /// A zoom that fits every searched area (centre + radius) into the map.
    fn fit_look_view(&self, points: &[LookPointSample]) -> (f64, f64, f64) {
        let (mut min_lat, mut max_lat) = (f64::MAX, f64::MIN);
        let (mut min_lon, mut max_lon) = (f64::MAX, f64::MIN);
        for point in points {
            let dlat = point.radius_mi / 69.0;
            let dlon = point.radius_mi / (69.0 * point.lat.to_radians().cos().max(0.2));
            min_lat = min_lat.min(point.lat - dlat);
            max_lat = max_lat.max(point.lat + dlat);
            min_lon = min_lon.min(point.lon - dlon);
            max_lon = max_lon.max(point.lon + dlon);
        }
        let center_lat = (min_lat + max_lat) * 0.5;
        let center_lon = (min_lon + max_lon) * 0.5;
        let span_m = ((max_lat - min_lat) * 110_574.0)
            .max((max_lon - min_lon) * 111_320.0 * center_lat.to_radians().cos());
        let extent = f64::from(
            self.state
                .view
                .width_px
                .min(self.state.view.height_px)
                .max(240.0),
        ) * 0.75;
        let zoom = (3..=12)
            .rev()
            .find(|z| radar_render::meters_per_pixel(center_lat, f64::from(*z)) * extent >= span_m)
            .unwrap_or(3) as f64;
        (center_lat, center_lon, zoom)
    }

    /// Display a final reply (after any LOOK round) and apply its map move.
    fn ai_finish(&mut self, prose: String, action: Option<crate::ai_actions::MapAction>) {
        self.state.ai_pending = false;
        self.state.ai_look_depth = 0;
        self.state.ai_looking_at = None;
        if !prose.trim().is_empty() {
            self.ai_push(AiRole::Assistant, prose.clone());
        }
        self.state.ai_report = Some(prose);
        self.state.ai_generated_at = Some(std::time::Instant::now());
        self.state.ai_actions = None;
        if let Some(action) = action {
            let summary = crate::ai_actions::apply(&action, &mut self.state);
            if !summary.is_empty() {
                log::info!("AI view: {summary}");
                self.state.ai_actions = Some(summary);
            }
        }
    }

    /// Build the snapshot (optionally with the LOOK samples), compose the
    /// prompts and send the request. Callers own the pending flags.
    fn send_ai(&mut self, question: Option<String>, look: Vec<LookPointSample>) {
        if self.state.markers.is_empty() {
            self.state.ai_pending = false;
            self.ai_note("Place at least one marker on the map first.".into());
            return;
        }
        let Some(site) = radar_core::sites::site(&self.state.site_id) else {
            self.state.ai_pending = false;
            self.ai_note("No radar site is selected.".into());
            return;
        };
        let Some(volume) = self.state.current().map(|f| Arc::clone(&f.volume)) else {
            self.state.ai_pending = false;
            self.ai_note("No radar volume is loaded yet.".into());
            return;
        };
        if !self.state.ai_config().is_configured() {
            self.state.ai_settings_open = true;
            self.state.ai_pending = false;
            self.ai_note("Add your OpenCode Go API key in Settings → AI meteorologist.".into());
            return;
        }
        // The map computes tracks while drawing, but an AI question can
        // arrive before the first draw (especially right after a site
        // switch). Derive from the two newest scans here if needed.
        let tracks_owned: Vec<radar_render::StormTrack> =
            if let Some((_, tracks)) = &self.tracks_cache {
                tracks.clone()
            } else if self.state.ring.len() >= 2 {
                let n = self.state.ring.len();
                let prev = &self.state.ring[n - 2];
                let cur = &self.state.ring[n - 1];
                radar_render::compute_tracks(
                    &prev.volume,
                    &cur.volume,
                    self.state.tilt_deg,
                    site.latitude,
                    site.longitude,
                )
            } else {
                Vec::new()
            };
        let scan_interval_min = if self.state.ring.len() >= 2 {
            let n = self.state.ring.len();
            Some(
                (self.state.ring[n - 1].volume_start - self.state.ring[n - 2].volume_start)
                    .num_minutes()
                    .max(1),
            )
        } else {
            None
        };
        let tracks: &[radar_render::StormTrack] = &tracks_owned;
        let mrms = self.state.mrms.as_ref().map(|(m, _)| m.as_slice());
        let goes = self.state.goes.as_ref().map(|(i, _)| i.as_slice());
        let srm = self.state.srm_cache.as_ref().map(|(_, v)| v.as_ref());
        let brief = crate::brief::build(&crate::brief::BriefInputs {
            site,
            mode: match self.state.mode {
                crate::state::Mode::Live => "live",
                crate::state::Mode::Archive => "archive",
            },
            volume: &volume,
            min_dbz: self.state.min_dbz,
            markers: &self.state.markers,
            marker_etas: &self.marker_etas,
            tracks,
            alerts: &self.state.alerts,
            outlook: self.state.outlook.as_ref(),
            mcds: &self.state.mcds,
            lsrs: &self.state.lsrs,
            forecast: self.state.forecast.as_ref(),
            forecast_point: self.state.forecast_point,
            forecast_age_min: self
                .state
                .forecast_fetched_at
                .map(|t| t.elapsed().as_secs() / 60),
            observation: self.state.observation.as_ref(),
            totals: &self.state.totals,
            mrms,
            view_moment: self.state.moment,
            view_tilt_deg: self.state.tilt_deg,
            view_center: (self.state.view.center_lat, self.state.view.center_lon),
            view_zoom: self.state.view.zoom,
            view3d_open: self.state.view3d_open,
            show_melting_layer: self.state.show_melting_layer,
            look_points: &look,
            srm_volume: srm,
            goes,
            scan_interval_min,
            now: chrono::Utc::now(),
        });

        // Real chat turns: the transcript becomes role messages, and the
        // current question is restated in the final user message.
        let mut history: Vec<(String, String)> = self
            .state
            .ai_messages
            .iter()
            .filter(|m| m.role != AiRole::Note)
            .map(|m| {
                (
                    if m.role == AiRole::User {
                        "user".to_string()
                    } else {
                        "assistant".to_string()
                    },
                    m.text.chars().take(1200).collect::<String>(),
                )
            })
            .collect();
        if history.last().is_some_and(|(role, _)| role == "user") {
            history.pop();
        }
        if !look.is_empty() {
            self.state.ai_last_look = look
                .iter()
                .map(|p| (p.label.clone(), p.lat, p.lon, p.radius_mi))
                .collect();
        }
        let keep = history.len().saturating_sub(8);
        let history: Vec<(String, String)> = history[keep..].to_vec();

        let mut user = brief.user.clone();
        if !look.is_empty() {
            let names = look
                .iter()
                .map(|p| format!("{} ({:.0} mi radius)", p.label, p.radius_mi))
                .collect::<Vec<_>>()
                .join(", ");
            user.push_str(&format!(
                "\n\nYou asked to look at: {names}. The app sampled those AREAS and put them \
                 under \"requested_samples\": each has the centre column plus `area` with the \
                 strongest echo, its distance/direction, % of the disk with >=20 dBZ, the nearest \
                 echo and any warnings. Answer from the area, not a single gate."
            ));
        }
        let asked_text = question
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "Write a brief for my {} marker(s).",
                    self.state.markers.len()
                )
            });
        user.push_str(&format!(
            "\n\nThe Spotter's latest message: {asked_text}\n\
             Answer it from this snapshot. Above all, SHOW it: include a MAP directive that puts \
             the evidence on screen (focus the cell/couplet/hail core, switch product or tilt, \
             open the 3D pane or a cross-section, show the melting layer, mark what matters). \
             Keep it under 150 words unless the data demands more, keep the units, and still end \
             with the \"Watch for:\" line."
        ));

        let session = format!("stormscope-{}", chrono::Utc::now().timestamp_millis());
        if self
            .tx
            .send(DataCmd::AiBrief {
                config: self.state.ai_config(),
                system: brief.system,
                history,
                user,
                session,
            })
            .is_err()
        {
            self.state.ai_pending = false;
            self.ai_note("The data thread is not running.".into());
            return;
        }
        self.state.ai_pending = true;
        self.state.ai_error = None;
        self.state.ai_snapshot = Some(brief.label);
        self.state.ai_last_question = question.map(|q| q.trim().to_string());
    }

    /// Send a tiny prompt to check the endpoint, model and key.
    pub fn ai_test(&mut self) {
        if self.state.ai_test_pending {
            return;
        }
        if !self.state.ai_config().is_configured() {
            self.state.ai_test_status =
                Some("Add the API key first (or use a local endpoint).".into());
            return;
        }
        if self
            .tx
            .send(DataCmd::AiTest {
                config: self.state.ai_config(),
            })
            .is_ok()
        {
            self.state.ai_test_pending = true;
            self.state.ai_test_status = Some("Testing the endpoint…".into());
        }
    }

    fn pump_mrms(&mut self) {
        if !self.state.show_mrms {
            return;
        }
        let want = radar_core::sites::site(&self.state.site_id)
            .map(|s| radar_data::local_overlay_box(s.latitude, s.longitude));
        let fresh = self.state.mrms.as_ref().is_some_and(|(m, at)| {
            at.elapsed() < std::time::Duration::from_secs(600)
                && m.last().is_some_and(|near| {
                    want.is_none_or(|w| {
                        (near.south - w[0]).abs() < 0.2
                            && (near.west - w[1]).abs() < 0.2
                            && (near.north - w[2]).abs() < 0.2
                            && (near.east - w[3]).abs() < 0.2
                    })
                })
        });
        if fresh {
            return;
        }
        // A mosaic covering the wrong window (site switch) or a failed fetch
        // must not loop: back off between tries, on top of the request gate.
        if self
            .state
            .mrms
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() < std::time::Duration::from_secs(30))
        {
            return;
        }
        if self
            .state
            .mrms_requested_at
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(120))
        {
            return;
        }
        self.state.mrms_requested_at = Some(std::time::Instant::now());
        let _ = self.tx.send(DataCmd::LoadMrms);
    }

    /// Keep the GOES satellite overlay fresh while it is on: fetch on first
    /// enable, on a layer/satellite change, and every ten minutes after.
    fn pump_goes(&mut self) {
        if !self.state.show_goes {
            return;
        }
        let Some(site) = radar_core::sites::site(&self.state.site_id) else {
            return;
        };
        let west = site.longitude < -105.0;
        let bbox = radar_data::local_overlay_box(site.latitude, site.longitude);
        let wanted = (self.state.goes_layer, west);
        let current = self.state.goes.as_ref().is_some_and(|(images, at)| {
            images.first().is_some_and(|img| {
                (img.layer, img.west_satellite) == wanted
                    && at.elapsed() < std::time::Duration::from_secs(600)
                    && images.last().is_some_and(|near| {
                        (near.south - bbox[0]).abs() < 0.2
                            && (near.west - bbox[1]).abs() < 0.2
                            && (near.north - bbox[2]).abs() < 0.2
                            && (near.east - bbox[3]).abs() < 0.2
                    })
            })
        });
        if current {
            return;
        }
        if let Some(pending) = self.state.goes_pending {
            // A different layer/satellite supersedes the in-flight request;
            // otherwise wait for it (with a staleness watchdog).
            let stale = self
                .state
                .goes_requested_at
                .is_some_and(|at| at.elapsed() > std::time::Duration::from_secs(180));
            if pending == wanted && !stale {
                return;
            }
        }
        self.state.goes_pending = Some(wanted);
        self.state.goes_requested_at = Some(std::time::Instant::now());
        let _ = self.tx.send(DataCmd::LoadGoes {
            layer: wanted.0,
            west_satellite: wanted.1,
        });
    }

    /// Ask the data thread for the storm-relative volume the current frame +
    /// motion setting needs. One derivation is in flight at a time: while it
    /// builds (and on playback, where every frame wants its own), the panes
    /// keep showing the last derived volume instead of blanking.
    fn pump_srm(&mut self) {
        if self.state.moment != radar_core::Moment::StormRelativeVelocity {
            return;
        }
        let Some(key) = self.state.srm_key() else {
            return;
        };
        if self.state.srm_cache.as_ref().map(|(k, _)| *k) == Some(key) {
            self.state.srm_shown_key = Some(key);
            return;
        }
        if let Some(pending) = self.state.srm_pending {
            // A derivation this old means the request was lost (site switch,
            // archive jump); ask again rather than showing nothing forever.
            if self
                .srm_requested_at
                .is_some_and(|t| t.elapsed() > std::time::Duration::from_secs(3))
            {
                log::warn!("SRM derive {pending:x} never came back; re-requesting");
                self.state.srm_pending = None;
            } else {
                return;
            }
        }
        let Some(frame_volume) = self
            .state
            .current()
            .map(|f| std::sync::Arc::clone(&f.volume))
        else {
            return;
        };
        self.state.srm_pending = Some(key);
        self.srm_requested_at = Some(std::time::Instant::now());
        let _ = self.tx.send(DataCmd::DeriveSrm {
            key,
            volume: frame_volume,
            toward_deg: self.state.srm_dir_deg,
            speed_kt: self.state.srm_speed_kt,
        });
    }

    fn ui_frame(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pump();
        self.pump_srm();
        self.pump_mrms();
        self.pump_goes();
        self.pump_ai_site();
        self.pump_ai_data();
        self.handle_input(ui.ctx());

        // Texture accounting (debug): the app's GPU-side footprint, and the
        // names of the biggest residents so a leak is attributable. On a
        // shared-memory GPU this is what competes with system RAM.
        if log::log_enabled!(log::Level::Debug)
            && self.tex_stats_at.elapsed() >= std::time::Duration::from_secs(10)
        {
            self.tex_stats_at = std::time::Instant::now();
            let tm = ui.ctx().tex_manager();
            let tm = tm.read();
            let mut items: Vec<(usize, String)> = tm
                .allocated()
                .map(|(_, m)| {
                    (
                        m.bytes_used(),
                        format!("{} {}x{}", m.name, m.size[0], m.size[1]),
                    )
                })
                .collect();
            items.sort_by(|a, b| b.0.cmp(&a.0));
            let total: usize = items.iter().map(|(b, _)| b).sum();
            log::debug!(
                "textures: {} alloc, {:.1} MB total | top: {}",
                tm.num_allocated(),
                total as f32 / 1e6,
                items
                    .iter()
                    .take(4)
                    .map(|(b, n)| format!("{n} {:.1}MB", *b as f32 / 1e6))
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
            log::debug!(
                "ring: {} frames, {:.2} GB (budget {:.2} GB) | rss: {} MB",
                self.state.ring.len(),
                self.state.ring_bytes as f64 / 1e9,
                self.state.ring_budget_bytes as f64 / 1e9,
                std::fs::read_to_string("/proc/self/statm")
                    .ok()
                    .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
                    .map(|pages| pages * 4096 / 1_000_000)
                    .map(|mb| mb.to_string())
                    .unwrap_or_else(|| "?".into()),
            );
            // Belt-and-braces on top of the mallopt tuning in main.rs: hand
            // free heap pages back to the OS periodically, so a process left
            // open all day doesn't creep into system memory pressure.
            if self.last_trim.elapsed() >= std::time::Duration::from_secs(300) {
                self.last_trim = std::time::Instant::now();
                #[cfg(target_env = "gnu")]
                unsafe {
                    libc::malloc_trim(0);
                }
            }
        }

        let dt = ui.input(|i| i.stable_dt);
        self.state.tick(std::time::Duration::from_secs_f32(dt));

        // Keep viewport size in sync so rasterization matches the panel.
        let avail = ui.available_size();
        self.state.view.width_px = avail.x;
        self.state.view.height_px = avail.y;

        crate::ui::layout(ui, self);
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
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // Time the frame's *work* separately from the gap since the previous
        // frame: an idle app wakes about once a second (timers such as the
        // scan-age pill), and counting that sleep as frame cost would bury a
        // genuinely slow frame — which is exactly what happened with the
        // first version of this line ("1 fps, avg 983 ms" while doing
        // nothing). Both numbers feed the stats line in `record_frame_stats`.
        let t0 = std::time::Instant::now();
        self.ui_frame(ui, frame);
        self.record_frame_stats(t0.elapsed(), ui.ctx());
    }
}
