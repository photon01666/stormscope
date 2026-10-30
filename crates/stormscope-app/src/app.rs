//! The eframe App: owns AppState, spawns background data threads, polls
//! their channels each frame, and drives the UI.

use crate::brief::LookPointSample;
use crate::state::{AiChatMessage, AiRole, AiTab, AppState, LoopState};
use eframe::egui;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

/// Force the native window frame dark, independent of the Windows app theme.
///
/// The app is always dark, so a light titlebar and the Windows 11 border look
/// like a rendering bug. DWM's immersive dark mode darkens the caption; the
/// caption/border/text colours (Windows 11) make the 1 px frame and titlebar
/// match the egui panels. Best-effort: older Windows builds ignore the newer
/// attributes.
#[cfg(windows)]
fn apply_dark_window_frame(cc: &eframe::CreationContext<'_>) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
        DwmSetWindowAttribute,
    };

    let Ok(handle) = cc.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(win) = handle.as_raw() else {
        return;
    };
    let hwnd = win.hwnd.get() as HWND;
    let dark: i32 = 1;
    // egui's dark panel fill (#1b1b1b) as a COLORREF (0x00BBGGRR), and white
    // caption text to go with it.
    let panel: u32 = 0x001b_1b1b;
    let white: u32 = 0x00ff_ffff;
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            &dark as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );
        for (attr, value) in [
            (DWMWA_BORDER_COLOR, panel),
            (DWMWA_CAPTION_COLOR, panel),
            (DWMWA_TEXT_COLOR, white),
        ] {
            DwmSetWindowAttribute(
                hwnd,
                attr as u32,
                &value as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<u32>() as u32,
            );
        }
    }
}

/// Which conversation a model request belongs to. Live updates have their own
/// path so they never enter the manual chat transcript and cannot block Ask.
#[derive(Debug, Clone)]
pub enum AiRequestKind {
    Chat,
    Live {
        scan: chrono::DateTime<chrono::Utc>,
        site: String,
        seq: u64,
    },
}

/// Messages from background data threads -> UI.
pub enum DataMsg {
    Volume(Arc<radar_core::Volume>),
    /// Alerts near the selected site (drawn on the map).
    Alerts(Vec<radar_core::warnings::Warning>),
    /// Alerts that apply exactly to the selected station (fast lane).
    AlertsFast(Vec<radar_core::warnings::Warning>),
    /// Every active alert nationwide (alerts browser only).
    AllAlerts(Vec<radar_core::warnings::Warning>),
    /// Current UV index for the forecast point (Open-Meteo; NWS has no feed).
    UvIndex(Option<f32>),
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
    /// Model sounding for a requested point.
    Sounding(radar_core::sounding::Sounding),
    /// Sounding fetch failed.
    SoundingError(String),
    /// Terrain grid for beam-blockage masking.
    Terrain(std::sync::Arc<radar_core::terrain::TerrainGrid>),
    /// Terrain fetch failed.
    TerrainError(String),
    /// HRRR point forecast profiles.
    Hrrr(radar_data::SoundingSeries),
    /// HRRR profile fetch failed.
    HrrrError(String),
    /// Synoptic (Mesowest) stations in the current view, with latest obs.
    SynopticStations(Vec<radar_data::synoptic::SynopticStation>),
    /// Synoptic fetch failed.
    SynopticError(String),
    /// Free METAR airport observations for the selected radar window.
    MetarStations(Vec<radar_data::synoptic::SynopticStation>),
    /// METAR fetch failed.
    MetarError(String),
    /// Recent METAR reports for one airport.
    MetarHistory(std::sync::Arc<radar_data::MetarHistory>),
    /// METAR history fetch failed.
    MetarHistoryError(String),
    /// Observed RAOB at the requested point.
    ObservedSounding(radar_core::sounding::Sounding),
    /// RAOB fetch failed.
    ObservedSoundingError(String),
    /// RAP analysis profile at the requested point.
    RapSounding(radar_core::sounding::Sounding),
    /// RAP fetch failed.
    RapSoundingError(String),
    /// Official Level III N0Q composite image.
    Level3(std::sync::Arc<radar_data::level3::Level3Image>),
    /// Level III fetch failed.
    Level3Error(String),
    /// QPE-vs-gauge verification report.
    Verification(std::sync::Arc<radar_data::QpeVerification>),
    /// Verification fetch failed.
    VerificationError(String),
    /// Multi-scan storm trajectories for the trend window.
    StormHistory {
        key: u64,
        histories: Vec<radar_render::StormHistory>,
    },
    /// Storm-history computation failed.
    StormHistoryError(String),
    /// GEFS ensemble plume.
    Ensemble(std::sync::Arc<radar_data::EnsembleSeries>),
    /// Ensemble fetch failed.
    EnsembleError(String),
    /// HRRR 500 mb wind/height run.
    MidLevel(std::sync::Arc<radar_data::MidLevelRun>),
    /// Mid-level wind fetch failed.
    MidLevelError(String),
    /// Ambient Weather personal stations on the account.
    AmbientStations(Vec<radar_data::ambient::AmbientStation>),
    /// Ambient Weather fetch failed.
    AmbientError(String),
    /// Newest videos from the curated weather YouTube channels.
    YouTubeFeed(Result<Vec<radar_data::YouTubeVideo>, String>),
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
        cached: bool,
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
    /// QPE accumulation: CONUS-wide image plus the sharp radar window.
    Qpe {
        product: radar_data::mrms::MrmsProduct,
        mosaics: Vec<Arc<radar_data::mrms::Mosaic>>,
    },
    /// One HRRR 2D model field over the local radar box.
    ModelField(Arc<radar_data::HrrrField>),
    /// GOES satellite frames (GIBS WMS images): CONUS-wide and the sharp
    /// window around the radar.
    Goes {
        images: Vec<Arc<radar_data::goes::GoesImage>>,
    },
    /// A new HRRR FutureCast run: its init time, box and frame plan. Frames
    /// follow one by one so the first one appears while the rest download.
    FuturecastRun {
        init: chrono::DateTime<chrono::Utc>,
        bbox: [f64; 4],
        frames: Vec<radar_data::futurecast::FrameInfo>,
        extended: bool,
    },
    /// One decoded HRRR simulated-reflectivity frame.
    FuturecastFrame(Arc<radar_data::futurecast::Frame>),
    /// The FutureCast fetch finished (possibly with a partial set of frames).
    FuturecastDone {
        received: usize,
        total: usize,
        error: Option<String>,
        extended: bool,
    },
    /// GOES GLM flashes from one 20-second file.
    Lightning {
        strikes: Vec<radar_data::glm::GlmStrike>,
        satellite: String,
        key: String,
    },
    LightningError(String),
    /// A cached day was deleted (scans removed, bytes reclaimed).
    CachedDeleted {
        date: chrono::NaiveDate,
        removed: usize,
        bytes: u64,
    },
    /// The AI meteorologist's reply, or the error to show in its window.
    AiBrief {
        kind: AiRequestKind,
        result: Result<String, String>,
    },
    /// Result of the Settings popup's connectivity test.
    AiTest(Result<String, String>),
    /// Android self-update: latest release lookup / APK download.
    UpdateCheck(Result<Option<radar_data::update::UpdateInfo>, String>),
    UpdateDownload(Result<std::path::PathBuf, String>),
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
    /// Delete every cached scan for one Central day at a site.
    DeleteCachedDay {
        site: String,
        date: chrono::NaiveDate,
    },
    /// Load a Central day entirely from the on-disk cache (no network): the
    /// window of cached scans around `target`.
    LoadCachedDay {
        site: String,
        date: chrono::NaiveDate,
        target: chrono::DateTime<chrono::Utc>,
    },
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
    LoadLastTwoHours(String, Option<chrono::DateTime<chrono::Utc>>),
    /// Page the loaded archive day earlier (-1) or later (+1).
    ArchiveWindowShift(i32),
    /// Leave archive mode: clear the ring and refresh with the newest
    /// available scan immediately.
    /// Leave archive/replay for live radar of the given site — the UI's site
    /// travels with the command so the prime and poller can never diverge
    /// from what the app is watching (a silent desync dropped every volume).
    EnterLive(String),
    RefreshLive(String),
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
    /// Fetch the newest QPE accumulation for one product.
    LoadQpe(radar_data::mrms::MrmsProduct),
    /// Fetch one HRRR or RAP 2D field for a forecast hour.
    LoadModelField {
        model: radar_data::FieldModel,
        kind: radar_data::HrrrFieldKind,
        hour: u16,
    },
    /// Update the Synoptic token held by the data thread.
    SetSynopticToken(String),
    /// Fetch Synoptic stations for the given bbox [w,s,e,n] and viewport.
    LoadSynoptic {
        bbox: [f64; 4],
        width: u32,
        height: u32,
    },
    /// Fetch METAR stations for a bbox [w,s,e,n].
    LoadMetar {
        bbox: [f64; 4],
    },
    /// Fetch the recent report history for one METAR station.
    LoadMetarHistory(String),
    /// Update the Ambient Weather credentials held by the data thread.
    SetAmbientKeys {
        api_key: String,
        app_key: String,
    },
    /// Fetch the Ambient Weather stations now.
    LoadAmbient,
    /// Refresh the curated weather YouTube feed (three uploads per channel).
    LoadYouTubeFeed,
    /// Fetch the latest GOES frame for the chosen layer/satellite.
    LoadGoes {
        layer: radar_data::goes::GoesLayer,
        west_satellite: bool,
    },
    /// Fetch the complete HRRR FutureCast run for a box: latest 18-hour
    /// cycle, or the 48-hour extended cycle.
    LoadFuturecast {
        bbox: [f64; 4],
        extended: bool,
    },
    /// Fetch the newest GOES GLM lightning file for the chosen satellite.
    LoadGlm {
        west: bool,
    },
    RefreshAlerts,
    /// Fetch the SPC categorical outlook for a day (1..=3).
    LoadOutlook(u8, radar_core::outlooks::OutlookType),
    /// Rain/snow totals for the stations around (lat, lon).
    LoadTotals(f64, f64),
    /// Fetch a model sounding for a point.
    LoadSounding(f64, f64),
    /// Fetch an observed RAOB nearest a point.
    LoadObservedSounding(f64, f64),
    /// Fetch the RAP analysis profile at a point.
    LoadRapSounding(f64, f64),
    /// Fetch the official N0Q Level III composite for a bbox.
    LoadLevel3 {
        bbox: [f64; 4],
    },
    /// Compare MRMS QPE with nearby station gauges for a duration.
    LoadVerification {
        lat: f64,
        lon: f64,
        hours: u16,
    },
    /// Build multi-scan storm histories for the trend window.
    LoadStormHistory {
        volumes: Vec<Arc<radar_core::Volume>>,
        key: u64,
        tilt_deg: f32,
        lat: f64,
        lon: f64,
    },
    /// Fetch a GEFS ensemble plume for a point.
    LoadEnsemble {
        field: radar_data::EnsembleField,
        lat: f64,
        lon: f64,
    },
    /// Fetch the HRRR 500 mb wind/height run.
    LoadMidLevel,
    /// Fetch terrain around a radar site for beam-blockage masking.
    LoadTerrain(f64, f64),
    /// Fetch HRRR point profiles for a coordinate.
    LoadHrrr(f64, f64),
    /// Fetch the forecaster discussion text for a day (1..=3).
    LoadOutlookDiscussion(u8),
    /// Fetch the NWS gridpoint forecast for a coordinate.
    LoadForecast(f64, f64),
    /// Ask the AI meteorologist for a brief or an answer (spawned on its own
    /// thread so a slow model never holds up the data coordinator).
    AiBrief {
        kind: AiRequestKind,
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
    /// Android self-update: look for a newer GitHub release.
    CheckUpdate {
        current: String,
        asset_suffix: String,
    },
    /// Self-update: download the selected installer asset.
    DownloadUpdate {
        url: String,
        version: String,
        asset_suffix: String,
    },
    /// Delete every downloaded radar volume from disk (`l2/`).
    ClearRadar,
    /// Delete every cached basemap tile from disk (`tiles/`).
    ClearTiles,
    Shutdown,
}

/// Id of the synthetic test polygon `STORMSCOPE_DEBUG_WARNING` injects.
const DEBUG_WARNING_ID: &str = "DEBUG-TEST-OVERLAY";

/// A surface observation older than this makes the next AI question wait for
/// a refresh, so asking twice never reuses a stale current-weather snapshot.
const AI_OBS_FRESH_SECS: f64 = 120.0;

/// Which cache section a pending "clear" is asking to delete.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ClearTarget {
    Radar,
    Tiles,
}

#[cfg(target_os = "android")]
#[derive(Clone, Debug)]
pub struct LivePendingTool {
    pub id: String,
    pub name: String,
}

pub struct StormApp {
    pub state: AppState,
    pub tx: Sender<DataCmd>,
    pub rx: Receiver<DataMsg>,
    /// Set by GO LIVE / snap-to-live: a running archive stream checks it and
    /// stops, so the EnterLive command is not stuck behind 24 downloads.
    pub archive_cancel: Arc<std::sync::atomic::AtomicBool>,
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
    /// Cached QPE textures, one per window.
    pub qpe_tex: Vec<(usize, egui::TextureHandle)>,
    /// Cached HRRR 2D field texture.
    pub model_field_tex: Vec<(usize, egui::TextureHandle)>,
    /// Cached GOES textures, one per window.
    pub goes_tex: Vec<(usize, egui::TextureHandle)>,
    /// Cached official Level III N0Q texture.
    pub level3_tex: Vec<(usize, egui::TextureHandle)>,
    /// Cached 500 mb speed textures.
    pub midlevel_tex: Vec<(usize, egui::TextureHandle)>,
    /// Cached FutureCast textures, keyed by frame Arc identity.
    pub futurecast_tex: Vec<(usize, egui::TextureHandle)>,
    /// Cached YouTube thumbnail textures, keyed by video id.
    pub youtube_tex: Vec<(String, egui::TextureHandle)>,
    /// Native child WebView embedded over the in-app video window.
    #[cfg(not(target_os = "android"))]
    pub video_embed: Option<crate::video_embed::VideoEmbed>,
    /// Screen-space rectangle the WebView should cover this frame.
    pub youtube_video_rect: Option<egui::Rect>,
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
    /// Rotation/TVS signatures for the current volume (cached by scan time).
    pub rotation_cache: Option<(u64, Vec<radar_core::severe::RotationSignature>)>,
    /// Hail cores accumulated across the ring (cached by ring signature).
    pub hail_cache: Option<(u64, Vec<radar_core::severe::HailCore>)>,
    /// VAD profile for the current volume (cached by scan time).
    pub vad_cache: Option<(u64, radar_core::vad::VadProfile)>,
    /// QC-processed reflectivity sweep. Dragging must not rerun the filter
    /// and terrain mask on every raster; key by sweep + config + DEM.
    pub qc_cache: Option<(u64, Arc<radar_core::Sweep>)>,
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
    /// A question asked while the surface observation was stale, parked here
    /// until the forced refresh lands (or a deadline passes), then sent so
    /// the answer is built from fresh current-weather data.
    pub ai_fresh_queue: Option<Option<String>>,
    pub ai_fresh_started: Option<std::time::Instant>,
    pub ai_fresh_deadline: Option<std::time::Instant>,
    /// Android Gemini Live voice session and its event stream.
    #[cfg(target_os = "android")]
    pub live: Option<crate::gemini_live::LiveSession>,
    #[cfg(target_os = "android")]
    pub live_rx: Option<Receiver<crate::gemini_live::LiveEvent>>,
    #[cfg(target_os = "android")]
    pub live_pending_tool: Option<LivePendingTool>,
    #[cfg(not(target_os = "android"))]
    pub live_pending_tool: Option<()>,
    /// The pending DATA fetch came from a Gemini Live tool, not the text AI.
    pub ai_data_from_live: bool,
    /// In-progress map pan origin, for drift-free dragging.
    pub map_drag_start: Option<(radar_render::MapView, egui::Pos2)>,
    /// Automatic per-scan Live AI Tracking state, rolling memory and feed.
    pub live_ai: crate::live_ai::LiveAi,
}

/// One marker's storm-arrival estimate: the earliest incoming cell.
#[derive(Debug, Clone)]
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
    /// Full raster signature (frame content + view + scale).
    pub sig: u64,
    /// Frame-content signature only (no view/scale). Used to keep the loop
    /// animating during a drag without rasterizing every pointer frame.
    pub frame_sig: u64,
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
    /// Android: last GPS fix mirrored for background Storm Watch radar
    /// gating. The data thread refreshes it from Java while backgrounded.
    pub gps_fix: Option<(f64, f64)>,
    /// Android: map markers checked by background Storm Watch radar gating.
    pub watch_markers: Vec<(f64, f64)>,
    /// Android: the Storm Watch setting, mirrored so the data thread can gate
    /// background radar without calling back into the UI thread.
    pub storm_watch_enabled: bool,
    /// Android: only live-mode apps keep downloading radar in the background.
    pub live_mode: bool,
}

impl StormApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // The whole UI is designed dark. egui otherwise follows the OS theme,
        // and Windows' light app mode turned the AI window (and every stock
        // widget) into white-on-white. Pin the dark theme on every platform.
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        // The app is dark regardless of the Windows app theme; make the native
        // frame match instead of leaving a light titlebar/border around it.
        #[cfg(windows)]
        apply_dark_window_frame(cc);

        let (tx_ui, rx_data): (Sender<DataMsg>, Receiver<DataMsg>) = std::sync::mpsc::channel();
        let (tx_data, rx_ui): (Sender<DataCmd>, Receiver<DataCmd>) = std::sync::mpsc::channel();

        let data = Arc::new(Mutex::new(SharedData {
            sites: radar_core::sites::SITES.iter().collect(),
            tilts: vec![
                0.5, 0.9, 1.3, 1.8, 2.4, 3.1, 4.0, 5.1, 6.4, 8.0, 10.0, 12.0, 19.5,
            ],
            gps_fix: None,
            watch_markers: Vec::new(),
            storm_watch_enabled: false,
            live_mode: true,
        }));

        // Background data thread: fetches, decodes and polls; results are
        // delivered to the UI over the message channel below.
        let archive_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel = Arc::clone(&archive_cancel);
        let shared = Arc::clone(&data);
        std::thread::Builder::new()
            .name("data".into())
            .spawn(move || crate::background::run(rx_ui, tx_ui, cancel, shared))
            .expect("spawn data thread");

        // A window's resize band is only a few pixels wide by default; on a
        // HiDPI display it is easy to miss it and have the press fall through
        // to the map. Give every window a comfortably grabby edge.
        cc.egui_ctx.all_styles_mut(|style| {
            style.interaction.resize_grab_radius_side = 14.0;
            style.interaction.resize_grab_radius_corner = 18.0;
            // A step darker than egui's stock dark theme (27): the map and
            // radar colors carry the screen, the chrome should recede.
            let mut visuals = style.visuals.clone();
            visuals.panel_fill = egui::Color32::from_gray(18);
            visuals.window_fill = egui::Color32::from_gray(18);
            visuals.extreme_bg_color = egui::Color32::from_gray(6);
            visuals.widgets.noninteractive.bg_fill = egui::Color32::from_gray(18);
            visuals.widgets.noninteractive.weak_bg_fill = egui::Color32::from_gray(18);
            style.visuals = visuals;
            // Android: a window's ✕ is sized from the Heading font's row
            // height — 18 pt is a ~24 dp target, far too small for a finger.
            // A bigger Heading grows the tap square (and titles) to ~35 dp.
            #[cfg(target_os = "android")]
            {
                style.text_styles.insert(
                    egui::TextStyle::Heading,
                    egui::FontId::new(26.0, egui::FontFamily::Proportional),
                );
            }
        });

        let mut state = AppState::default();
        #[cfg(target_os = "android")]
        {
            // Seed the toggle before the first frame so a quick home press
            // can still gate background radar correctly.
            state.storm_watch_enabled = crate::android_storm_watch::enabled();
            state.storm_watch_polled_at = f64::NEG_INFINITY;
        }
        #[cfg(windows)]
        crate::embedded_piper::ensure_extracted();
        #[cfg(not(target_os = "android"))]
        crate::voice::set_voice(state.voice_model.clone());
        if let Some(fonts) = Self::configure_fonts(&cc.egui_ctx) {
            cc.egui_ctx.set_fonts(fonts);
        }

        // Basemap tiles share the disk cache root with radar volumes.
        let cache_root = radar_data::DiskCache::default_root();
        let tiles = radar_render::TileLayer::new(cache_root.join("tiles"));

        let app = Self {
            state,
            tx: tx_data,
            rx: rx_data,
            archive_cancel,
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
            qpe_tex: Vec::new(),
            model_field_tex: Vec::new(),
            goes_tex: Vec::new(),
            level3_tex: Vec::new(),
            midlevel_tex: Vec::new(),
            futurecast_tex: Vec::new(),
            youtube_tex: Vec::new(),
            #[cfg(not(target_os = "android"))]
            video_embed: crate::video_embed::VideoEmbed::new(cc),
            youtube_video_rect: None,
            view3d_section_key: None,
            prev_tracks: Vec::new(),
            marker_etas: Vec::new(),
            rotation_cache: None,
            hail_cache: None,
            vad_cache: None,
            qc_cache: None,
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
            ai_fresh_queue: None,
            ai_fresh_started: None,
            ai_fresh_deadline: None,
            #[cfg(target_os = "android")]
            live: None,
            #[cfg(target_os = "android")]
            live_rx: None,
            #[cfg(target_os = "android")]
            live_pending_tool: None,
            #[cfg(not(target_os = "android"))]
            live_pending_tool: None,
            ai_data_from_live: false,
            map_drag_start: None,
            live_ai: crate::live_ai::LiveAi::default(),
        };
        // The data thread boots on the hard-coded default; if the user saved
        // a different startup site, switch it immediately.
        if app.state.site_id != radar_core::DEFAULT_SITE {
            let _ = app.tx.send(DataCmd::SetSite(app.state.site_id.clone()));
        }
        app
    }

    /// Leave archive/replay and return to live radar: the mode flips,
    /// playback stops, and the data thread clears the ring and refills from
    /// the newest scans. Shared by the timeline's "GO LIVE" pill and the L
    /// key.
    pub fn snap_to_live(&mut self) {
        // A window stream can be 24 sequential downloads (seconds to
        // minutes); flag it to stop before queueing the live switch.
        self.archive_cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.state.mode = crate::state::Mode::Live;
        self.state.loop_state = crate::state::LoopState::Paused;
        self.state.archive_jump = None;
        self.state.alerts_popup_open = false;
        let _ = self
            .tx
            .send(crate::app::DataCmd::EnterLive(self.state.site_id.clone()));
    }

    /// Rescan the on-disk cache for the selected station. Cheap (one
    /// directory), refreshed periodically and after a site switch; the bottom
    /// timeline uses it to expose every downloaded scan, not just the ring.
    pub fn refresh_cached_scans(&mut self) {
        let site = self.state.site_id.clone();
        let scans: Vec<crate::state::CachedScan> = radar_data::cache::list_cached_volumes(&site)
            .into_iter()
            .map(|v| crate::state::CachedScan {
                start: v.start,
                bytes: v.bytes,
            })
            .collect();
        self.state.cached_scans = scans;
        self.state.cached_scans_site = site;
        self.state.cached_scans_at = Some(std::time::Instant::now());
    }

    /// Cached scans grouped into Central days for the Downloaded browser.
    pub fn refresh_cached_days(&mut self) {
        let site = self.state.site_id.clone();
        let central = crate::timefmt::central();
        let mut days: std::collections::BTreeMap<chrono::NaiveDate, crate::state::CachedDay> =
            std::collections::BTreeMap::new();
        for volume in radar_data::cache::list_cached_volumes(&site) {
            let date = volume.start.with_timezone(central).date_naive();
            days.entry(date)
                .and_modify(|day| {
                    day.scans += 1;
                    day.bytes += volume.bytes;
                    day.first = day.first.min(volume.start);
                    day.last = day.last.max(volume.start);
                })
                .or_insert(crate::state::CachedDay {
                    date,
                    scans: 1,
                    bytes: volume.bytes,
                    first: volume.start,
                    last: volume.start,
                });
        }
        let mut days: Vec<crate::state::CachedDay> = days.into_values().collect();
        days.sort_by(|a, b| b.date.cmp(&a.date));
        self.state.cached_days = days;
        self.state.cached_days_site = site;
        self.state.cached_days_at = Some(std::time::Instant::now());
    }

    /// Fetch the terrain grid when beam-blockage masking is enabled and the
    /// site changed. Cheap no-op otherwise.
    pub fn ensure_terrain(&mut self) {
        if !self.state.qc_blockage {
            return;
        }
        let site_id = self.state.site_id.clone();
        if self.state.qc_terrain_site != site_id {
            self.state.qc_terrain = None;
            self.state.qc_terrain_error = None;
            self.state.qc_terrain_site = site_id.clone();
        }
        if self.state.qc_terrain.is_none()
            && !self.state.qc_terrain_pending
            && self.state.qc_terrain_error.is_none()
        {
            if let Some(site) = radar_core::sites::site(&site_id) {
                self.state.qc_terrain_pending = true;
                self.state.qc_terrain_error = None;
                let _ = self
                    .tx
                    .send(DataCmd::LoadTerrain(site.latitude, site.longitude));
            }
        }
    }

    /// Mean motion of the tracked cells, as (toward degrees, knots). Used to
    /// seed the storm-relative velocity product with a real background flow.
    pub fn mean_storm_motion(&self) -> Option<(f32, f32)> {
        let (_, tracks) = self.tracks_cache.as_ref()?;
        if tracks.is_empty() {
            return None;
        }
        let n = tracks.len() as f64;
        let e = tracks.iter().map(|t| t.vel_e).sum::<f64>() / n;
        let ncomp = tracks.iter().map(|t| t.vel_n).sum::<f64>() / n;
        let kmh = e.hypot(ncomp) * 60.0;
        let kt = (kmh / 1.852) as f32;
        let toward = (e.atan2(ncomp).to_degrees().rem_euclid(360.0)) as f32;
        Some((toward, kt))
    }

    /// Select a radar site from the map (or controls) and tell the data
    /// thread to load it. Mirrors what the controls panel does on change.
    pub fn select_site(&mut self, id: &str) {
        if self.state.site_id == id {
            return;
        }
        self.state.site_id = id.to_string();
        // Live AI trends belong to one radar; do not compare cells across a
        // site switch. Keep the visible feed as history.
        self.live_ai.reset_for_site(id);
        // Tracks from the old site must not smooth into the new site's.
        self.prev_tracks.clear();
        self.tracks_cache = None;
        self.marker_etas.clear();
        self.rotation_cache = None;
        self.hail_cache = None;
        self.vad_cache = None;
        self.qc_cache = None;
        self.state.model_field = None;
        self.state.model_field_requested_at = None;
        self.refresh_cached_scans();
        self.refresh_cached_days();
        let _ = self.tx.send(crate::app::DataCmd::SetSite(id.to_string()));
        // The station layer is view-based; a site switch should not wait for
        // the 20-minute Synoptic timer.
        if self.state.show_synoptic {
            self.fetch_synoptic_now();
        }
    }

    /// Switch radar site from the Controls picker: move the map to that
    /// station and refresh the view's weather stations.
    pub fn open_site(&mut self, id: &str) {
        let changed = self.state.site_id != id;
        if let Some(site) = radar_core::sites::site(id) {
            self.state.view.center_lat = site.latitude;
            self.state.view.center_lon = site.longitude;
            self.state.view.zoom = self.state.view.zoom.max(7.0);
        }
        if changed {
            self.select_site(id);
        } else if self.state.show_synoptic {
            self.fetch_synoptic_now();
        }
    }

    /// Fetch the user's Ambient station now (no-op without both keys).
    pub(crate) fn fetch_ambient_now(&mut self) {
        if self.state.ambient_api_key.trim().is_empty()
            || self.state.ambient_app_key.trim().is_empty()
        {
            return;
        }
        let _ = self.tx.send(DataCmd::SetAmbientKeys {
            api_key: self.state.ambient_api_key.clone(),
            app_key: self.state.ambient_app_key.clone(),
        });
        let _ = self.tx.send(DataCmd::LoadAmbient);
        self.state.ambient_pending = true;
        self.state.ambient_last_attempt = Some(std::time::Instant::now());
    }

    /// Fetch Synoptic stations for the current map view now.
    pub(crate) fn fetch_synoptic_now(&mut self) {
        if self.state.synoptic_token.trim().is_empty() {
            return;
        }
        let view = &self.state.view;
        let w = view.width() as f64;
        let h = view.height() as f64;
        let (x0, y0) = view.view_px_to_lonlat(0.0, 0.0);
        let (x1, y1) = view.view_px_to_lonlat(w, h);
        let bbox = [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)];
        let (width, height) = (view.width() as u32, view.height() as u32);
        let center = (view.center_lat, view.center_lon, view.zoom);
        let _ = self
            .tx
            .send(DataCmd::SetSynopticToken(self.state.synoptic_token.clone()));
        let _ = self.tx.send(DataCmd::LoadSynoptic {
            bbox,
            width,
            height,
        });
        self.state.synoptic_pending = true;
        self.state.synoptic_last_attempt = Some(std::time::Instant::now());
        self.state.synoptic_last_view = Some(center);
    }

    /// Fetch METARs around the selected radar now.
    pub(crate) fn fetch_metar_now(&mut self) {
        let Some(site) = radar_core::sites::site(&self.state.site_id) else {
            return;
        };
        let bbox = radar_data::local_overlay_box(site.latitude, site.longitude);
        let _ = self.tx.send(DataCmd::LoadMetar { bbox });
        self.state.metar_pending = true;
        self.state.metar_last_attempt = Some(std::time::Instant::now());
    }

    /// Fetch the official N0Q Level III composite for the selected radar.
    pub(crate) fn fetch_level3_now(&mut self) {
        if !self.state.show_level3 {
            return;
        }
        let Some(site) = radar_core::sites::site(&self.state.site_id) else {
            return;
        };
        if self.state.level3_pending {
            return;
        }
        let bbox = radar_data::local_overlay_box(site.latitude, site.longitude);
        let _ = self.tx.send(DataCmd::LoadLevel3 { bbox });
        self.state.level3_pending = true;
        self.state.level3_requested_at = Some(std::time::Instant::now());
        self.state.level3_site = self.state.site_id.clone();
        self.state.level3_error = None;
    }

    /// Send the app's alert lists through the shared voice notifier. The
    /// data thread calls the same notifier, so desktop speech does not depend
    /// on UI frames (e.g. a hidden Wayland workspace); the shared seen set
    /// prevents duplicate announcements.
    fn announce_nearby_alerts(&self) {
        crate::voice::notify_near(&self.state.alerts);
        crate::voice::notify_near(&self.state.all_alerts);
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

    fn configure_fonts(_ctx: &egui::Context) -> Option<egui::FontDefinitions> {
        let mut fonts = egui::FontDefinitions::default();
        // Prefer a mono font for readouts if present on the system.
        for candidate in [
            "DejaVuSansMono",
            "LiberationMono",
            "consola",
            "CascadiaMono",
            "monospace",
        ] {
            let Some(path) = find_system_font(candidate) else {
                continue;
            };
            let Ok(bytes) = std::fs::read(&path) else {
                log::warn!("font {candidate}: cannot read {path}");
                continue;
            };
            // The family list holds *keys into font_data*, not file paths.
            // Inserting the path alone made epaint panic with "No font data
            // found" the first time a monospace glyph was drawn (notably on
            // Windows, where Consolas is a top-level file in C:\Windows\Fonts).
            let key = format!("system-mono-{candidate}");
            fonts
                .font_data
                .insert(key.clone(), Arc::new(egui::FontData::from_owned(bytes)));
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .insert(0, key);
            break;
        }
        // Ubuntu-Light (egui's prose font) has no arrows or triangles, so
        // strings like "Settings → AI meteorologist" render as tofu boxes.
        // Hack ships with egui for monospace and covers them; add it as a
        // last-resort fallback for prose too.
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push("Hack".to_owned());
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
                            if let Some(tool) = self.live_pending_tool.take() {
                                let _ = points;
                                #[cfg(target_os = "android")]
                                self.respond_live_tool(
                                    tool.id,
                                    tool.name,
                                    serde_json::json!({ "ok": true, "summary": self.voice_summary() }),
                                );
                                #[cfg(not(target_os = "android"))]
                                let _ = tool;
                            } else if !points.is_empty() {
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
                    self.announce_nearby_alerts();
                }
                DataMsg::UvIndex(uv) => self.state.uv_index = uv,
                DataMsg::Reports(v) => self.state.lsrs = v,
                DataMsg::Mcds(v) => self.state.mcds = v,
                DataMsg::AlertsFast(a) => {
                    self.state.alerts_fast = a;
                    self.state.alerts = crate::state::merge_alerts(
                        &self.state.alerts_slow,
                        &self.state.alerts_fast,
                    );
                    self.announce_nearby_alerts();
                }
                DataMsg::AllAlerts(a) => {
                    self.state.all_alerts = a;
                    self.announce_nearby_alerts();
                }
                DataMsg::Observation(o) => {
                    // Freshness is stamped by the forecast message that shares
                    // the same request; keep the older stamp if one is set.
                    self.state
                        .forecast_fetched_at
                        .get_or_insert_with(std::time::Instant::now);
                    self.state.observation_fetched_at = Some(std::time::Instant::now());
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
                DataMsg::SynopticStations(v) => {
                    // Keep the independent METAR layer when the token-backed
                    // Synoptic refresh replaces the rest of the stations.
                    let metar: Vec<_> = self
                        .state
                        .synoptic_stations
                        .iter()
                        .filter(|s| s.source == radar_data::synoptic::StationSource::Metar)
                        .cloned()
                        .collect();
                    let existing: std::collections::HashSet<String> =
                        v.iter().map(|s| s.stid.clone()).collect();
                    self.state.synoptic_stations = v;
                    self.state
                        .synoptic_stations
                        .extend(metar.into_iter().filter(|s| !existing.contains(&s.stid)));
                    self.state.synoptic_last_fetch = Some(std::time::Instant::now());
                    self.state.synoptic_pending = false;
                    self.state.synoptic_error = None;
                    log::debug!("synoptic stations: {}", self.state.synoptic_stations.len());
                }
                DataMsg::SynopticError(e) => {
                    self.state.synoptic_pending = false;
                    self.state.synoptic_error = Some(e);
                }
                DataMsg::MetarStations(v) => {
                    let n = v.len();
                    self.state
                        .synoptic_stations
                        .retain(|s| s.source != radar_data::synoptic::StationSource::Metar);
                    let existing: std::collections::HashSet<String> = self
                        .state
                        .synoptic_stations
                        .iter()
                        .map(|s| s.stid.clone())
                        .collect();
                    self.state
                        .synoptic_stations
                        .extend(v.into_iter().filter(|s| !existing.contains(&s.stid)));
                    self.state.metar_last_fetch = Some(std::time::Instant::now());
                    self.state.metar_pending = false;
                    self.state.metar_error = None;
                    log::debug!("METAR stations merged: {n}");
                }
                DataMsg::MetarError(e) => {
                    self.state.metar_pending = false;
                    self.state.metar_error = Some(e);
                }
                DataMsg::MetarHistory(history) => {
                    self.state.metar_history = Some(history);
                    self.state.metar_history_pending = false;
                    self.state.metar_history_error = None;
                }
                DataMsg::MetarHistoryError(e) => {
                    self.state.metar_history_pending = false;
                    self.state.metar_history_error = Some(e);
                }
                DataMsg::ObservedSounding(s) => {
                    self.state.observed_sounding_station = Some(s.source.clone());
                    self.state.observed_sounding = Some(s);
                    self.state.observed_sounding_fetched_at = Some(std::time::Instant::now());
                    self.state.observed_sounding_error = None;
                }
                DataMsg::ObservedSoundingError(e) => {
                    self.state.observed_sounding_error = Some(e);
                    self.state.observed_sounding_requested = None;
                }
                DataMsg::RapSounding(s) => {
                    self.state.rap_sounding = Some(s);
                    self.state.rap_sounding_fetched_at = Some(std::time::Instant::now());
                    self.state.rap_sounding_error = None;
                }
                DataMsg::RapSoundingError(e) => {
                    self.state.rap_sounding_error = Some(e);
                    self.state.rap_sounding_requested = None;
                }
                DataMsg::Level3(image) => {
                    log::info!(
                        "N0Q Level III: {}x{} at {}",
                        image.width,
                        image.height,
                        image.time.format("%H:%MZ")
                    );
                    self.state.level3 = Some((image, std::time::Instant::now()));
                    self.state.level3_pending = false;
                    self.state.level3_requested_at = None;
                }
                DataMsg::Level3Error(e) => {
                    log::warn!("N0Q Level III: {e}");
                    self.state.level3_pending = false;
                    self.state.level3_error = Some(e);
                }
                DataMsg::Verification(report) => {
                    log::info!(
                        "QPE verification: {} pairs, bias {:.2} in",
                        report.n,
                        report.bias_in
                    );
                    self.state.verification = Some(report);
                    self.state.verification_pending = false;
                    self.state.verification_error = None;
                    self.state.verification_fetched_at = Some(std::time::Instant::now());
                }
                DataMsg::VerificationError(e) => {
                    self.state.verification_pending = false;
                    self.state.verification_error = Some(e);
                }
                DataMsg::StormHistory { key, histories } => {
                    if self.state.storm_trends_requested == Some(key) {
                        self.state.storm_trends_pending = false;
                        self.state.storm_trends = Some(histories);
                        self.state.storm_trends_key = Some(key);
                        self.state.storm_trends_error = None;
                        self.state.storm_trends_selected = 0;
                    }
                }
                DataMsg::StormHistoryError(e) => {
                    self.state.storm_trends_pending = false;
                    self.state.storm_trends_error = Some(e);
                }
                DataMsg::Ensemble(series) => {
                    log::info!(
                        "ensemble {}: {} members x {} times",
                        series.field.label(),
                        series.members.len(),
                        series.times.len()
                    );
                    self.state.ensemble = Some(series);
                    self.state.ensemble_pending = false;
                    self.state.ensemble_error = None;
                    self.state.ensemble_fetched_at = Some(std::time::Instant::now());
                }
                DataMsg::EnsembleError(e) => {
                    self.state.ensemble_pending = false;
                    self.state.ensemble_error = Some(e);
                }
                DataMsg::MidLevel(run) => {
                    log::info!(
                        "mid-level wind: {} frames init {}",
                        run.frames.len(),
                        run.init.format("%H:%MZ")
                    );
                    self.state.midlevel = Some(run);
                    self.state.midlevel_pending = false;
                    self.state.midlevel_error = None;
                    self.state.midlevel_frame = 0;
                }
                DataMsg::MidLevelError(e) => {
                    self.state.midlevel_pending = false;
                    self.state.midlevel_error = Some(e);
                }
                DataMsg::AmbientStations(v) => {
                    log::debug!("ambient stations: {}", v.len());
                    self.state.ambient_stations = v;
                    self.state.ambient_last_fetch = Some(std::time::Instant::now());
                    self.state.ambient_pending = false;
                    self.state.ambient_error = None;
                }
                DataMsg::AmbientError(e) => {
                    self.state.ambient_pending = false;
                    self.state.ambient_error = Some(e);
                }
                DataMsg::YouTubeFeed(result) => {
                    self.state.youtube_pending = false;
                    self.state.youtube_fetched_at = Some(std::time::Instant::now());
                    match result {
                        Ok(videos) => {
                            log::info!("YouTube feed: {} videos", videos.len());
                            self.state.youtube_videos =
                                videos.into_iter().map(std::sync::Arc::new).collect();
                            self.state.youtube_error = None;
                            // Scripted smoke test: open the newest video
                            // automatically when the feed lands.
                            if self.state.youtube_player.is_none()
                                && std::env::var_os("STORMSCOPE_VIDEO_OPEN").is_some()
                                && let Some(video) = self.state.youtube_videos.first().cloned()
                            {
                                let _ = crate::player::open(self, &video.id, &video.title);
                            }
                        }
                        Err(error) => {
                            log::warn!("YouTube feed failed: {error}");
                            self.state.youtube_error = Some(error);
                        }
                    }
                }
                DataMsg::Sounding(s) => {
                    log::debug!("sounding loaded: {} levels", s.levels.len());
                    self.state.sounding_requested = Some((s.latitude, s.longitude));
                    self.state.sounding = Some(s);
                    self.state.sounding_fetched_at = Some(std::time::Instant::now());
                    self.state.sounding_error = None;
                    self.pump_ai_data();
                }
                DataMsg::SoundingError(e) => {
                    log::warn!("sounding: {e}");
                    self.state.sounding_error = Some(e);
                    self.state.sounding_requested = None;
                }
                DataMsg::Terrain(grid) => {
                    log::info!("terrain grid loaded: {}x{}", grid.width, grid.height);
                    self.state.qc_terrain = Some(grid);
                    self.state.qc_terrain_pending = false;
                    self.state.qc_terrain_error = None;
                    // The raster signature does not contain the terrain grid
                    // itself; force the next frame to rebuild the texture.
                    self.radar_tex = None;
                    self.radar_pending = None;
                }
                DataMsg::TerrainError(e) => {
                    log::warn!("terrain: {e}");
                    self.state.qc_terrain_pending = false;
                    self.state.qc_terrain_error = Some(e);
                }
                DataMsg::Hrrr(series) => {
                    log::info!("HRRR series: {} profiles", series.profiles.len());
                    self.state.hrrr_requested = Some((series.latitude, series.longitude));
                    let now = chrono::Utc::now();
                    self.state.hrrr_hour = series
                        .profiles
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, p)| (p.valid_time - now).num_seconds().abs())
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    self.state.hrrr = Some(series);
                    self.state.hrrr_fetched_at = Some(std::time::Instant::now());
                    self.state.hrrr_error = None;
                }
                DataMsg::HrrrError(e) => {
                    log::warn!("HRRR series: {e}");
                    self.state.hrrr_error = Some(e);
                    self.state.hrrr_requested = None;
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
                    log::debug!(
                        "outlook day {} {} loaded (issued {label})",
                        o.day,
                        o.ty.label()
                    );
                    if o.day == self.state.outlook_day {
                        // One slot per product; a refresh replaces its own
                        // type without dropping the other three.
                        self.state
                            .outlooks
                            .retain(|x| !(x.day == o.day && x.ty == o.ty));
                        self.state.outlooks.push(o);
                        self.state.outlook = self
                            .state
                            .outlooks
                            .iter()
                            .find(|x| x.ty == self.state.outlook_type)
                            .cloned();
                        self.pump_ai_data();
                    }
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
                    self.rotation_cache = None;
                    self.hail_cache = None;
                    self.vad_cache = None;
                    self.qc_cache = None;
                    self.state.model_field = None;
                    self.state.model_field_requested_at = None;
                    // Tracks are georeferenced to the old site/mode; don't
                    // let the smoothing blend across the switch.
                    self.prev_tracks.clear();
                    self.marker_etas.clear();
                    self.state.dbz_crosshair = false;
                    self.state.dbz_crosshair_pos = None;
                    // FutureCast is tied to the site's box. Only a site
                    // change invalidates it; the startup/return-to-live
                    // prime also sends ClearFrames, and clearing there made
                    // the UI issue a duplicate download.
                    if !self.state.futurecast_site.is_empty()
                        && self.state.futurecast_site != self.state.site_id
                    {
                        self.state.futurecast = None;
                        self.state.futurecast_pending = false;
                        self.state.futurecast_requested_at = None;
                        self.state.futurecast_playing = false;
                        self.state.futurecast_site.clear();
                    }
                }
                DataMsg::ArchiveCrawl(status) => {
                    self.state.archive_crawl = Some(status);
                }
                DataMsg::LoadProgress {
                    label,
                    done,
                    total,
                    cached,
                } => {
                    self.state.load_progress = Some(crate::state::LoadProgress {
                        label,
                        done,
                        total,
                        cached,
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
                DataMsg::Qpe { product, mosaics } => {
                    self.state.qpe_requested_at = None;
                    if let Some(wide) = mosaics.first() {
                        log::info!(
                            "QPE {}: {} images, {}x{} at {}",
                            product.label(),
                            mosaics.len(),
                            wide.width,
                            wide.height,
                            wide.time.format("%H:%MZ")
                        );
                    }
                    self.state.qpe = Some((mosaics, std::time::Instant::now()));
                    self.state.qpe_product = product;
                    self.pump_ai_data();
                }
                DataMsg::ModelField(field) => {
                    log::info!(
                        "{} field {:?} f{:02} valid {}: {}x{}",
                        field.model.label(),
                        field.kind,
                        field.hour,
                        field.valid.format("%H:%MZ"),
                        field.width,
                        field.height
                    );
                    self.state.model_field = Some((field, std::time::Instant::now()));
                    self.state.model_field_requested_at = None;
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
                DataMsg::FuturecastRun {
                    init,
                    bbox,
                    frames,
                    extended,
                } => {
                    // A late message from a previous site or horizon must not
                    // replace the current run.
                    let wanted = radar_core::sites::site(&self.state.site_id).map(|site| {
                        radar_data::futurecast::forecast_box(site.latitude, site.longitude)
                    });
                    if wanted.is_some_and(|wanted| wanted != bbox)
                        || extended != self.state.futurecast_extended
                    {
                        log::debug!("FutureCast: ignoring a stale run");
                        return;
                    }
                    self.state.futurecast_pending = true;
                    let previous_hour = self
                        .state
                        .futurecast
                        .as_ref()
                        .and_then(|fc| fc.selected_info().map(|info| info.hour));
                    let mut futurecast =
                        crate::state::FuturecastState::new(init, bbox, frames, extended);
                    if let Some(hour) = previous_hour
                        && let Some(index) = futurecast.plan.iter().position(|p| p.hour == hour)
                    {
                        futurecast.selected = index;
                    } else if let Some(index) = futurecast
                        .plan
                        .iter()
                        .position(|info| info.valid >= chrono::Utc::now())
                    {
                        // The strip exposes the whole model run, but open on the
                        // first frame that is actually in the future.
                        futurecast.selected = index;
                    }
                    log::info!(
                        "FutureCast: HRRR {} run, {}-hour {} frames ({}..{}Z)",
                        init.format("%Y-%m-%d %HZ"),
                        if extended { 48 } else { 18 },
                        futurecast.plan.len(),
                        futurecast
                            .plan
                            .first()
                            .map_or("?".into(), |f| f.valid.format("%H").to_string()),
                        futurecast
                            .plan
                            .last()
                            .map_or("?".into(), |f| f.valid.format("%H").to_string()),
                    );
                    self.state.futurecast = Some(futurecast);
                    self.state.status_text =
                        format!("FutureCast · HRRR {}Z run", init.format("%H"));
                }
                DataMsg::FuturecastFrame(frame) => {
                    let hour = frame.hour;
                    if let Some(futurecast) = self.state.futurecast.as_mut()
                        && frame.init == futurecast.init
                    {
                        futurecast.insert(frame);
                        // Keep the slot chosen when the run arrived (the
                        // first future hour). Frames download in order, so
                        // auto-selecting "the first loaded" here would snap
                        // the view back to f01 — often an hour already past.
                        log::debug!("FutureCast: f{hour:02} ready");
                    }
                    self.pump_ai_data();
                }
                DataMsg::FuturecastDone {
                    received,
                    total,
                    error,
                    extended,
                } => {
                    if extended != self.state.futurecast_extended {
                        log::debug!("FutureCast: ignoring a done message for the other horizon");
                        return;
                    }
                    self.state.futurecast_pending = false;
                    if let Some(futurecast) = self.state.futurecast.as_mut() {
                        futurecast.received_at = std::time::Instant::now();
                    }
                    if let Some(error) = error {
                        log::warn!("FutureCast: {error}");
                        self.state.status_text =
                            format!("FutureCast · {received}/{total} frames ({error})");
                    } else {
                        self.state.status_text =
                            format!("FutureCast · HRRR · {received}/{total} frames");
                    }
                    self.pump_ai_data();
                }
                DataMsg::AiBrief { kind, result } => match kind {
                    AiRequestKind::Chat => match result {
                        Ok(reply) => {
                            let (prose, action, look, data) =
                                crate::ai_actions::split_reply(&reply);
                            let can_look = self.state.ai_look_depth < 2;
                            let looked = can_look && look.is_some_and(|look| self.ai_look(look));
                            if !looked {
                                let can_fetch = self.state.ai_data_depth == 0;
                                let fetched = can_fetch
                                    && data.is_some_and(|data| self.ai_request_data(data));
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
                                " — the AI provider may be having a temporary outage; press Retry."
                            } else {
                                ""
                            };
                            self.ai_note(format!("Request failed: {error}{hint}"));
                        }
                    },
                    AiRequestKind::Live { scan, site, seq } => {
                        self.finish_live_ai(scan, site, seq, result);
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
                DataMsg::UpdateCheck(result) => {
                    log::info!("UpdateCheck received: {result:?}");
                    self.state.update_check_pending = false;
                    self.state.update_checked_at = Some(std::time::Instant::now());
                    match result {
                        Ok(Some(info)) => {
                            self.state.update_available = Some(info);
                            self.state.update_dialog_open = true;
                        }
                        Ok(None) => {
                            self.state.update_status = Some("Stormscope is up to date.".into());
                        }
                        Err(error) => {
                            self.state.update_status =
                                Some(format!("Update check failed: {error}"));
                        }
                    }
                }
                DataMsg::UpdateDownload(result) => {
                    self.state.update_download_pending = false;
                    match result {
                        Ok(path) => {
                            self.state.update_downloaded = Some(path.clone());
                            self.state.update_dialog_open = true;
                            self.state.update_status =
                                Some("Downloaded. Complete the Android install prompt.".into());
                            self.try_install_update(path);
                        }
                        Err(error) => {
                            self.state.update_status =
                                Some(format!("Update download failed: {error}"));
                        }
                    }
                }
                DataMsg::Lightning {
                    strikes,
                    satellite,
                    key,
                } => {
                    self.state.lightning_pending = false;
                    if self.state.lightning_last_key.as_deref() != Some(key.as_str()) {
                        self.state.lightning_last_key = Some(key);
                        let now = chrono::Utc::now();
                        let new_strikes = strikes;
                        // Bin new strikes by minute for the last 60 minutes.
                        use chrono::Timelike;
                        use std::collections::BTreeMap;
                        let mut by_min: BTreeMap<chrono::DateTime<chrono::Utc>, usize> =
                            BTreeMap::new();
                        for s in &new_strikes {
                            let minute =
                                match s.time.with_second(0).and_then(|t| t.with_nanosecond(0)) {
                                    Some(t) => t,
                                    None => continue,
                                };
                            if now - minute >= chrono::Duration::minutes(60) {
                                continue;
                            }
                            *by_min.entry(minute).or_insert(0) += 1;
                        }
                        // Merge into existing bins. Only add bins we haven't seen for this update.
                        for (m, c) in by_min {
                            let mut found = false;
                            for (bm, _bc) in self.state.lightning_bins.iter() {
                                if *bm == m {
                                    found = true;
                                    break;
                                }
                            }
                            if !found {
                                self.state.lightning_bins.push_back((m, c));
                            }
                        }
                        // Trim bins to last 60 minutes, then keep them ordered
                        // oldest-first (GLM files can arrive slightly late).
                        let bin_cutoff = now - chrono::Duration::minutes(60);
                        self.state
                            .lightning_bins
                            .make_contiguous()
                            .sort_by_key(|(m, _)| *m);
                        self.state.lightning_bins.retain(|(m, _)| *m >= bin_cutoff);
                        while self.state.lightning_bins.len() > 60 {
                            self.state.lightning_bins.pop_front();
                        }
                        self.state.lightning.extend(new_strikes);
                        let cutoff = chrono::Utc::now() - chrono::Duration::minutes(20);
                        self.state.lightning.retain(|s| s.time >= cutoff);
                        if self.state.lightning.len() > 12_000 {
                            let cut = self.state.lightning.len() - 12_000;
                            self.state.lightning.drain(..cut);
                        }
                        log::info!(
                            "lightning ({satellite}): {} flashes in memory, {} bins",
                            self.state.lightning.len(),
                            self.state.lightning_bins.len()
                        );
                    }
                    self.state.lightning_satellite = Some(satellite);
                }
                DataMsg::LightningError(error) => {
                    self.state.lightning_pending = false;
                    log::warn!("lightning: {error}");
                    self.state.status_text = format!("lightning: {error}");
                }
                DataMsg::CachedDeleted {
                    date,
                    removed,
                    bytes,
                } => {
                    log::info!(
                        "deleted {removed} cached scans for {date} ({:.1} MB)",
                        bytes as f64 / 1e6
                    );
                    self.state.downloads_confirm = None;
                    self.state.status_text = format!(
                        "deleted {removed} cached scans for {date} · {:.1} GB freed",
                        bytes as f64 / 1e9
                    );
                    self.refresh_cached_scans();
                    self.refresh_cached_days();
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

        self.drain_ai_fresh_queue();

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
        let futurecast_active = self.state.show_futurecast && self.state.futurecast.is_some();
        if ctx.input(|i| i.key_pressed(egui::Key::Space)) {
            if futurecast_active {
                // Space controls the forecast loop while FutureCast owns the
                // bottom bar.
                self.state.futurecast_playing = !self.state.futurecast_playing;
                self.state.futurecast_last_step = ctx.input(|i| i.time);
                if self.state.futurecast_playing {
                    self.state.loop_state = LoopState::Paused;
                }
            } else {
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
        }
        if ctx.input(|i| i.key_pressed(egui::Key::L)) {
            // Same as the timeline's "GO LIVE" pill.
            self.snap_to_live();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
            if futurecast_active {
                self.state.futurecast_step(1);
            } else {
                self.state.step_forward();
            }
        }
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
            if futurecast_active {
                self.state.futurecast_step(-1);
            } else {
                self.state.step_back();
            }
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
    /// The fallback question for a brief: the GPS fix is a sample, so a brief
    /// is valid (and useful) on a phone with no pins at all.
    fn brief_ask(&self) -> String {
        let markers = self.state.markers.len();
        let pins = format!("{} marker{}", markers, if markers == 1 { "" } else { "s" });
        match (self.state.gps_fix.is_some(), markers) {
            (true, 0) => "Write a brief for my current GPS location.".into(),
            (true, _) => format!("Write a brief for my current GPS location and my {pins}."),
            (false, _) => format!("Write a brief for my {pins}."),
        }
    }

    pub fn ai_request(&mut self, question: Option<String>) {
        if self.state.ai_pending {
            return;
        }
        let asked = question.filter(|q| !q.trim().is_empty());
        // Answer from fresh current weather: when a surface observation is on
        // screen but older than the freshness window, force a refresh and park
        // the question until the new observation lands (or a deadline passes,
        // so a slow NWS reply can never hang the request).
        let stale = self.state.observation.is_some()
            && self
                .state
                .observation_age_secs()
                .is_none_or(|age| age > AI_OBS_FRESH_SECS);
        if stale {
            let now = std::time::Instant::now();
            self.state.ai_pending = true;
            self.ai_fresh_queue = Some(asked);
            self.ai_fresh_started = Some(now);
            self.ai_fresh_deadline = Some(now + std::time::Duration::from_secs(6));
            log::info!(
                "AI: observation {:?} old — refreshing current conditions first",
                self.state.observation_age_secs().map(|a| a as i64)
            );
            crate::ui::forecast::request_forecast_now(self);
            return;
        }
        self.finish_ai_request(asked);
    }

    fn finish_ai_request(&mut self, asked: Option<String>) {
        let shown = asked.clone().unwrap_or_else(|| self.brief_ask());
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

    /// Release a question parked while the observation refresh was in flight:
    /// send it when fresh data landed, or on the deadline fall back to the
    /// cached snapshot rather than hanging the request.
    fn drain_ai_fresh_queue(&mut self) {
        let (Some(started), Some(deadline)) = (self.ai_fresh_started, self.ai_fresh_deadline)
        else {
            return;
        };
        let fresh = self
            .state
            .observation_fetched_at
            .is_some_and(|t| t >= started);
        if !fresh && std::time::Instant::now() < deadline {
            return;
        }
        // Outer Option = parked-or-not; flatten gives the inner question
        // (None means "use the current GPS/marker brief prompt").
        let asked = self.ai_fresh_queue.take().flatten();
        self.ai_fresh_started = None;
        self.ai_fresh_deadline = None;
        self.state.ai_pending = false;
        if fresh {
            log::info!("AI: answering with a fresh surface observation");
        } else {
            log::warn!("AI: current-conditions refresh timed out; using the cached snapshot");
        }
        self.finish_ai_request(asked);
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

    /// Start the Android installer, first sending the user to the
    /// "install unknown apps" page when that permission is missing. The update
    /// modal stays open either way so the user can try again after granting it.
    pub(crate) fn try_install_update(&mut self, path: std::path::PathBuf) {
        #[cfg(target_os = "android")]
        {
            if crate::android_lifecycle::can_install_packages() {
                crate::android_lifecycle::install_apk(&path);
                self.state.update_status =
                    Some("Opening the installer — finish the system prompt.".into());
            } else {
                self.state.update_status = Some(
                    "Allow Stormscope to install unknown apps, then tap Install again.".into(),
                );
                crate::android_lifecycle::open_install_settings();
            }
        }
        #[cfg(windows)]
        {
            self.state.update_status = Some("Restarting to install the update…".into());
            let _ = std::process::Command::new(&path)
                .arg("--silent")
                .arg("--update")
                .spawn();
            std::process::exit(0);
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            let Some(appimage) = std::env::var_os("APPIMAGE").map(std::path::PathBuf::from) else {
                self.state.update_status = Some(
                    "This build is not an AppImage. Download the new release from GitHub.".into(),
                );
                return;
            };
            let file_name = appimage
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "stormscope.AppImage".into());
            let temp = appimage.with_file_name(format!(".{file_name}.new"));
            let old = appimage.with_file_name(format!(".{file_name}.old"));
            let install = (|| -> std::io::Result<()> {
                std::fs::copy(&path, &temp)?;
                std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))?;
                if old.exists() {
                    let _ = std::fs::remove_file(&old);
                }
                std::fs::rename(&appimage, &old)?;
                if let Err(error) = std::fs::rename(&temp, &appimage) {
                    let _ = std::fs::rename(&old, &appimage);
                    return Err(error);
                }
                let _ = std::fs::remove_file(&old);
                Ok(())
            })();
            match install {
                Ok(()) => {
                    self.state.update_status = Some("Restarting into the new version…".into());
                    let _ = std::process::Command::new(&appimage).spawn();
                    std::process::exit(0);
                }
                Err(error) => {
                    self.state.update_status = Some(format!(
                        "Could not replace the AppImage ({}). Download it from GitHub instead.",
                        error
                    ));
                }
            }
        }
        #[cfg(not(any(target_os = "android", windows, target_os = "linux")))]
        {
            let _ = path;
        }
    }

    /// Undo the AI's map manoeuvre automatically an hour after it happened.
    fn auto_restore_ai_view(&mut self, ctx: &egui::Context) {
        let Some(at) = self.state.ai_view_restore_at else {
            return;
        };
        let now = std::time::Instant::now();
        if now < at {
            ctx.request_repaint_after(at - now);
            return;
        }
        if crate::ai_actions::restore(&mut self.state) {
            self.state.ai_actions = None;
            log::info!("AI map view auto-restored after 60 minutes");
        }
    }

    // ---- Android Gemini Live voice conversation ------------------------

    /// A short plain-language summary of the data the voice model has. The
    /// full JSON follows it; putting the observation and forecast first keeps
    /// the model from treating the session as an open-ended chat.
    #[cfg(target_os = "android")]
    fn voice_summary(&self) -> String {
        let mut out = String::new();
        if let Some((lat, lon)) = self.state.gps_fix {
            out.push_str(&format!("User's GPS fix: {lat:.4}, {lon:.4}.\n"));
        }
        if !self.state.markers.is_empty() {
            out.push_str(&format!(
                "Manual map markers: {}.\n",
                self.state.markers.len()
            ));
        }
        out.push_str("RAINFALL / SNOW TOTALS (inches, from NWS stations near the user):\n");
        if self.state.totals.is_empty() {
            out.push_str("- No station totals are loaded for this point.\n");
        } else {
            for station in self.state.totals.iter().take(5) {
                let mut parts: Vec<String> = station
                    .buckets
                    .iter()
                    .take(4)
                    .map(|(label, value)| format!("{label} {value:.2} in"))
                    .collect();
                parts.extend(
                    station
                        .hourly_sums
                        .iter()
                        .take(3)
                        .map(|(label, value)| format!("{label} {value:.2} in")),
                );
                if parts.is_empty() {
                    out.push_str(&format!(
                        "- {} ({}): no rain/snow values reported.\n",
                        station.station_id, station.station_name
                    ));
                } else {
                    out.push_str(&format!(
                        "- {} ({}): {}.\n",
                        station.station_id,
                        station.station_name,
                        parts.join(", ")
                    ));
                }
            }
            out.push_str(
                "Use these station values for every rain or snow accumulation question. \
                 0.00 means the station reported no measurable precipitation. Never say you \
                 lack access to rainfall or snow totals while this section lists stations.\n",
            );
        }
        if let Some(fc) = &self.state.futurecast
            && fc.loaded() > 0
        {
            out.push_str(&format!(
                "HRRR FutureCast ({}h run, initialized {}Z, {} frames loaded):\n",
                if fc.extended { 48 } else { 18 },
                fc.init.format("%H"),
                fc.loaded()
            ));
            let mut points: Vec<(String, f64, f64)> = Vec::new();
            if let Some((lat, lon)) = self.state.gps_fix {
                points.push(("GPS".into(), lat, lon));
            }
            for m in self.state.markers.iter().take(2) {
                points.push((m.display(), m.lat, m.lon));
            }
            for (label, lat, lon) in points {
                let mut line = String::new();
                for (info, frame) in fc.plan.iter().zip(fc.frames.iter()) {
                    let Some(frame) = frame else { continue };
                    let Some(dbz) = frame.dbz_at(lat, lon) else {
                        continue;
                    };
                    if dbz < 5.0 {
                        continue;
                    }
                    let ptype = match frame.ptype_at(lat, lon) {
                        Some(2) => " snow",
                        Some(4) => " ice",
                        Some(1) => " rain",
                        _ => "",
                    };
                    line.push_str(&format!(
                        " {}Z {:.0}dBZ{};",
                        info.valid.format("%H"),
                        dbz,
                        ptype
                    ));
                    if line.len() > 220 {
                        break;
                    }
                }
                if line.is_empty() {
                    out.push_str(&format!(
                        "- {label}: no simulated echo above 5 dBZ in the loaded frames.\n"
                    ));
                } else {
                    out.push_str(&format!("- {label}:{line}\n"));
                }
            }
            out.push_str(
                "FutureCast is an HRRR model forecast, not observed radar; use it for \
                 arrival timing and always call it a forecast.\n",
            );
        }
        if let Some(obs) = self.state.observation.as_ref() {
            let to_f = |c: f32| c * 9.0 / 5.0 + 32.0;
            let to_mph = |kmh: f32| kmh * 0.621_371;
            out.push_str(&format!(
                "Current observation at {} ({}): {}",
                obs.station_id, obs.station_name, obs.text
            ));
            if let Some(t) = obs.temperature_c {
                out.push_str(&format!(", {:.0}°F", to_f(t)));
            }
            if let Some(d) = obs.dewpoint_c {
                out.push_str(&format!(", dew point {:.0}°F", to_f(d)));
            }
            if let Some(w) = obs.wind_speed_kmh {
                out.push_str(&format!(", wind {:.0} mph", to_mph(w)));
            }
            if let Some(g) = obs.wind_gust_kmh {
                out.push_str(&format!(", gusting {:.0} mph", to_mph(g)));
            }
            if let Some(h) = obs.humidity_pct {
                out.push_str(&format!(", humidity {h:.0}%"));
            }
            out.push_str(".\n");
        }
        if let Some(forecast) = self.state.forecast.as_ref() {
            out.push_str(&format!(
                "NWS forecast for {} (office {}):\n",
                forecast.place, forecast.office
            ));
            for p in forecast.periods.iter().take(8) {
                let temp_f = if p.unit.eq_ignore_ascii_case("C") {
                    p.temperature * 9.0 / 5.0 + 32.0
                } else {
                    p.temperature
                };
                out.push_str(&format!(
                    "- {}: {:.0}°F, {}, wind {}\n",
                    p.name, temp_f, p.short, p.wind
                ));
            }
        } else {
            out.push_str("No NWS forecast is loaded for the user's point.\n");
        }
        let active_sounding = match self.state.sounding_kind {
            crate::state::SoundingKind::Model => self.state.sounding.as_ref(),
            crate::state::SoundingKind::Rap => self.state.rap_sounding.as_ref(),
            crate::state::SoundingKind::Observed => self.state.observed_sounding.as_ref(),
        };
        if let Some(s) = active_sounding {
            let to_f = |c: f64| c * 9.0 / 5.0 + 32.0;
            let to_ft = |m: f64| m * 3.280_84;
            let kind_label = match self.state.sounding_kind {
                crate::state::SoundingKind::Model => "Model sounding (Open-Meteo, not observed)",
                crate::state::SoundingKind::Rap => "RAP analysis profile (not observed)",
                crate::state::SoundingKind::Observed => "Observed RAOB",
            };
            out.push_str(&format!(
                "{kind_label}: surface {:.0}°F / dew {:.0}°F, \
                 CAPE {:.0} J/kg, CIN {:.0} J/kg, PWAT {:.1} mm, LCL {:.0} ft, \
                 0C {:.0} ft, shear 0-6 km {:.0} mph, SRH 0-3 km {:.0} m2/s2.\n",
                to_f(s.surface_temp_c),
                to_f(s.surface_dewpoint_c),
                s.sb_cape_jkg,
                s.sb_cin_jkg,
                s.pwat_mm,
                s.lcl_height_m.map(to_ft).unwrap_or(0.0),
                s.freezing_level_m.map(to_ft).unwrap_or(0.0),
                s.shear_0_6km_ms * 2.236_94,
                s.srh_0_3km,
            ));
            out.push_str(&format!(
                "Heuristic environment indicators: severe {:.0}/100 ({}), tornado {:.0}/100 ({}), \
                 winter {:.0}/100 ({}, type {}). These are not official forecasts.\n",
                s.severe_potential(),
                radar_core::sounding::Sounding::potential_label(s.severe_potential()),
                s.tornado_potential(),
                radar_core::sounding::Sounding::potential_label(s.tornado_potential()),
                s.winter_potential(),
                radar_core::sounding::Sounding::potential_label(s.winter_potential()),
                s.winter_type(),
            ));
        }
        if let (Some(site), Some(frame)) = (
            radar_core::sites::site(&self.state.site_id),
            self.state.current(),
        ) {
            let rotation = radar_core::severe::rotation_signatures(
                &frame.volume,
                site.latitude,
                site.longitude,
            );
            if !rotation.is_empty() {
                out.push_str("Low-level rotation signatures (experimental, single radar):\n");
                for sig in rotation.iter().take(4) {
                    out.push_str(&format!(
                        "- {} at {:.3},{:.3}, {:.0} mph delta-V, {:.1}° tilt{}.\n",
                        if sig.tvs { "TVS?" } else { "MESO" },
                        sig.lat,
                        sig.lon,
                        sig.delta_v_ms * 2.236_94,
                        sig.elevation_deg,
                        if sig.tvs { ", low-level" } else { "" },
                    ));
                }
            }
            let cores =
                radar_core::severe::hail_cores(&frame.volume, site.latitude, site.longitude);
            if !cores.is_empty() {
                out.push_str("Hail cores (MEHS, experimental):\n");
                for c in cores.iter().take(5) {
                    out.push_str(&format!(
                        "- {:.2} in at {:.3},{:.3}.\n",
                        c.mehs_in, c.lat, c.lon
                    ));
                }
            }
        }
        let alerts: Vec<&str> = self
            .state
            .alerts
            .iter()
            .take(5)
            .map(|alert| alert.event.as_str())
            .collect();
        if alerts.is_empty() {
            out.push_str("No active alerts near the radar site.\n");
        } else {
            out.push_str(&format!(
                "Active alerts near the radar site: {}.\n",
                alerts.join("; ")
            ));
        }
        out
    }

    /// Build the system instruction for a live voice session. The short
    /// summary is first, then the full JSON snapshot the typed AI uses.
    #[cfg(target_os = "android")]
    fn voice_prompt(&self) -> Result<String, String> {
        let brief = self.build_brief(&[])?;
        let summary = self.voice_summary();
        let mut prompt = String::with_capacity(brief.user.len() + summary.len() + 2200);
        prompt.push_str(
            "You are Stormscope's meteorologist, speaking live with a storm spotter by voice.\n\
             ALWAYS SPEAK US ENGLISH, even if you hear another language, an accent, or \
             background noise. Keep the SAME voice for the whole session: same pitch, accent \
             and speaking style. Never switch to another voice, imitate the user, or change \
             accent mid-answer.\n\n\
             You already have all of the weather data below. Answer from it. NEVER ask the \
             user to report conditions, tell you what they are observing, or provide weather \
             data — you are the meteorologist with the radar, warnings, observation and \
             forecast. If something is genuinely absent from the snapshot, name that one \
             missing thing and answer with what you do have. Never say you do not have \
             access to the forecast when a forecast appears in the summary below.\n\n\
             Talk like a meteorologist on the phone: natural, concise, 2-4 sentences per \
             turn (under 80 words), plain spoken English. No lists, headings, markdown, \
             JSON, or radar jargon unless the user uses it first. Use miles, feet, °F and \
             the local times in the data. Rain and snow totals are in the RAINFALL / SNOW \
             TOTALS section at the top of the summary and in `rain_snow_totals` (inches). \
             Use them for every accumulation question. If the stations report 0.00, say no \
             measurable precipitation was reported; never say you lack access to totals. \
             Do not invent data. The user may interrupt you; \
             stop immediately if they do. Do not greet or speak until the user speaks first. \
             If the transcription of the user's speech is garbled, ask one short clarifying \
             question instead of guessing. You can move the map, inspect other areas, fetch extra \
             data and mark features with the provided tools when it helps answer; keep spoken \
             replies short and say what you are showing. Do not narrate intermediate steps: \
             while a fetch_data call is running, say ONE short line that the data is downloading \
             and then stay silent until the tool result arrives. Never repeat that you are \
             checking or downloading. The standard HRRR FutureCast run is 18 hours; if the user \
             asks for the 48-hour extended outlook, call fetch_data with futurecast true and \
             futurecast_hours 48. A run can take 30-60 seconds to download, so tell the user once.\n\n\
             CURRENT SUMMARY:\n",
        );
        prompt.push_str(&summary);
        prompt.push_str(&format!("\nSnapshot: {}\n\n", brief.label));
        prompt.push_str("DETAILED SNAPSHOT (JSON):\n");
        prompt.push_str(&brief.user);
        Ok(prompt)
    }

    /// Open the WebSocket once the freshest forecast has landed.
    #[cfg(target_os = "android")]
    fn begin_voice_session(&mut self) -> Result<(), String> {
        let prompt = self.voice_prompt()?;
        log::debug!("Gemini Live summary:\n{}", self.voice_summary());
        log::info!(
            "Gemini Live snapshot: {} bytes (forecast={}, observation={}, totals={}, alerts={}, markers={})",
            prompt.len(),
            self.state.forecast.is_some(),
            self.state.observation.is_some(),
            self.state.totals.len(),
            self.state.alerts.len(),
            self.state.markers.len()
        );
        let config = crate::gemini_live::LiveConfig {
            api_key: self.state.gemini_api_key.trim().to_string(),
            model: self.state.gemini_model.trim().to_string(),
            voice: self.state.gemini_voice.clone(),
            system: prompt,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.live = Some(crate::gemini_live::LiveSession::start(config, tx));
        self.live_rx = Some(rx);
        self.state.voice_status = Some("Connecting to Gemini Live…".into());
        self.state.voice_level = 0.0;
        self.state.voice_polled_at = None;
        self.state.voice_listening = false;
        self.state.voice_speaking = false;
        self.state.voice_last_activity = Some(std::time::Instant::now());
        Ok(())
    }

    /// Start a Gemini Live voice conversation. Android only: the audio
    /// capture/playback bridge lives in MainActivity + GeminiLiveAudio.
    #[cfg(target_os = "android")]
    pub fn start_voice_convo(&mut self) {
        if self.state.voice_convo {
            return;
        }
        if self.state.gemini_api_key.trim().is_empty() {
            self.state.ai_settings_open = true;
            self.ai_note(
                "Add a Gemini API key in Settings → AI meteorologist → Gemini Live, then tap Talk."
                    .into(),
            );
            return;
        }
        if self.state.markers.is_empty() && self.state.gps_fix.is_none() {
            self.ai_note(
                "Waiting for a GPS fix — place a marker or use the GPS button, then tap Talk."
                    .into(),
            );
            return;
        }
        self.state.ai_open = true;
        self.state.voice_convo = true;
        self.state.voice_listening = false;
        self.state.voice_speaking = false;
        self.state.voice_partial.clear();
        self.state.voice_user_text.clear();
        self.state.voice_reply_text.clear();
        self.state.voice_level = 0.0;
        self.state.voice_polled_at = None;
        self.state.ai_snapshot = Some(format!("Gemini Live · {}", self.state.gemini_model));
        if self.state.forecast_point.is_none() {
            let gps = self.state.gps_fix;
            self.state.forecast_point = gps.or_else(|| {
                self.state
                    .markers
                    .first()
                    .map(|marker| (marker.lat, marker.lon))
            });
            self.state.forecast_point_gps = gps.is_some();
        }
        self.state.voice_pending_start = true;
        self.state.voice_pending_at = Some(std::time::Instant::now());
        self.state.voice_last_activity = Some(std::time::Instant::now());
        self.state.voice_status = Some("Loading the forecast and rain/snow totals…".into());
        // A voice brief must know the forecast, current conditions and
        // precipitation totals, not just the radar. The Live model cannot
        // issue the typed AI's DATA lookups, so fetch both before opening
        // the socket.
        crate::ui::forecast::request_forecast_now(self);
        let totals_target = self
            .state
            .gps_fix
            .or_else(|| {
                self.state
                    .markers
                    .first()
                    .map(|marker| (marker.lat, marker.lon))
            })
            .or(self.state.forecast_point);
        self.state.voice_totals_wanted = totals_target.is_some();
        if let Some((lat, lon)) = totals_target {
            crate::ui::totals::request_totals_now(self, lat, lon);
        }
    }

    /// End the live session and release the microphone / speaker.
    #[cfg(target_os = "android")]
    pub fn end_voice_convo(&mut self) {
        self.state.voice_convo = false;
        self.state.voice_listening = false;
        self.state.voice_speaking = false;
        self.state.voice_partial.clear();
        self.state.voice_user_text.clear();
        self.state.voice_reply_text.clear();
        self.state.voice_status = None;
        self.state.voice_level = 0.0;
        self.state.voice_polled_at = None;
        self.state.voice_pending_start = false;
        self.state.voice_pending_at = None;
        self.state.voice_totals_wanted = false;
        self.state.voice_last_activity = None;
        self.live_rx = None;
        if let Some(mut live) = self.live.take() {
            live.stop();
        }
    }

    /// Add the current voice turn to the visible transcript.
    #[cfg(target_os = "android")]
    fn voice_push_turn(&mut self) {
        let user = std::mem::take(&mut self.state.voice_user_text);
        let reply = std::mem::take(&mut self.state.voice_reply_text);
        if !user.trim().is_empty() {
            self.ai_push(AiRole::User, user.trim().to_string());
        }
        if !reply.trim().is_empty() {
            self.ai_push(AiRole::Assistant, reply.trim().to_string());
        }
    }

    #[cfg(target_os = "android")]
    fn respond_live_tool(&mut self, id: String, name: String, result: serde_json::Value) {
        if let Some(live) = &self.live {
            live.respond_tool(id, name, result);
        }
        self.live_pending_tool = None;
        // A live tool answer is not a text-AI request: stop the text
        // window's spinner and clear its "looking at" line.
        self.state.ai_pending = false;
        self.state.ai_looking_at = None;
    }

    #[cfg(target_os = "android")]
    fn live_look(&mut self, id: String, look: crate::ai_actions::LookRequest) {
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
            self.respond_live_tool(
                id,
                "look".into(),
                serde_json::json!({ "ok": false, "error": "no valid points" }),
            );
            return;
        }
        let names = points
            .iter()
            .map(|p| p.label.clone())
            .collect::<Vec<_>>()
            .join(", ");
        let (center_lat, center_lon, zoom) = self.fit_look_view(&points);
        let action = crate::ai_actions::MapAction {
            center_lat: Some(center_lat),
            center_lon: Some(center_lon),
            zoom: Some(zoom),
            product: look.product.clone(),
            ..Default::default()
        };
        let _ = crate::ai_actions::apply(&action, &mut self.state);
        self.state.ai_compact = true;
        // Site switch if a closer radar exists.
        if let Some(best) = self.closest_site_for(&points) {
            let current_id = self.state.site_id.clone();
            if best != current_id {
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
                self.state.mode = crate::state::Mode::Live;
                self.state.loop_state = crate::state::LoopState::Paused;
                self.select_site(&best);
                self.live_pending_tool = Some(LivePendingTool {
                    id,
                    name: "look".into(),
                });
                return;
            }
        }
        self.state.ai_looking_at = Some(names);
        self.respond_live_tool(
            id,
            "look".into(),
            serde_json::json!({ "ok": true, "summary": self.voice_summary() }),
        );
    }

    #[cfg(target_os = "android")]
    fn live_fetch_data(&mut self, id: String, request: crate::ai_actions::DataRequest) {
        if self.ai_request_data(request) {
            self.ai_data_from_live = true;
            self.live_pending_tool = Some(LivePendingTool {
                id,
                name: "fetch_data".into(),
            });
        } else {
            self.respond_live_tool(
                id,
                "fetch_data".into(),
                serde_json::json!({ "ok": true, "summary": self.voice_summary() }),
            );
        }
    }

    /// Drain Gemini Live events and keep the voice footer / mic meter fed.
    #[cfg(target_os = "android")]
    fn pump_gemini_live(&mut self) {
        if self.state.voice_convo && !self.state.ai_open {
            self.end_voice_convo();
            return;
        }
        if self.state.voice_pending_start {
            let now = std::time::Instant::now();
            let started = self.state.voice_pending_at.unwrap_or(now);
            let forecast_fresh = self
                .state
                .forecast_fetched_at
                .is_some_and(|fetched| fetched >= started);
            let totals_fresh = !self.state.voice_totals_wanted
                || self
                    .state
                    .totals_fetched_at
                    .is_some_and(|fetched| fetched >= started);
            let fresh = forecast_fresh && totals_fresh;
            if fresh || now.duration_since(started) > std::time::Duration::from_secs(20) {
                self.state.voice_pending_start = false;
                self.state.voice_pending_at = None;
                if !fresh {
                    log::warn!(
                        "Gemini Live: forecast/totals refresh did not land in 20 s; using the snapshot available"
                    );
                }
                if let Err(message) = self.begin_voice_session() {
                    self.ai_note(message);
                    self.end_voice_convo();
                }
            }
            return;
        }
        let events: Vec<crate::gemini_live::LiveEvent> = self
            .live_rx
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            use crate::gemini_live::LiveEvent;
            match event {
                LiveEvent::Status(status) => {
                    self.state.voice_status = Some(status);
                }
                LiveEvent::Listening => {
                    self.state.voice_listening = true;
                    self.state.voice_speaking = false;
                    self.state.voice_last_activity = Some(std::time::Instant::now());
                    self.state.voice_status = Some("Listening — just talk".into());
                }
                LiveEvent::UserPartial(text) => {
                    self.state.voice_last_activity = Some(std::time::Instant::now());
                    self.state.voice_partial = text;
                }
                LiveEvent::UserFinal(text) => {
                    self.state.voice_last_activity = Some(std::time::Instant::now());
                    let text = text.trim();
                    if !text.is_empty() {
                        // The Live transcription can repeat a cumulative
                        // segment; keep the newest full version.
                        if self.state.voice_user_text.is_empty()
                            || text.starts_with(&self.state.voice_user_text)
                        {
                            self.state.voice_user_text = text.to_string();
                        } else if !self.state.voice_user_text.ends_with(text) {
                            self.state.voice_user_text.push(' ');
                            self.state.voice_user_text.push_str(text);
                        }
                    }
                    self.state.voice_partial.clear();
                }
                LiveEvent::AssistantPartial(text) => {
                    self.state.voice_speaking = true;
                    self.state.voice_last_activity = Some(std::time::Instant::now());
                    let text = text.trim();
                    if !text.is_empty() {
                        if self.state.voice_reply_text.is_empty()
                            || text.starts_with(&self.state.voice_reply_text)
                        {
                            self.state.voice_reply_text = text.to_string();
                        } else if !self.state.voice_reply_text.ends_with(text) {
                            self.state.voice_reply_text.push(' ');
                            self.state.voice_reply_text.push_str(text);
                        }
                    }
                    self.state.voice_status = Some("Speaking…".into());
                }
                LiveEvent::Interrupted => {
                    self.state.voice_speaking = false;
                    self.state.voice_last_activity = Some(std::time::Instant::now());
                    self.state.voice_status = Some("Listening — just talk".into());
                }
                LiveEvent::TurnComplete => {
                    self.state.voice_speaking = false;
                    self.state.voice_last_activity = Some(std::time::Instant::now());
                    self.state.voice_status = Some("Listening — just talk".into());
                    self.voice_push_turn();
                    self.state.voice_partial.clear();
                }
                LiveEvent::ToolCall { id, name, args } => match name.as_str() {
                    "move_view" => {
                        let action: crate::ai_actions::MapAction =
                            serde_json::from_value(args).unwrap_or_default();
                        let summary = crate::ai_actions::apply(&action, &mut self.state);
                        self.state.ai_compact = true;
                        if let Some(live) = &self.live {
                            live.respond_tool(
                                id,
                                name,
                                serde_json::json!({ "ok": true, "summary": summary }),
                            );
                        }
                    }
                    "look" => {
                        let look: crate::ai_actions::LookRequest =
                            serde_json::from_value(args).unwrap_or_default();
                        self.live_look(id, look);
                    }
                    "fetch_data" => {
                        let request: crate::ai_actions::DataRequest =
                            serde_json::from_value(args).unwrap_or_default();
                        self.live_fetch_data(id, request);
                    }
                    _ => {
                        if let Some(live) = &self.live {
                            live.respond_tool(
                                id,
                                name,
                                serde_json::json!({ "ok": false, "error": "unknown tool" }),
                            );
                        }
                    }
                },
                LiveEvent::Error(error) => {
                    self.ai_note(error);
                    self.end_voice_convo();
                    return;
                }
                LiveEvent::Ended => {
                    if self.state.voice_convo {
                        self.ai_note(
                            "The Gemini Live session ended. Tap Talk to start a fresh one.".into(),
                        );
                        self.end_voice_convo();
                    }
                    return;
                }
            }
        }
        if self.state.voice_convo {
            let now = std::time::Instant::now();
            if self.state.voice_listening
                && !self.state.voice_speaking
                && self.state.voice_last_activity.is_some_and(|last| {
                    now.duration_since(last) > std::time::Duration::from_secs(60)
                })
            {
                log::info!("Gemini Live: 60 s of silence — ending the voice session");
                self.ai_note(
                    "Live AI session ended after 60 seconds of silence. Tap Talk to start a new one."
                        .into(),
                );
                self.end_voice_convo();
                return;
            }
            if self
                .state
                .voice_polled_at
                .is_none_or(|t| now.duration_since(t) > std::time::Duration::from_millis(100))
            {
                self.state.voice_polled_at = Some(now);
                self.state.voice_level = crate::android_live_audio::level();
            }
        }
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
        let want_extended = request
            .futurecast_hours
            .map(|h| h >= 48)
            .unwrap_or(self.state.futurecast_extended);
        if request.futurecast == Some(true)
            && self
                .state
                .futurecast
                .as_ref()
                .is_none_or(|fc| fc.loaded() == 0 || fc.extended != want_extended)
        {
            let Some(site) = radar_core::sites::site(&self.state.site_id) else {
                return false;
            };
            if !radar_data::futurecast::supported(site.latitude, site.longitude) {
                self.ai_note("HRRR FutureCast covers the CONUS only.".into());
                return false;
            }
            self.state.futurecast_extended = want_extended;
            self.state.show_futurecast = true;
            self.state.futurecast_pending = true;
            self.state.futurecast_requested_at = Some(std::time::Instant::now());
            self.state.futurecast_site = self.state.site_id.clone();
            let bbox = radar_data::futurecast::forecast_box(site.latitude, site.longitude);
            let _ = self.tx.send(DataCmd::LoadFuturecast {
                bbox,
                extended: want_extended,
            });
            waiting.push("futurecast".into());
            let horizon = if want_extended { "48-hour" } else { "18-hour" };
            notes.push(format!(
                "HRRR FutureCast ({horizon} run — downloading, this can take 30–60 s)"
            ));
        }
        if let Some(point) = &request.forecast {
            self.state.forecast = None;
            self.state.uv_index = None;
            self.state.observation = None;
            self.state.forecast_point = Some((point.lat, point.lon));
            self.state.forecast_point_gps = false;
            self.state.forecast_requested = Some((point.lat, point.lon));
            let _ = self.tx.send(DataCmd::LoadForecast(point.lat, point.lon));
            waiting.push("forecast".into());
            notes.push(match point.label.as_deref().map(str::trim) {
                Some(label) if !label.is_empty() => format!("NWS forecast for {label}"),
                _ => "NWS forecast".into(),
            });
        }
        if let Some(point) = &request.totals {
            self.state.totals.clear();
            self.state.totals_target = Some((point.lat, point.lon));
            self.state.totals_requested = Some((point.lat, point.lon, std::time::Instant::now()));
            let _ = self.tx.send(DataCmd::LoadTotals(point.lat, point.lon));
            waiting.push("totals".into());
            notes.push(match point.label.as_deref().map(str::trim) {
                Some(label) if !label.is_empty() => format!("station totals for {label}"),
                _ => "station totals".into(),
            });
        }
        if let Some(point) = &request.sounding {
            self.state.sounding = None;
            self.state.sounding_error = None;
            self.state.sounding_kind = crate::state::SoundingKind::Model;
            self.state.sounding_point = Some((point.lat, point.lon));
            self.state.sounding_point_gps = false;
            self.state.sounding_requested = Some((point.lat, point.lon));
            self.state.sounding_open = true;
            let _ = self.tx.send(DataCmd::LoadSounding(point.lat, point.lon));
            waiting.push("sounding".into());
            notes.push(match point.label.as_deref().map(str::trim) {
                Some(label) if !label.is_empty() => format!("model sounding for {label}"),
                _ => "model sounding".into(),
            });
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
        let mut looking = format!("loading {}", notes.join(", "));
        if waiting.iter().any(|l| l == "futurecast") {
            looking.push_str(" — FutureCast downloads a full HRRR run; this usually takes 30–60 s");
        }
        self.state.ai_looking_at = Some(looking);
        self.state.ai_pending_data = waiting;
        self.state.ai_pending_data_at = Some(std::time::Instant::now());
        self.state.ai_pending = true;
        self.ai_note(format!("Fetching {} for the analysis…", notes.join(", ")));
        true
    }

    /// Re-ask when every requested layer has landed (or the wait timed out).
    fn pump_ai_data(&mut self) {
        if self.state.ai_pending_data.is_empty() {
            return;
        }
        // A 48-hour FutureCast run can take a minute or more to deliver its
        // first frames; do not time it out on the short 45 s budget used by
        // the other layers.
        let timeout_secs = if self.state.ai_pending_data.iter().any(|l| l == "futurecast") {
            120
        } else {
            45
        };
        let timed_out = self
            .state
            .ai_pending_data_at
            .is_some_and(|at| at.elapsed() > std::time::Duration::from_secs(timeout_secs));
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
        let from_live = self.ai_data_from_live;
        self.ai_data_from_live = false;
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
        if let Some(tool) = self.live_pending_tool.take() {
            #[cfg(target_os = "android")]
            self.respond_live_tool(
                tool.id,
                tool.name,
                serde_json::json!({ "ok": true, "summary": self.voice_summary() }),
            );
            #[cfg(not(target_os = "android"))]
            let _ = tool;
        } else if from_live {
            // The voice session ended while the data was downloading; drop
            // the request instead of turning it into a text-AI question.
            self.state.ai_pending = false;
        } else {
            self.state.ai_pending = true;
            let question = self.state.ai_question_ctx.clone();
            let look = self.last_look_samples();
            self.send_ai(question, look);
        }
    }

    fn ai_data_ready(&self, layer: &str) -> bool {
        match layer {
            "mrms" => self.state.mrms.is_some(),
            "goes" => self.state.goes.is_some(),
            "futurecast" => {
                self.state.futurecast.as_ref().is_some_and(|fc| {
                    fc.loaded() > 0 && fc.extended == self.state.futurecast_extended
                })
            }
            "forecast" => self.state.forecast.is_some(),
            "totals" => !self.state.totals.is_empty(),
            "sounding" => self.state.sounding.is_some(),
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
            if let Some(tool) = self.live_pending_tool.take() {
                #[cfg(target_os = "android")]
                self.respond_live_tool(
                    tool.id,
                    tool.name,
                    serde_json::json!({ "ok": true, "summary": self.voice_summary() }),
                );
                #[cfg(not(target_os = "android"))]
                let _ = tool;
            } else {
                self.send_ai(self.state.ai_question_ctx.clone(), samples);
            }
            return;
        }
        self.state.ai_looking_at = None;
        self.state.ai_pending = false;
        if let Some(tool) = self.live_pending_tool.take() {
            #[cfg(target_os = "android")]
            self.respond_live_tool(
                tool.id,
                tool.name,
                serde_json::json!({ "ok": false, "error": format!("Could not load {site}") }),
            );
            #[cfg(not(target_os = "android"))]
            let _ = tool;
        }
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

    /// Build the compact weather snapshot the AI sees: decoded radar, storm
    /// tracks, warnings, outlook, the forecast and observation, plus the GPS
    /// sample and markers. `look` adds the extra areas a LOOK round sampled.
    fn build_brief(&self, look: &[LookPointSample]) -> Result<crate::brief::Brief, String> {
        // A GPS fix is a sample too: the brief always covers where the
        // spotter is, so no pin is needed on a phone.
        if self.state.markers.is_empty() && self.state.gps_fix.is_none() {
            return Err(
                "Still waiting for a GPS fix — place a marker, or try the GPS button.".into(),
            );
        }
        self.build_brief_inner(look, None, None)
    }

    /// The full snapshot builder shared by manual chat and the automatic live
    /// update. The live path passes the tracks and ETAs it just computed so
    /// both sides of the prompt describe the same scan.
    fn build_brief_inner(
        &self,
        look: &[LookPointSample],
        tracks_override: Option<&[radar_render::StormTrack]>,
        marker_etas_override: Option<&[Option<MarkerEta>]>,
    ) -> Result<crate::brief::Brief, String> {
        let Some(site) = radar_core::sites::site(&self.state.site_id) else {
            return Err("No radar site is selected.".into());
        };
        let Some(volume) = self.state.current().map(|f| Arc::clone(&f.volume)) else {
            return Err("No radar volume is loaded yet.".into());
        };
        // The map computes tracks while drawing, but an AI question can
        // arrive before the first draw (especially right after a site
        // switch). Derive from the two newest scans here if needed.
        let tracks_owned: Vec<radar_render::StormTrack> = if let Some(tracks) = tracks_override {
            tracks.to_vec()
        } else if let Some((_, tracks)) = &self.tracks_cache {
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
        let marker_etas = marker_etas_override.unwrap_or(&self.marker_etas);
        let mrms = self.state.mrms.as_ref().map(|(m, _)| m.as_slice());
        let goes = self.state.goes.as_ref().map(|(i, _)| i.as_slice());
        let srm = self.state.srm_cache.as_ref().map(|(_, v)| v.as_ref());
        let rotation =
            radar_core::severe::rotation_signatures(&volume, site.latitude, site.longitude);
        let hail_volumes: Vec<&radar_core::Volume> =
            self.state.ring.iter().map(|f| f.volume.as_ref()).collect();
        let hail = radar_core::severe::hail_swath(&hail_volumes, site.latitude, site.longitude);
        let vad = radar_core::vad::vad_profile(&volume);
        Ok(crate::brief::build(&crate::brief::BriefInputs {
            site,
            mode: match self.state.mode {
                crate::state::Mode::Live => "live",
                crate::state::Mode::Archive => "archive",
            },
            volume: &volume,
            min_dbz: self.state.min_dbz,
            markers: &self.state.markers,
            marker_etas,
            tracks,
            alerts: &self.state.alerts,
            outlook: self.state.outlook.as_ref(),
            mcds: &self.state.mcds,
            lsrs: &self.state.lsrs,
            forecast: self.state.forecast.as_ref(),
            forecast_point: self.state.forecast_point,
            gps_fix: self.state.gps_fix,
            forecast_age_min: self
                .state
                .forecast_fetched_at
                .map(|t| t.elapsed().as_secs() / 60),
            observation: self.state.observation.as_ref(),
            totals: &self.state.totals,
            sounding: self.state.sounding.as_ref(),
            sounding_age_min: self
                .state
                .sounding_fetched_at
                .map(|t| t.elapsed().as_secs() / 60),
            rotation: &rotation,
            hail: &hail,
            vad: Some(&vad),
            qpe: self.state.qpe.as_ref().map(|(m, _)| m.as_slice()),
            qpe_age_min: self
                .state
                .qpe
                .as_ref()
                .map(|(_, at)| at.elapsed().as_secs() / 60),
            hrrr: self.state.hrrr.as_ref(),
            hrrr_hour: self.state.hrrr_hour,
            model_field: self.state.model_field.as_ref().map(|(f, _)| f.as_ref()),
            model_field_age_min: self
                .state
                .model_field
                .as_ref()
                .map(|(_, at)| at.elapsed().as_secs() / 60),
            mrms,
            view_moment: self.state.moment,
            view_tilt_deg: self.state.tilt_deg,
            view_center: (self.state.view.center_lat, self.state.view.center_lon),
            view_zoom: self.state.view.zoom,
            view3d_open: self.state.view3d_open,
            show_melting_layer: self.state.show_melting_layer,
            look_points: look,
            srm_volume: srm,
            goes,
            futurecast: self.state.futurecast.as_ref(),
            scan_interval_min,
            now: chrono::Utc::now(),
        }))
    }

    /// Build the snapshot (optionally with the LOOK samples), compose the
    /// prompts and send the request. Callers own the pending flags.
    fn send_ai(&mut self, question: Option<String>, look: Vec<LookPointSample>) {
        let brief = match self.build_brief(&look) {
            Ok(brief) => brief,
            Err(message) => {
                self.state.ai_pending = false;
                self.ai_note(message);
                return;
            }
        };
        if !self.state.ai_config().is_configured() {
            self.state.ai_settings_open = true;
            self.state.ai_pending = false;
            let provider = if self
                .state
                .ai_base_url
                .to_ascii_lowercase()
                .contains("anthropic.com")
            {
                "Claude"
            } else {
                "OpenCode Go"
            };
            self.ai_note(format!(
                "Add your {provider} API key in Settings → AI meteorologist."
            ));
            return;
        }

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
            .unwrap_or_else(|| self.brief_ask());
        user.push_str(&format!(
            "\n\nThe Spotter's latest message: {asked_text}\n\
             Answer it from this snapshot. Write in plain English for a general audience: no \
             radar acronyms, no data dumps; say what the weather means and when it matters. \
             Above all, SHOW it: include a MAP directive that puts \
             the evidence on screen (focus the cell/couplet/hail core, switch product or tilt, \
             open the 3D pane or a cross-section, show the melting layer, mark what matters). \
             Keep it under 150 words unless the data demands more, keep the units, and still end \
             with the \"Watch for:\" line."
        ));

        let session = format!("stormscope-{}", chrono::Utc::now().timestamp_millis());
        if self
            .tx
            .send(DataCmd::AiBrief {
                kind: AiRequestKind::Chat,
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

    /// Turn Live AI Tracking on/off. Enabling for the first time opens the
    /// consent notice instead of starting silently.
    pub fn set_live_ai_enabled(&mut self, enabled: bool) {
        if enabled && !self.state.live_ai_consent {
            self.state.live_ai_consent_open = true;
            return;
        }
        self.state.live_ai_enabled = enabled;
        if enabled {
            self.state.ai_tab = AiTab::Live;
            self.state.ai_open = true;
            if self.live_ai.last_site.as_deref() != Some(self.state.site_id.as_str()) {
                self.live_ai.reset_for_site(&self.state.site_id);
            } else {
                self.live_ai.last_scan = None;
                self.live_ai.retry_at = None;
            }
            self.live_ai.mark_alerts(&self.state.alerts);
        } else {
            self.live_ai.stop();
        }
        self.state.persist_ai();
    }

    /// Accept the live-tracking notice, then start.
    pub fn confirm_live_ai(&mut self) {
        self.state.live_ai_consent = true;
        self.state.live_ai_consent_open = false;
        self.set_live_ai_enabled(true);
    }

    /// Start the next live scan when the data and provider are ready. Called
    /// every UI frame; the newest-scan check makes it idempotent.
    fn pump_live_ai(&mut self) {
        if !self.state.live_ai_enabled || self.state.mode != crate::state::Mode::Live {
            return;
        }
        let site_id = self.state.site_id.clone();
        if self.live_ai.last_site.as_deref() != Some(site_id.as_str()) {
            self.live_ai.reset_for_site(&site_id);
            self.live_ai.mark_alerts(&self.state.alerts);
        }
        if self.live_ai.in_flight.is_some() || self.state.ai_pending {
            return;
        }
        if !self.state.ai_config().is_configured() {
            return;
        }
        if self
            .live_ai
            .retry_at
            .is_some_and(|at| at > std::time::Instant::now())
        {
            return;
        }
        self.live_ai.retry_at = None;
        let Some(scan) = self.state.newest_start() else {
            return;
        };
        if self.live_ai.last_scan.is_some_and(|last| scan <= last) {
            return;
        }
        if self.state.ring.len() < 2 {
            return;
        }
        let Some(site) = radar_core::sites::site(&site_id) else {
            return;
        };
        let n = self.state.ring.len();
        let (prev_volume, volume) = {
            let prev = &self.state.ring[n - 2];
            let cur = &self.state.ring[n - 1];
            (Arc::clone(&prev.volume), Arc::clone(&cur.volume))
        };
        // A site switch clears the ring asynchronously. Do not analyze the
        // previous radar's final volume against the new site's coordinates.
        if (volume.meta.site_id != site_id && volume.meta.site_id != "UNKNOWN")
            || (prev_volume.meta.site_id != site_id && prev_volume.meta.site_id != "UNKNOWN")
        {
            return;
        }
        let raw = radar_render::compute_tracks(
            &prev_volume,
            &volume,
            crate::live_ai::TRACK_TILT_DEG,
            site.latitude,
            site.longitude,
        );
        let tracks = crate::live_ai::smooth_tracks(&self.live_ai.prev_tracks, raw);
        self.live_ai.prev_tracks = tracks.clone();

        let now = chrono::Utc::now();
        let primary = crate::live_ai::LiveLocation::resolve(&self.state, site, &volume);
        let assessment = crate::live_ai::assess(
            &volume,
            site,
            &tracks,
            &self.state.alerts,
            &primary,
            now,
            self.live_ai.overhead,
        );
        self.live_ai.overhead = assessment.surface_precip;
        let marker_etas = crate::live_ai::marker_etas(&tracks, &self.state.markers);
        // First scan after enabling (or a site switch): seed the alert set so
        // the existing warnings are context, not a burst of "new" ones.
        if self.live_ai.seen_alerts.is_empty() {
            self.live_ai.mark_alerts(&self.state.alerts);
        }
        let new_warnings = self.live_ai.new_warning_events(&self.state.alerts, now);
        let memory: Vec<crate::live_ai::LiveMemory> = self.live_ai.memory.iter().cloned().collect();
        let scan_interval_min = Some(
            (self.state.ring[n - 1].volume_start - self.state.ring[n - 2].volume_start)
                .num_minutes()
                .max(1),
        );
        // The live update sees exactly what a manual question sees: the
        // full snapshot (sounding, rotation, hail, VAD, QPE, model fields,
        // MRMS, GOES, HRRR/FutureCast, totals, reports) plus the live scan
        // series and deterministic primary assessment.
        let full = match self.build_brief_inner(&[], Some(&tracks), Some(&marker_etas)) {
            Ok(full) => full,
            Err(error) => {
                log::warn!("Live AI: could not build the full snapshot: {error}");
                self.live_ai.retry_at =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(30));
                return;
            }
        };
        let brief = match crate::brief::live_brief_from_full(
            &full,
            site,
            scan,
            &primary,
            &assessment,
            &memory,
            &new_warnings,
            self.live_ai.scan_number + 1,
            scan_interval_min,
            now,
        ) {
            Ok(brief) => brief,
            Err(error) => {
                log::warn!("Live AI: could not augment the full snapshot: {error}");
                self.live_ai.retry_at =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(30));
                return;
            }
        };
        let request = self
            .live_ai
            .begin(scan, site.id.to_string(), assessment, &self.state.alerts);
        log::info!(
            "Live AI: analyzing {} scan {} (primary {}, {} tracked cells)",
            site.id,
            scan.format("%H:%MZ"),
            primary.feed_label(),
            tracks.len()
        );
        let session = format!("stormscope-live-{}", chrono::Utc::now().timestamp_millis());
        if self
            .tx
            .send(DataCmd::AiBrief {
                kind: AiRequestKind::Live {
                    scan,
                    site: site.id.to_string(),
                    seq: request.seq,
                },
                config: self.state.ai_config(),
                system: brief.system,
                history: Vec::new(),
                user: brief.user,
                session,
            })
            .is_err()
        {
            self.live_ai.in_flight = None;
            self.live_ai.last_scan = None;
            log::warn!("Live AI: data thread is not running");
        }
    }

    fn finish_live_ai(
        &mut self,
        _scan: chrono::DateTime<chrono::Utc>,
        site: String,
        seq: u64,
        result: Result<String, String>,
    ) {
        let matches = self
            .live_ai
            .in_flight
            .as_ref()
            .is_some_and(|request| request.seq == seq);
        if !matches {
            return;
        }
        let request = self.live_ai.in_flight.take().expect("checked above");
        if !self.state.live_ai_enabled || site != self.state.site_id {
            return;
        }
        let assessment = request.assessment;
        let latency_ms = request.started.elapsed().as_millis() as u64;
        match result {
            Ok(reply) => {
                let (prose, _action, _look, _data) = crate::ai_actions::split_reply(&reply);
                let body = prose.trim().to_string();
                if body.is_empty() {
                    self.live_ai_error(
                        request.scan,
                        site,
                        assessment,
                        "the model returned an empty update".to_string(),
                        latency_ms,
                    );
                    return;
                }
                log::info!(
                    "Live AI: scan {} answered in {} ms ({} chars)",
                    request.scan.format("%H:%MZ"),
                    latency_ms,
                    body.len()
                );
                let entry = crate::live_ai::LiveEntry {
                    scan: request.scan,
                    site: site.clone(),
                    generated_at: chrono::Utc::now(),
                    location: assessment.location.feed_label(),
                    source: assessment.location.source,
                    eta_min: assessment.eta.as_ref().map(|eta| eta.minutes),
                    threat: assessment.threat,
                    headline: assessment.headline.clone(),
                    body: body.clone(),
                    error: false,
                    latency_ms,
                };
                self.live_ai.remember(request.scan, &assessment, &body);
                self.live_ai.seen_alerts = request.alerts_at_start;
                self.live_ai.last_assessment = Some(assessment.clone());
                self.live_ai.failures = 0;
                self.live_ai.retry_at = None;
                self.live_ai.push(entry);
                if self.state.live_ai_speak
                    && self.state.voice_alerts
                    && !self.state.voice_convo
                    && let Some(speech) = self.live_ai.speech_for(&assessment)
                {
                    #[cfg(target_os = "android")]
                    {
                        if self.state.gemini_api_key.trim().is_empty() {
                            crate::voice::announce(speech);
                        } else {
                            crate::gemini_live::speak_alert(
                                crate::gemini_live::LiveConfig {
                                    api_key: self.state.gemini_api_key.trim().to_string(),
                                    model: self.state.gemini_model.trim().to_string(),
                                    voice: self.state.gemini_voice.clone(),
                                    system: String::new(),
                                },
                                speech,
                            );
                        }
                    }
                    #[cfg(not(target_os = "android"))]
                    crate::voice::announce(speech);
                }
            }
            Err(error) => {
                log::warn!("Live AI scan failed: {error}");
                self.live_ai_error(request.scan, site, assessment, error, latency_ms);
            }
        }
    }

    fn live_ai_error(
        &mut self,
        scan: chrono::DateTime<chrono::Utc>,
        site: String,
        assessment: crate::live_ai::LiveAssessment,
        error: String,
        latency_ms: u64,
    ) {
        let entry = crate::live_ai::LiveEntry {
            scan,
            site,
            generated_at: chrono::Utc::now(),
            location: assessment.location.feed_label(),
            source: assessment.location.source,
            eta_min: assessment.eta.as_ref().map(|eta| eta.minutes),
            threat: assessment.threat,
            headline: "Live analysis failed".to_string(),
            body: format!("{error} — the next scan will try again."),
            error: true,
            latency_ms,
        };
        self.live_ai.push(entry);
        self.live_ai.last_assessment = Some(assessment);
        self.live_ai.failures = self.live_ai.failures.saturating_add(1);
        let delay = match self.live_ai.failures {
            0 | 1 => 120,
            2 => 300,
            _ => 900,
        };
        self.live_ai.retry_at =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(delay));
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

    fn pump_qpe(&mut self) {
        if !self.state.show_qpe {
            return;
        }
        let want = radar_core::sites::site(&self.state.site_id)
            .map(|s| radar_data::local_overlay_box(s.latitude, s.longitude));
        let fresh = self.state.qpe.as_ref().is_some_and(|(m, at)| {
            at.elapsed() < std::time::Duration::from_secs(600)
                && m.last().is_some_and(|near| {
                    near.product == self.state.qpe_product
                        && want.is_none_or(|w| {
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
        if self
            .state
            .qpe
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() < std::time::Duration::from_secs(30))
        {
            return;
        }
        if self
            .state
            .qpe_requested_at
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(120))
        {
            return;
        }
        self.state.qpe_requested_at = Some(std::time::Instant::now());
        let _ = self.tx.send(DataCmd::LoadQpe(self.state.qpe_product));
    }

    fn pump_model_field(&mut self) {
        if !self.state.show_model_field {
            return;
        }
        let want = radar_core::sites::site(&self.state.site_id)
            .map(|s| radar_data::local_overlay_box(s.latitude, s.longitude));
        let fresh = self.state.model_field.as_ref().is_some_and(|(f, at)| {
            at.elapsed() < std::time::Duration::from_secs(600)
                && f.model == self.state.model_field_model
                && f.kind == self.state.model_field_kind
                && f.hour == self.state.model_field_hour
                && want.is_none_or(|w| {
                    (f.south - w[0]).abs() < 0.2
                        && (f.west - w[1]).abs() < 0.2
                        && (f.north - w[2]).abs() < 0.2
                        && (f.east - w[3]).abs() < 0.2
                })
        });
        if fresh {
            return;
        }
        if self
            .state
            .model_field_requested_at
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(90))
        {
            return;
        }
        self.state.model_field_requested_at = Some(std::time::Instant::now());
        let _ = self.tx.send(DataCmd::LoadModelField {
            model: self.state.model_field_model,
            kind: self.state.model_field_kind,
            hour: self.state.model_field_hour,
        });
    }

    /// Keep the GOES satellite overlay fresh while it is on: fetch on first
    /// enable, on a layer/satellite change, and every ten minutes after.
    /// Fetch one GLM file per minute while the Lightning layer is enabled.
    fn pump_lightning(&mut self) {
        use std::time::Duration;
        if !self.state.show_lightning {
            return;
        }
        if self.state.lightning_pending {
            if self
                .state
                .lightning_requested_at
                .is_some_and(|t| t.elapsed() > Duration::from_secs(30))
            {
                self.state.lightning_pending = false;
            } else {
                return;
            }
        }
        if self
            .state
            .lightning_requested_at
            .is_some_and(|t| t.elapsed() < Duration::from_secs(60))
        {
            return;
        }
        let Some(site) = radar_core::sites::site(&self.state.site_id) else {
            return;
        };
        let west = site.longitude < -105.0;
        if self.tx.send(DataCmd::LoadGlm { west }).is_ok() {
            self.state.lightning_pending = true;
            self.state.lightning_requested_at = Some(std::time::Instant::now());
        }
    }

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

    /// Switch FutureCast between the latest 18-hour cycle and the older
    /// 48-hour extended cycle. Clearing the loaded run makes the switch
    /// explicit; the next UI frame asks the data thread for the new horizon.
    pub fn toggle_futurecast_horizon(&mut self) {
        self.state.futurecast_extended = !self.state.futurecast_extended;
        self.state.futurecast = None;
        self.state.futurecast_pending = false;
        self.state.futurecast_requested_at = None;
        self.state.futurecast_playing = true;
        self.state.futurecast_last_step = 0.0;
    }

    /// Advance the FutureCast animation. This lives on the app, not in the
    /// timeline widget: fullscreen hides the bottom panel, and playback must
    /// keep running there.
    fn advance_futurecast_playback(&mut self, ctx: &egui::Context) {
        if !self.state.show_futurecast || !self.state.futurecast_playing {
            return;
        }
        let now = ctx.input(|i| i.time);
        if self.state.futurecast_last_step <= 0.0 {
            self.state.futurecast_last_step = now;
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        if now - self.state.futurecast_last_step < 0.35 {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        if let Some(fc) = self.state.futurecast.as_mut() {
            let start = fc.selected;
            let mut next = None;
            for step in 1..=fc.plan.len() {
                let i = (start + step) % fc.plan.len();
                if fc.frames[i].is_some() {
                    next = Some(i);
                    break;
                }
            }
            if let Some(i) = next
                && i != start
            {
                fc.selected = i;
            }
        }
        self.state.futurecast_last_step = now;
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }

    /// Keep the HRRR FutureCast layer fresh while it is on. HRRR cycles
    /// hourly; a 30-minute refresh catches a new run without polling NOAA
    /// per frame. The 120 s retry floor keeps a failure from becoming a loop.
    fn pump_futurecast(&mut self) {
        use std::time::Duration;
        if !self.state.show_futurecast {
            return;
        }
        let Some(site) = radar_core::sites::site(&self.state.site_id) else {
            return;
        };
        if !radar_data::futurecast::supported(site.latitude, site.longitude) {
            return;
        }
        let bbox = radar_data::futurecast::forecast_box(site.latitude, site.longitude);
        if self.state.futurecast_pending {
            let stale = self
                .state
                .futurecast_requested_at
                .is_some_and(|at| at.elapsed() > Duration::from_secs(180));
            if !stale {
                return;
            }
            log::warn!("FutureCast fetch stalled; re-requesting");
            self.state.futurecast_pending = false;
        }
        let fresh = self.state.futurecast.as_ref().is_some_and(|fc| {
            fc.bbox == bbox
                && fc.extended == self.state.futurecast_extended
                && fc.loaded() > 0
                && fc.received_at.elapsed() < Duration::from_secs(30 * 60)
        });
        if fresh {
            return;
        }
        let retry = if self.state.futurecast.is_some() {
            Duration::from_secs(120)
        } else {
            Duration::from_secs(15)
        };
        if self
            .state
            .futurecast_requested_at
            .is_some_and(|at| at.elapsed() < retry)
        {
            return;
        }
        self.state.futurecast_requested_at = Some(std::time::Instant::now());
        self.state.futurecast_pending = true;
        self.state.futurecast_site = self.state.site_id.clone();
        let _ = self.tx.send(DataCmd::LoadFuturecast {
            bbox,
            extended: self.state.futurecast_extended,
        });
    }

    /// Ask the data thread for the storm-relative volume the current frame +
    /// motion setting needs. One derivation is in flight at a time: while it
    /// builds (and on playback, where every frame wants its own), the panes
    /// keep showing the last derived volume instead of blanking.
    fn pump_srm(&mut self) {
        let needs_srm = self.state.moment == radar_core::Moment::StormRelativeVelocity
            || (self.state.view3d_open
                && self.state.pane_max != crate::state::PaneMax::Map
                && self.state.view3d_moment == radar_core::Moment::StormRelativeVelocity);
        if !needs_srm {
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
        self.auto_restore_ai_view(ui.ctx());
        if !self.state.view3d_open && self.view3d.is_some() {
            // Closing the pane must not leave a renderer/mesh worker alive.
            self.view3d = None;
            self.view3d_section_key = None;
            log::debug!("3D pane off: released its GPU resources and mesh worker");
        }
        let mut voice_points: Vec<(f64, f64)> = self
            .state
            .markers
            .iter()
            .map(|marker| (marker.lat, marker.lon))
            .collect();
        if let Some(fix) = self.state.gps_fix {
            voice_points.push(fix);
        }
        #[cfg(target_os = "android")]
        {
            // Storm Watch gates background radar on the GPS fix and every
            // map marker. The data thread refreshes the GPS point from Java
            // while backgrounded; markers are static until the app resumes.
            let mut markers: Vec<(f64, f64)> = self
                .state
                .markers
                .iter()
                .map(|marker| (marker.lat, marker.lon))
                .collect();
            let mut seen = std::collections::HashSet::new();
            markers.retain(|point| seen.insert((point.0.to_bits(), point.1.to_bits())));
            markers.truncate(11);
            if let Ok(mut shared) = self.data.lock() {
                shared.gps_fix = self.state.gps_fix;
                shared.watch_markers = markers;
                shared.storm_watch_enabled = self.state.storm_watch_enabled;
                shared.live_mode = self.state.mode == crate::state::Mode::Live;
            }
        }
        crate::voice::set_filter(crate::voice::VoiceFilter {
            enabled: self.state.voice_alerts,
            tornado: self.state.voice_tornado,
            severe_thunderstorm: self.state.voice_severe_thunderstorm,
            watches: self.state.voice_watches,
            other_warnings: self.state.voice_other_warnings,
            points: voice_points,
        });
        self.pump();
        self.pump_srm();
        self.pump_mrms();
        self.pump_qpe();
        self.pump_model_field();
        self.pump_futurecast();
        self.advance_futurecast_playback(ui.ctx());
        self.pump_goes();
        self.pump_lightning();
        self.pump_ai_site();
        self.pump_ai_data();
        self.pump_live_ai();
        // The foreground service writes the Storm Watch toggle when its
        // notification Stop action is used; keep the Settings checkbox in
        // sync without polling Java every frame.
        #[cfg(target_os = "android")]
        {
            let now = ui.ctx().input(|input| input.time);
            if now - self.state.storm_watch_polled_at > 1.0 {
                self.state.storm_watch_polled_at = now;
                self.state.storm_watch_enabled = crate::android_storm_watch::enabled();
            }
            // A tapped alert notification: open that alert's NOAA detail
            // (full-pane on a phone) with the polygon focused behind it.
            if let Some(id) = crate::android_storm_watch::take_alert_id() {
                self.state.alert_detail = Some(id.clone());
                self.state.alerts_popup_open = true;
                self.state.alerts_focus = Some(id);
                self.state.alerts_focus_at = ui.ctx().input(|i| i.time);
            }
            // The persistent notification's content intent opens the NWS
            // forecast for the spotter's GPS point (or the current pin).
            if crate::android_storm_watch::take_brief_request() {
                self.state.ai_open = false;
                self.state.ai_compact = false;
                self.state.forecast_pick_mode = false;
                if (self.state.forecast_point.is_none() || self.state.forecast_point_gps)
                    && let Some((lat, lon)) = self.state.gps_fix
                {
                    self.state.forecast_point = Some((lat, lon));
                    self.state.forecast_point_gps = true;
                }
                self.state.forecast_open = true;
            }
            // Returning from the background: the data thread stopped the live
            // poller on pause, so restart it on every resume. RefreshLive keeps
            // the frames already in memory and only asks for the newest scan;
            // it does not clear or replay the cached timeline.
            if crate::android_lifecycle::take_resume_refresh()
                && self.state.mode == crate::state::Mode::Live
            {
                let _ = self
                    .tx
                    .send(DataCmd::RefreshLive(self.state.site_id.clone()));
            }
            // Background update check: once a day, ask GitHub for a newer
            // release and surface it as a centred modal when one exists.
            if !self.state.update_check_pending
                && self.state.update_checked_at.is_none_or(|last| {
                    last.elapsed() >= std::time::Duration::from_secs(24 * 60 * 60)
                })
            {
                self.state.update_check_pending = true;
                let _ = self.tx.send(DataCmd::CheckUpdate {
                    current: env!("CARGO_PKG_VERSION").to_string(),
                    asset_suffix: "-android.apk".into(),
                });
            }
            self.pump_gemini_live();
            // Sessions are event-driven, but the footer meter, status and
            // transcript need a frame even when the user is not touching the
            // screen. Without this the UI can sit on "Connecting…" while the
            // audio session is already live.
            if self.state.voice_convo {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        // Synoptic: auto-fetch on startup and every 20 minutes. Upstream
        // station data changes slowly (ASOS hourly, mesonets 5-15 min) and
        // the free tier is ~5,000 requests/month, so polling faster than
        // this buys nothing. Pan/zoom does not force a refetch.
        if self.state.show_synoptic
            && !self.state.synoptic_pending
            && !self.state.synoptic_token.trim().is_empty()
        {
            let due = self
                .state
                .synoptic_last_fetch
                .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(20 * 60));
            let throttled = self
                .state
                .synoptic_last_attempt
                .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(60));
            if due && throttled {
                self.fetch_synoptic_now();
            }
        }
        // METAR: free, no token. Airports report hourly (special reports as
        // needed); 10 minutes keeps the layer current without hammering it.
        if self.state.show_metar && !self.state.metar_pending {
            let due = self
                .state
                .metar_last_fetch
                .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(10 * 60));
            let throttled = self
                .state
                .metar_last_attempt
                .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(60));
            if due && throttled {
                self.fetch_metar_now();
            }
        }
        // Official Level III N0Q: refresh every 5 minutes, or immediately
        // after a radar-site change.
        if self.state.show_level3 && !self.state.level3_pending {
            let due = self
                .state
                .level3
                .as_ref()
                .is_none_or(|(_, at)| at.elapsed() >= std::time::Duration::from_secs(5 * 60))
                || self.state.level3_site != self.state.site_id;
            if due {
                self.fetch_level3_now();
            }
        }
        // Ambient: auto-fetch on startup and every 5 minutes.
        if !self.state.ambient_pending
            && !self.state.ambient_api_key.trim().is_empty()
            && !self.state.ambient_app_key.trim().is_empty()
        {
            let due = self
                .state
                .ambient_last_fetch
                .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(300));
            let throttled = self
                .state
                .ambient_last_attempt
                .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(60));
            if due && throttled {
                self.fetch_ambient_now();
            }
        }
        #[cfg(windows)]
        if !self.state.update_check_pending
            && self
                .state
                .update_checked_at
                .is_none_or(|last| last.elapsed() >= std::time::Duration::from_secs(24 * 60 * 60))
        {
            self.state.update_check_pending = true;
            let _ = self.tx.send(DataCmd::CheckUpdate {
                current: env!("CARGO_PKG_VERSION").to_string(),
                asset_suffix: "-windows-x86_64-setup.exe".into(),
            });
        }
        #[cfg(target_os = "linux")]
        if std::env::var_os("APPIMAGE").is_some()
            && !self.state.update_check_pending
            && self
                .state
                .update_checked_at
                .is_none_or(|last| last.elapsed() >= std::time::Duration::from_secs(24 * 60 * 60))
        {
            self.state.update_check_pending = true;
            let _ = self.tx.send(DataCmd::CheckUpdate {
                current: env!("CARGO_PKG_VERSION").to_string(),
                asset_suffix: "-linux-x86_64.AppImage".into(),
            });
        }
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
                rss_mb()
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

/// Resident set size in MB, on platforms that expose it cheaply (Linux).
/// Windows/macOS report `None`, which the debug log prints as "?".
fn rss_mb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        return std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
            .map(|pages| pages * 4096 / 1_000_000);
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Minimal system font lookup (Linux fontconfig paths, Windows font folder).
fn find_system_font(name: &str) -> Option<String> {
    let dirs: &[&str] = if cfg!(windows) {
        &[r"C:\Windows\Fonts"]
    } else {
        &["/usr/share/fonts", "/usr/local/share/fonts"]
    };
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
        #[cfg(not(target_os = "android"))]
        if let Some(embed) = &mut self.video_embed {
            if embed.take_close_requested() {
                self.state.youtube_player = None;
                self.youtube_video_rect = None;
            }
            if let Err(error) = embed.sync(
                self.state.youtube_player.as_ref(),
                self.youtube_video_rect,
                ui.ctx().pixels_per_point(),
            ) {
                log::warn!("embedded video: {error}");
                self.state.youtube_player_error = Some(error);
                self.state.youtube_player = None;
                self.youtube_video_rect = None;
            }
        }
        self.record_frame_stats(t0.elapsed(), ui.ctx());
    }
}
