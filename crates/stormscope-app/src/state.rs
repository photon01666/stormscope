//! Application state: which site/product/tilt are selected, the animation
//! ring buffer of decoded volumes, live-vs-archive mode, and the current
//! map view + playhead.

use chrono::{DateTime, Utc};
use radar_core::{Moment, Volume};
use radar_render::MapView;
use std::collections::VecDeque;
use std::sync::Arc;

/// How many decoded volumes are kept resident for looping. 24 scans ≈ 2 hours
/// of wall-clock radar time — but the *memory budget* below is the binding
/// limit in practice (a decoded volume runs 150-200 MB), so the ring doesn't
/// pin ~5 GB of RAM on a machine that has other work to do.
///
/// Android gets a much smaller ring: phone memory is shared with the OS and
/// the per-app native budget is nowhere near a desktop's.
#[cfg(not(target_os = "android"))]
pub const RING_CAPACITY: usize = 24;
#[cfg(target_os = "android")]
pub const RING_CAPACITY: usize = 8;

/// Resident-memory budget for the animation ring. Evicting oldest-first down
/// to this keeps the app's footprint predictable: leaving it open all night
/// must not push the system into swap (and, on a shared-memory GPU, into
/// driver buffer-eviction stalls that stutter the whole desktop).
#[cfg(not(target_os = "android"))]
pub const RING_BUDGET_BYTES: usize = 3 * 1024 * 1024 * 1024;
#[cfg(target_os = "android")]
pub const RING_BUDGET_BYTES: usize = 512 * 1024 * 1024;

/// Never trim below this many frames — a loop needs a few scans to be a loop.
#[cfg(not(target_os = "android"))]
pub const MIN_RING_FRAMES: usize = 8;
#[cfg(target_os = "android")]
pub const MIN_RING_FRAMES: usize = 3;

/// Approximate resident size of one decoded volume: gate data is the bulk
/// (i16 per gate per moment), plus azimuth tables and struct overhead.
fn volume_bytes(v: &radar_core::Volume) -> usize {
    let mut n = std::mem::size_of::<radar_core::Volume>() + v.sweeps.len() * 64;
    for s in &v.sweeps {
        n += std::mem::size_of::<radar_core::Sweep>();
        n += s.azimuths_deg.len() * std::mem::size_of::<f32>();
        for m in &s.moments {
            n += std::mem::size_of::<radar_core::MomentData>() + m.data.len() * 2;
        }
    }
    n
}

/// Products the selector offers for the current volume: whatever the volume
/// carries (decoded base moments plus the derived products attached at
/// decode), plus storm-relative velocity whenever velocity is present — SRM
/// is derived on demand from the motion sliders.
pub fn available_moments(state: &AppState) -> Vec<Moment> {
    let mut moments: Vec<Moment> = state
        .current()
        .map(|f| f.volume.meta.moments.clone())
        .unwrap_or_default();
    if moments.contains(&Moment::Velocity) && !moments.contains(&Moment::StormRelativeVelocity) {
        moments.push(Moment::StormRelativeVelocity);
    }
    if moments.is_empty() {
        moments.push(Moment::Reflectivity);
    }
    moments
}

/// Identity of one storm-relative derivation: the frame's scan time plus the
/// motion setting. Any change means a new SRM volume.
pub fn srm_key_of(volume: &Volume, toward_deg: f32, speed_kt: f32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    volume.meta.volume_start.timestamp_millis().hash(&mut h);
    toward_deg.to_bits().hash(&mut h);
    speed_kt.to_bits().hash(&mut h);
    h.finish()
}

/// A user-placed, labeled location pin on the map. Persisted to
/// `~/.config/stormscope/markers.json`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MapMarker {
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub label: String,
}

impl MapMarker {
    /// Display name: the label, or a coordinate-based fallback.
    pub fn display(&self) -> String {
        if self.label.trim().is_empty() {
            format!("{:.3}, {:.3}", self.lat, self.lon)
        } else {
            self.label.trim().to_string()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Follow the newest volume from the live poller.
    Live,
    /// Scrub through archive volumes for a chosen date.
    Archive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopState {
    Playing,
    Paused,
}

/// Which pill in the top-center badge row is under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeKind {
    Mode,
    Age,
    Alerts,
    Forecast,
    /// The "FutureCast" pill: HRRR simulated reflectivity. While it is
    /// active the same pill says "Live Radar" and turns the layer off.
    Futurecast,
}

/// Loaded HRRR FutureCast run: its plan, the frames received so far, and the
/// frame the user is looking at. Frames arrive progressively from the data
/// thread; a missing slot is still downloading.
pub struct FuturecastState {
    pub init: DateTime<Utc>,
    pub bbox: [f64; 4],
    pub plan: Vec<radar_data::futurecast::FrameInfo>,
    pub frames: Vec<Option<Arc<radar_data::futurecast::Frame>>>,
    pub selected: usize,
    /// True for the 48-hour extended cycle, false for the latest 18-hour run.
    pub extended: bool,
    /// When the run finished downloading; the overlay refreshes after 30 min.
    pub received_at: std::time::Instant,
}

impl FuturecastState {
    pub fn new(
        init: DateTime<Utc>,
        bbox: [f64; 4],
        plan: Vec<radar_data::futurecast::FrameInfo>,
        extended: bool,
    ) -> Self {
        let frames = vec![None; plan.len()];
        Self {
            init,
            bbox,
            plan,
            frames,
            selected: 0,
            extended,
            received_at: std::time::Instant::now(),
        }
    }

    pub fn selected_info(&self) -> Option<&radar_data::futurecast::FrameInfo> {
        self.plan.get(self.selected)
    }

    pub fn selected_frame(&self) -> Option<&Arc<radar_data::futurecast::Frame>> {
        self.frames.get(self.selected).and_then(Option::as_ref)
    }

    /// Store a frame in its plan slot. Returns whether it was useful.
    pub fn insert(&mut self, frame: Arc<radar_data::futurecast::Frame>) -> bool {
        let Some(index) = self.plan.iter().position(|info| info.hour == frame.hour) else {
            return false;
        };
        if let Some(slot) = self.frames.get_mut(index) {
            *slot = Some(frame);
            true
        } else {
            false
        }
    }

    pub fn loaded(&self) -> usize {
        self.frames.iter().filter(|f| f.is_some()).count()
    }

    /// First/last slot with a decoded frame, for the jump buttons and the
    /// auto-loop (missing frames just extend the wait).
    pub fn first_loaded(&self) -> Option<usize> {
        self.frames.iter().position(Option::is_some)
    }

    pub fn last_loaded(&self) -> Option<usize> {
        self.frames.iter().rposition(Option::is_some)
    }
}

/// An in-flight bulk radar load (a live prime refill or an archive window):
/// what is downloading, and how far along. The map shows it as a prominent
/// pill so a multi-second download never reads as the app being stuck.
#[derive(Clone, Debug)]
pub struct LoadProgress {
    pub label: String,
    pub done: u32,
    pub total: u32,
    /// True when the current file is coming from the on-disk cache; false
    /// only when it is actually being downloaded from S3.
    pub cached: bool,
    pub at: std::time::Instant,
}

/// Which set the alerts browser is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertsTab {
    /// Every active alert nationwide.
    All,
    /// Only alerts whose polygon lies near the selected radar site.
    Station,
    /// Active alerts with no polygon (zone/county based). Text and timing
    /// only — nothing to draw or center the map on — so they live on their
    /// own tab instead of crowding the polygon alerts.
    Zones,
    /// Historical warnings for the archive day (IEM VTEC archive).
    Archive,
    /// The same archive day, restricted to the selected station's area
    /// (polygon-based warnings only).
    ArchiveStation,
}

/// Union of the sweep set and the point set, deduped by alert id (the sweep
/// entry stays when both have it, so ordering by severity is preserved).
pub fn merge_alerts(
    slow: &[radar_core::warnings::Warning],
    fast: &[radar_core::warnings::Warning],
) -> Vec<radar_core::warnings::Warning> {
    let mut out: Vec<radar_core::warnings::Warning> = slow.to_vec();
    for w in fast {
        if !out.iter().any(|s| s.id == w.id) {
            out.push(w.clone());
        }
    }
    out
}

/// Radius (km) within which an alert counts as "at" the selected station:
/// the same radius the near-site fetch uses, i.e. the radar's practical
/// warning area.
pub const STATION_ALERT_RADIUS_KM: f64 = 400.0;

/// One end of the measure-tool line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasureEnd {
    A,
    B,
}

/// Whatever a measure-mode drag is currently moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasureDrag {
    End(MeasureEnd),
    Label,
}

/// Which pane owns the central area while the 3D volume pane is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PaneMax {
    /// Side-by-side split: 2D map and 3D volume with a draggable divider.
    #[default]
    None,
    /// The 2D map fills the central area.
    Map,
    /// The 3D volume fills the central area.
    View3d,
}

/// One line of the meteorologist conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AiRole {
    User,
    Assistant,
    /// An error or status note (never sent to the model).
    Note,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AiChatMessage {
    pub role: AiRole,
    pub text: String,
}

/// What an AI map mark means; drives its colour and where it reads best.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiAnnotationKind {
    Tornado,
    Hail,
    Wind,
    Flood,
    /// An updraft / storm-top feature that only makes sense in 3D.
    Updraft,
    Generic,
}

impl AiAnnotationKind {
    pub fn rgb(self) -> [u8; 3] {
        match self {
            AiAnnotationKind::Tornado => [255, 90, 180],
            AiAnnotationKind::Hail => [255, 175, 60],
            AiAnnotationKind::Wind => [255, 225, 90],
            AiAnnotationKind::Flood => [80, 220, 150],
            AiAnnotationKind::Updraft => [120, 200, 255],
            AiAnnotationKind::Generic => [120, 200, 255],
        }
    }
}

/// A mark the AI put on the maps: a labelled circle on the 2D map and a ring
/// (plus an altitude pole in 3D) in the volume pane.
#[derive(Debug, Clone)]
pub struct AiAnnotation {
    pub lat: f64,
    pub lon: f64,
    /// Circle radius on the ground, miles.
    pub radius_mi: f64,
    /// Height of the feature (kft AGL); 0 = surface. 3D-only features use this.
    pub alt_kft: f32,
    pub label: String,
    pub kind: AiAnnotationKind,
}

/// The view the AI is allowed to change, captured before a `MAP:` manoeuvre
/// so the report window can put everything back.
#[derive(Debug, Clone)]
pub struct AiViewSaved {
    pub center_lat: f64,
    pub center_lon: f64,
    pub zoom: f64,
    pub moment: Moment,
    pub tilt_deg: f32,
    pub view3d_open: bool,
    pub pane_max: PaneMax,
    pub view3d_yaw: f32,
    pub view3d_pitch: f32,
    pub view3d_dist_km: f32,
    pub view3d_ve: f32,
    pub view3d_section: bool,
    pub view3d_section_az: f32,
    pub show_melting_layer: bool,
}

/// One Central day of cached scans for the selected site.
#[derive(Debug, Clone)]
pub struct CachedDay {
    pub date: chrono::NaiveDate,
    pub scans: usize,
    pub bytes: u64,
    pub first: chrono::DateTime<chrono::Utc>,
    pub last: chrono::DateTime<chrono::Utc>,
}

/// One radar volume present in the on-disk cache for the selected site.
#[derive(Debug, Clone, Copy)]
pub struct CachedScan {
    pub start: chrono::DateTime<chrono::Utc>,
    pub bytes: u64,
}

/// One frame in the animation ring.
pub struct Frame {
    pub volume_start: DateTime<Utc>,
    pub volume: Arc<Volume>,
    /// Approximate resident bytes of `volume` (ring budget accounting).
    pub bytes: usize,
}

pub struct AppState {
    pub mode: Mode,
    /// Selected archive day (Central-date; edited in the Archive popup).
    pub archive_date: chrono::NaiveDate,
    /// The Archive popup (opened from Settings), and the time of day
    /// (Central, minutes since midnight) the user wants to pull from.
    pub archive_popup_open: bool,
    /// The product explainer window (the (i) beside the Product selector).
    pub product_help_open: bool,
    pub archive_minute: u32,
    /// Warning-archive backfill progress (from the background crawler).
    pub archive_crawl: Option<radar_data::warnings_archive::CrawlStatus>,
    /// Historical warnings loaded for the archive tab (Central day), and the
    /// same list presented as live-alert-shaped values (index aligned).
    pub archive_warnings: Vec<radar_data::warnings_archive::HistoricWarning>,
    pub archive_warning_view: Vec<radar_core::warnings::Warning>,
    /// Which Central day `archive_warnings` holds.
    pub archive_warnings_date: Option<chrono::NaiveDate>,
    /// In-flight request (day + when sent): dedupes tab clicks and bounds
    /// retries, exactly like the totals window does.
    pub archive_warnings_requested: Option<(chrono::NaiveDate, std::time::Instant)>,
    /// Historical warning whose NWS text popup is open (PROD_ID), the
    /// fetched texts (None = unavailable), and the in-flight requests.
    pub archive_detail: Option<String>,
    pub archive_texts: std::collections::HashMap<String, Option<String>>,
    pub archive_text_requested: std::collections::HashSet<String>,
    /// The in-flight bulk radar load, if any (see [`LoadProgress`]).
    pub load_progress: Option<LoadProgress>,
    /// Last site whose volumes were dropped as "not the selected station"
    /// (logs once per distinct site so a desync is visible).
    pub site_mismatch_logged: Option<String>,
    /// Pending "park the playhead at this moment once the radar lands"
    /// (armed by clicking a historical warning; consumed in the pump).
    pub archive_jump: Option<(chrono::DateTime<chrono::Utc>, std::time::Instant)>,
    /// Historical warning under the map pointer, and when its hover began
    /// (the overlay's tooltip dwell).
    pub archive_hover: Option<String>,
    pub archive_hover_at: f64,
    /// Historical warning just selected in the browser: keep its polygon on
    /// screen for a short while even if the loaded scan is slightly outside
    /// its in-effect window (the playhead seek can lag the archive load).
    pub archive_focus: Option<(String, std::time::Instant)>,
    /// The per-pane control windows (opened from each pane's bottom-right
    /// "Controls" chip). The sidebar keeps everything else.
    pub map_controls_open: bool,
    pub view3d_controls_open: bool,
    pub loop_state: LoopState,
    pub fps: f32,

    // Selection
    pub site_id: String,
    pub moment: Moment,
    pub tilt_deg: f32,
    pub opacity: f32,
    /// Minimum dBZ filter (the "Min dBZ" slider): suppresses returns below
    /// this strength — the fix for clear-air clutter reading as a green blob.
    /// Only applied to Reflectivity-family moments.
    pub min_dbz: f32,

    // Map
    pub view: MapView,

    // Animation ring: index 0 = oldest retained, last = newest.
    pub ring: VecDeque<Frame>,
    /// Playhead into `ring` (0..ring.len()). Clamped on push.
    pub playhead: usize,
    /// Resident bytes across `ring` (sum of `Frame::bytes`).
    pub ring_bytes: usize,
    /// Eviction budget for `ring_bytes` (a field, not just the const, so
    /// tests can shrink it).
    pub ring_budget_bytes: usize,

    // Alerts
    /// Every alert the app draws and lists: the 400 km sweep merged with the
    /// fast-lane point set below (`rebuild_alerts`).
    pub alerts: Vec<radar_core::warnings::Warning>,
    /// The slower 400 km sweep around the station (newest fetch).
    pub alerts_slow: Vec<radar_core::warnings::Warning>,
    /// Alerts the API says apply *exactly to the station*, polled every
    /// minute (a few hundred bytes per fetch). New warnings show up here
    /// first, so the badge flashes and the polygons appear within a minute of
    /// issuance, while the wider sweep catches up on its own cadence.
    pub alerts_fast: Vec<radar_core::warnings::Warning>,
    /// Every active alert nationwide, for the alerts browser.
    pub all_alerts: Vec<radar_core::warnings::Warning>,
    /// Master switch for spoken alerts (desktop builds only). Persisted in
    /// `voice-alerts.json`; the per-family switches narrow it.
    pub voice_alerts: bool,
    pub voice_tornado: bool,
    pub voice_severe_thunderstorm: bool,
    pub voice_watches: bool,
    pub voice_other_warnings: bool,
    /// Piper voice id selected in Settings (`None` = best installed voice,
    /// currently the female lessac high model). Android uses system TTS.
    pub voice_model: Option<String>,
    /// Android opt-in background Storm Watch service. The Android
    /// SharedPreferences value is the source of truth; this is synced from
    /// Java for the Settings checkbox.
    pub storm_watch_enabled: bool,
    pub storm_watch_polled_at: f64,
    pub show_alerts: bool,
    /// Alerts browser visibility and which tab is showing.
    pub alerts_popup_open: bool,
    pub alerts_tab: AlertsTab,
    /// Search box contents for the alerts browser.
    pub alerts_filter: String,
    /// Alert id under the pointer in the alerts browser: the map glows that
    /// polygon so a list row can be matched to the warning on screen.
    pub alerts_hover: Option<String>,
    /// How opaque warning polygon fills are drawn: 1.0 is the shipped look
    /// (a light wash that lets the radar through), 0.0 leaves just the
    /// outlines. The Settings menu carries the slider.
    pub alerts_fill: f32,
    /// Warnings under the pointer *on the map*, most clickable first: the
    /// polygon whose outline is nearest the pointer, then the containing
    /// polygons smallest first (see `ui::alerts::warning_stack_at`). Every one
    /// of them glows and is named — the first at full strength, because that
    /// is what a click opens — so pointing at a warning shows what it is
    /// without opening anything.
    pub alerts_map_stack: Vec<String>,
    /// SPC risk category (`DN`) under the map pointer, for the outline
    /// emphasis and the "what does this level mean" tooltip.
    pub outlook_hover: Option<u16>,
    /// egui time the current outlook hover began (tooltip dwell).
    pub outlook_hover_at: f64,
    /// egui time when the current map hover began, for the tooltip's
    /// dwell delay (a passing pointer should not pop text).
    pub alerts_map_hover_at: f64,
    /// Alert id the browser last jumped to, and when (egui time, seconds):
    /// the map flashes it, then settles into a steady highlight.
    pub alerts_focus: Option<String>,
    pub alerts_focus_at: f64,
    /// Alert id whose NOAA detail popup is open (set by the browser's
    /// Details button or by clicking a warning polygon on the map).
    pub alert_detail: Option<String>,

    // Storm reports (Preliminary Local Storm Reports) and Mesoscale
    // Discussions: both polled every five minutes by the coordinator.
    /// Newest-first list of reports (12 h window, capped) with coordinates.
    pub lsrs: Vec<radar_data::lsrs::Lsr>,
    pub show_lsrs: bool,
    /// Report id under the pointer / whose detail popup is open.
    pub lsr_hover: Option<String>,
    pub lsr_hover_at: f64,
    pub lsr_detail: Option<String>,
    /// Active SPC mesoscale discussions by number.
    pub mcds: Vec<radar_data::mcds::Mcd>,
    pub show_mcds: bool,
    /// Discussion number under the pointer / whose detail popup is open.
    pub mcd_hover: Option<u32>,
    pub mcd_hover_at: f64,
    pub mcd_detail: Option<u32>,

    // Device location (Android). The first fix selects the nearest radar
    // and points the forecast at the user; the map chip can ask for a fresh
    // one at any time.
    pub gps_fix: Option<(f64, f64)>,
    /// egui-clock time of the last JNI poll (throttles to once every couple
    /// of seconds while a fix is outstanding).
    pub gps_polled_at: f64,
    /// The first fix has been applied (polling can stop).
    pub gps_applied: bool,
    /// A locate request is in flight (drives the chip's busy state).
    pub gps_waiting: bool,
    /// Soft-keyboard height in points (Android IME inset), polled while the
    /// AI windows are open so the chat box can stay above the keyboard.
    pub ime_inset_pt: f32,
    pub ime_polled_at: f64,

    // NWS forecast (station by default, or a point picked on the map).
    pub forecast: Option<radar_core::forecast::Forecast>,
    /// Current conditions from the nearest reporting station (same point as
    /// the forecast above).
    pub observation: Option<radar_core::forecast::Observation>,
    pub forecast_open: bool,
    /// Armed by the window's "select on map" control: the next map click
    /// becomes the forecast point.
    pub forecast_pick_mode: bool,
    /// The window was minimized to let a map pick happen; reopen it as soon
    /// as the point lands.
    pub forecast_reopen_after_pick: bool,
    /// Manually picked forecast point; `None` = follow the selected station.
    pub forecast_point: Option<(f64, f64)>,
    /// The current point came from the device's GPS fix (not a manual pin),
    /// so a fresh fix may move it.
    pub forecast_point_gps: bool,
    /// Station id the request/result corresponds to, so the window can tell
    /// when a site switch needs a refetch.
    pub forecast_site: String,
    /// Coordinates already requested (dedupes sends while one is in flight).
    pub forecast_requested: Option<(f64, f64)>,
    /// When the forecast/observation currently on screen was fetched, for the
    /// auto-refresh cadence.
    pub forecast_fetched_at: Option<std::time::Instant>,
    /// Last time a surface observation actually landed (the forecast stamp
    /// only marks the first response of a pair). The AI uses this to decide
    /// whether a second question should wait for a refresh.
    pub observation_fetched_at: Option<std::time::Instant>,
    /// Current UV index for the forecast point (Open-Meteo).
    pub uv_index: Option<f32>,
    // Rain/snow totals (NWS observation series around the selected site).
    pub totals_open: bool,
    pub totals: Vec<radar_data::StationTotals>,
    /// Target the loaded totals belong to (lat, lon).
    pub totals_target: Option<(f64, f64)>,
    /// In-flight request: (lat, lon, when sent) — dedupes the 30 s repaint
    /// probe and bounds retries after a failure to once a minute.
    pub totals_requested: Option<(f64, f64, std::time::Instant)>,
    pub totals_fetched_at: Option<std::time::Instant>,
    /// The map overlay: station dot under the pointer (station id) and when.
    pub show_totals: bool,
    pub totals_hover: Option<String>,
    pub totals_hover_at: f64,

    // 3D volume pane — split view beside the 2D map.
    /// Whether the 3D pane is shown beside the map. `STORMSCOPE_3D_OPEN=1`
    /// in the environment opens it at launch (scripted screenshots).
    pub view3d_open: bool,
    /// Which pane fills the central area (`None` = side-by-side split).
    pub pane_max: PaneMax,
    /// Fraction of the central width the map keeps while split.
    pub pane_split: f32,
    /// Orbit camera for the 3D pane. `view3d_dist_km < 1` means "auto-fit".
    pub view3d_yaw: f32,
    pub view3d_pitch: f32,
    pub view3d_dist_km: f32,
    /// Orbit center offset from the site (km, ENU) — moved by panning the
    /// 3D pane (right/middle drag); "Fit" recenters it.
    pub view3d_target_x_km: f32,
    pub view3d_target_y_km: f32,
    pub view3d_target_z_km: f32,
    /// Vertical exaggeration of the volume (height multiplier).
    pub view3d_ve: f32,
    /// Opacity of the volume surfaces.
    pub view3d_alpha: f32,
    /// Echo display threshold, in the current moment's units.
    pub view3d_threshold: f32,
    /// Product shown in the 3D pane, independent of the 2D map's selection.
    pub view3d_moment: Moment,
    /// Mesh density of the 3D pane.
    pub view3d_detail: radar_render::MeshDetail,
    /// Elevation cuts to include, degrees; `None` = unbounded. The 3D
    /// "tilt" control — all cuts render at once unless narrowed.
    pub view3d_tilt_lo: Option<f32>,
    pub view3d_tilt_hi: Option<f32>,
    /// Altitude slab shown, feet (0 = ground, 65k = no ceiling). The shader
    /// discards fragments outside, so moving these never rebuilds geometry.
    pub view3d_alt_lo_ft: f32,
    pub view3d_alt_hi_ft: f32,
    /// Cross-section curtain: whether it is drawn, and the azimuth it cuts
    /// along (degrees clockwise from north).
    pub view3d_section: bool,
    pub view3d_section_az: f32,
    /// Isosurface shells to draw, in dBZ (30 / 50 / 65); only meaningful for
    /// reflectivity-like products.
    pub view3d_iso: [bool; 3],
    /// What the three shell checkboxes mean: convective 30/50/65 dBZ, or the
    /// winter set (snow tops out near 20-25 dBZ, so 30/50/65 draws almost
    /// nothing).
    pub view3d_shell_preset: ShellPreset,
    /// The hover data probe (first + strongest echo under the cursor).
    pub view3d_probe: bool,
    /// The 2D map's hover probe: the value of the displayed product at the
    /// pointer, with its beam height and distance from the radar.
    pub map_probe: bool,
    /// Android-only touch crosshair for reading dBZ without a finger tooltip.
    pub dbz_crosshair: bool,
    pub dbz_crosshair_pos: Option<(f64, f64)>,
    /// Draw the 0 °C level overlay: isohypses of the melting-layer product
    /// (the CC bright band) every 2 kft. Off by default; one tap in the
    /// layers menu when the rain/snow line matters.
    pub show_melting_layer: bool,
    /// Draw the MRMS multi-radar composite under the radar layer.
    pub show_mrms: bool,
    /// Wash strength of the MRMS layer (1 = the table's own colors).
    pub mrms_opacity: f32,
    /// The newest mosaics (CONUS-wide first, the radar window last) and when
    /// they were received (refresh policy).
    pub mrms: Option<(
        Vec<std::sync::Arc<radar_data::mrms::Mosaic>>,
        std::time::Instant,
    )>,
    /// When the in-flight MRMS fetch was requested (rate limit + retry).
    pub mrms_requested_at: Option<std::time::Instant>,
    /// Draw GOES GLM lightning flashes (official NOAA), last 20 minutes.
    pub show_lightning: bool,
    pub lightning: Vec<radar_data::glm::GlmStrike>,
    pub lightning_pending: bool,
    pub lightning_requested_at: Option<std::time::Instant>,
    pub lightning_last_key: Option<String>,
    pub lightning_satellite: Option<String>,
    /// Draw the GOES satellite overlay (NASA GIBS) under the radar.
    pub show_goes: bool,
    /// Wash strength of the satellite layer.
    pub goes_opacity: f32,
    /// Which satellite product: clean infrared or GeoColor.
    pub goes_layer: radar_data::goes::GoesLayer,
    /// The latest frames (CONUS-wide first, the radar window last) and when
    /// they were received.
    pub goes: Option<(
        Vec<std::sync::Arc<radar_data::goes::GoesImage>>,
        std::time::Instant,
    )>,
    /// The (layer, satellite) request in flight, and when it was sent.
    pub goes_pending: Option<(radar_data::goes::GoesLayer, bool)>,
    pub goes_requested_at: Option<std::time::Instant>,
    /// Draw the HRRR FutureCast layer: simulated composite reflectivity for
    /// the next hours. This is a model forecast, not observed radar.
    pub show_futurecast: bool,
    pub futurecast_opacity: f32,
    pub futurecast: Option<FuturecastState>,
    pub futurecast_pending: bool,
    pub futurecast_requested_at: Option<std::time::Instant>,
    /// Site whose box the in-flight/loaded run belongs to; a change means
    /// the old request must not be allowed to repaint the new radar.
    pub futurecast_site: String,
    /// True = the 48-hour extended HRRR cycle; false = latest 18-hour run.
    pub futurecast_extended: bool,
    /// Colour FutureCast by HRRR's categorical precipitation type (snow
    /// blue, ice purple, freezing rain magenta) instead of pure reflectivity.
    pub futurecast_precip_type: bool,
    /// Auto-loop through the forecast frames while the layer is active.
    pub futurecast_playing: bool,
    /// Last frame advance time (egui's input time, seconds).
    pub futurecast_last_step: f64,
    /// NWS warning polygons draped on the 3D ground. Its own switch and its
    /// own wash strength, so the 3D scene can keep the warnings even when the
    /// 2D map's alert layer is off (and vice versa). The fill reads like the
    /// map's slider: 0 = outlines only, 1 = the shipped wash.
    pub view3d_alerts: bool,
    pub view3d_alerts_fill: f32,
    /// Height the warning polygons are extruded to (k ft above the ground):
    /// the polygons wrap the storm instead of lying flat on it. 0 = flat.
    pub view3d_alerts_top_kft: f32,
    /// Storm-relative motion the SRM product subtracts: direction storms
    /// move *toward* (degrees clockwise from north) and speed in knots.
    pub srm_dir_deg: f32,
    pub srm_speed_kt: f32,
    /// SRM display volume, keyed by (frame, motion): derived on the data
    /// thread and swapped in whole, because it is motion-dependent and
    /// therefore cannot ride along at decode like VIL/TOPS/MEHS do.
    pub srm_cache: Option<(u64, Arc<Volume>)>,
    /// Key of the SRM derivation in flight (one at a time; the newest
    /// request is sent when it lands).
    pub srm_pending: Option<u64>,
    /// Key of the SRM volume currently displayed (telemetry/debug).
    pub srm_shown_key: Option<u64>,
    /// Derived storm tracks overlay (cell motion arrows + nowcast lines).
    pub show_tracks: bool,

    // SPC convective outlook overlay. The map draws every product loaded for
    // the selected day (categorical + tornado/wind/hail probabilities);
    // `outlook` is the selected one used by the legend and discussion popup.
    pub outlook: Option<radar_core::outlooks::Outlook>,
    pub outlooks: Vec<radar_core::outlooks::Outlook>,
    pub show_outlook: bool,
    pub outlook_day: u8,
    /// Which outlook product the legend/discussion follows.
    pub outlook_type: radar_core::outlooks::OutlookType,
    /// Draw all four products together (the selector's ALL, the default).
    pub outlook_all_types: bool,
    /// Fill opacity of the outlook polygons (0..=1).
    pub outlook_opacity: f32,
    /// Open state of the SPC discussion popup, which risk area was clicked
    /// (its `DN`), and the fetched discussion text per day.
    pub outlook_detail_open: bool,
    pub outlook_detail_dn: Option<u16>,
    pub outlook_discussion: Option<(u8, String)>,
    /// Day whose discussion is already requested (dedupes sends).
    pub outlook_discussion_requested: Option<u8>,

    // Status surfaced in the status bar (filled by the coordinator).
    pub status_text: String,
    /// Archive window position (from `DataMsg::ArchiveInfo`): `total` scans in
    /// the day, `[archive_start, archive_start+archive_len)` loaded.
    pub archive_total: usize,
    pub archive_start: usize,
    pub archive_len: usize,
    /// On-disk cache sizes in bytes (from `DataMsg::CacheInfo`, refreshed
    /// about once a second by the coordinator) — shown in the Settings menu.
    /// `None` until first reported.
    pub cache_radar_bytes: Option<u64>,
    pub cache_tile_bytes: Option<u64>,

    // Cached radar volumes for the selected site (the timeline's disk strip).
    pub cached_scans: Vec<CachedScan>,
    pub cached_scans_site: String,
    pub cached_scans_at: Option<std::time::Instant>,
    /// Cached days for the selected site, shown by the Downloaded browser.
    pub cached_days: Vec<CachedDay>,
    pub cached_days_site: String,
    pub cached_days_at: Option<std::time::Instant>,
    pub downloads_open: bool,
    /// Day awaiting a second click before its cached scans are deleted.
    pub downloads_confirm: Option<chrono::NaiveDate>,

    // User-placed map markers.
    pub markers: Vec<MapMarker>,
    /// Index of a marker whose label popup is open, if any.
    pub pending_marker: Option<usize>,
    /// Draft text for the open label popup. MUST persist across frames —
    /// egui's TextEdit is immediate-mode: cloning the stored label each frame
    /// would erase every keystroke on the next repaint.
    pub pending_marker_label: String,
    /// A new pin was just dropped, so the name popup should take text focus
    /// (once). Requesting it every frame makes the Android IME flicker.
    pub marker_focus_pending: bool,
    /// Chrome-free fullscreen: only the radar map is drawn (no status bar,
    /// timeline, chips, badges or windows). Toggled by a long press.
    pub fullscreen: bool,
    /// Long-press edge tracking: one fullscreen toggle per press, re-armed on
    /// release (kept across frames).
    pub fullscreen_press_handled: bool,
    /// Any text field had focus this frame. On Android this also means the
    /// IME bridge mirrors `text_focus_text` into Gboard's editor.
    pub text_focus: bool,
    /// Android: the focused field's current value, mirrored every frame so
    /// the IME bridge can seed Gboard's editor state (marker label / AI
    /// composer do this; other fields start Gboard from an empty buffer).
    pub text_focus_text: String,
    /// Android IME bridge: whether a widget held focus last frame.
    pub ime_was_focused: bool,
    /// Android IME bridge: the editor text the IME reported last frame.
    pub ime_last_text: Option<String>,
    /// When true the next map click drops a new marker (the "＋ Place"
    /// toggle; Shift+click works regardless).
    pub add_marker_mode: bool,
    /// Keep the controls sidebar open instead of auto-hiding it.
    /// Unpinned (default) the sidebar slides away and reappears when the
    /// pointer touches the left edge.
    pub sidebar_pinned: bool,
    /// Set by the sidebar's Close button on touch: suppress the hover/wide-zone
    /// reopen until the finger leaves the panel (otherwise the tap that closes
    /// it also counts as a pointer still inside the panel).
    pub sidebar_dismissed: bool,

    // Measure tool (RadarScope-style distance line + blue bubble).
    /// When true, map drags draw a measurement instead of panning.
    pub measure_mode: bool,
    /// Endpoints as (lon, lat). `b` fills in on the first drag/click.
    pub measure_a: Option<(f64, f64)>,
    pub measure_b: Option<(f64, f64)>,
    /// What a drag in progress is moving (endpoint or the bubble itself).
    pub measure_drag: Option<MeasureDrag>,
    /// Bubble offset in screen px from the A–B midpoint (`None` = default
    /// perch above the line). Set by dragging the bubble.
    pub measure_label_offset: Option<(f32, f32)>,
    /// Bubble tapped open: shows km + bearing under the miles readout.
    pub measure_expanded: bool,
    /// Last frame's bubble rect (screen coords) for hit-testing drags and
    /// expand taps. Rewritten every frame the bubble draws.
    pub measure_bubble_rect: Option<egui::Rect>,

    // AI meteorologist. Optional and off until configured: the report window
    // sends one compact JSON snapshot to an OpenAI-compatible endpoint (default
    // OpenCode Go, `deepseek-v4.1-flash`) and shows the model's short brief.
    pub ai_open: bool,
    /// Shrink the report to a small bar so the map the model moved stays visible.
    pub ai_compact: bool,
    /// The Settings → "AI meteorologist…" popup (endpoint/model/key).
    pub ai_settings_open: bool,
    pub ai_base_url: String,
    pub ai_model: String,
    pub ai_api_key: String,
    /// Reveal the key field instead of drawing dots.
    pub ai_show_key: bool,
    /// Persist the key in `ai.json` (0600). Off by default.
    pub ai_remember_key: bool,
    /// A request is in flight (one at a time).
    pub ai_pending: bool,
    pub ai_error: Option<String>,
    pub ai_report: Option<String>,
    /// The spotter's question the current answer belongs to (`None` = the
    /// automatic marker brief).
    pub ai_last_question: Option<String>,
    /// Draft of the "Ask the meteorologist" box (persists across frames).
    pub ai_question: String,
    /// The whole conversation shown in the window (kept for the session).
    pub ai_messages: Vec<AiChatMessage>,
    /// The AI window is size-managed by the app (egui's own vertical resize
    /// subtracts the title bar on the first drag frame, which made the bottom
    /// jump). `ai_resize_drag` = (mask, start pointer, start size).
    pub ai_window_size: egui::Vec2,
    pub ai_resize_drag: Option<(u8, egui::Pos2, egui::Vec2)>,

    /// The question the current request chain is answering (survives the
    /// LOOK round-trip).
    pub ai_question_ctx: Option<String>,
    /// 0 = no look yet; 1 = the follow-up snapshot after a LOOK request.
    pub ai_look_depth: u8,
    /// Places the model asked to sample before answering ("Wichita").
    pub ai_looking_at: Option<String>,
    /// A LOOK waiting for the nearest radar's volume: (label, lat, lon, radius).
    pub ai_pending_look: Option<Vec<(String, f64, f64, f64)>>,
    /// Site being loaded for that look, and when the switch began.
    pub ai_pending_site: Option<String>,
    pub ai_pending_site_at: Option<std::time::Instant>,
    /// Volumes received from that site since the switch (two are needed for
    /// scan-to-scan motion).
    pub ai_pending_site_volumes: u8,
    /// 0 = no data fetch yet; 1 = the follow-up after a DATA request.
    pub ai_data_depth: u8,
    /// Layers/data the current DATA round is waiting for.
    pub ai_pending_data: Vec<String>,
    pub ai_pending_data_at: Option<std::time::Instant>,
    /// The last LOOK areas, so a DATA re-ask keeps its `requested_samples`.
    pub ai_last_look: Vec<(String, f64, f64, f64)>,
    /// What the current report was built from ("KTWX 12:04Z · 3 markers").
    pub ai_snapshot: Option<String>,
    pub ai_generated_at: Option<std::time::Instant>,
    pub ai_test_pending: bool,
    pub ai_test_status: Option<String>,
    /// Android self-update: latest release, modal state and transfer status.
    pub update_available: Option<radar_data::update::UpdateInfo>,
    pub update_dialog_open: bool,
    pub update_check_pending: bool,
    pub update_download_pending: bool,
    pub update_downloaded: Option<std::path::PathBuf>,
    pub update_status: Option<String>,
    pub update_checked_at: Option<std::time::Instant>,
    /// What the model changed on the map ("VEL · 3D split · slice 90°"), if the
    /// last answer carried a `MAP:` directive.
    pub ai_actions: Option<String>,
    /// The view from before that manoeuvre, restored automatically after an
    /// hour (there is no manual Restore button any more).
    pub ai_view_before: Option<AiViewSaved>,
    /// When an AI map manoeuvre should be undone automatically (60 minutes).
    pub ai_view_restore_at: Option<std::time::Instant>,
    /// Marks the model placed on the 2D and 3D maps (a suspected tornado, a
    /// hail core, an updraft top…). Replaced by each answer that marks.
    pub ai_annotations: Vec<AiAnnotation>,

    // Gemini Live voice conversation (Android): a native speech-to-speech
    // session carries the same full weather snapshot the typed AI gets.
    /// Gemini Live API key (Google AI Studio), separate from the text key.
    pub gemini_api_key: String,
    /// Reveal the Gemini key field instead of drawing dots.
    pub gemini_show_key: bool,
    /// Persist the Gemini key in `ai.json` (0600).
    pub gemini_remember_key: bool,
    /// Live model name (`gemini-3.8-live-extended-thinking` by default).
    pub gemini_model: String,
    /// Prebuilt voice name (Aoede, Kore, Puck, …).
    pub gemini_voice: String,

    /// A Gemini Live conversation is running (tap Talk / End).
    pub voice_convo: bool,
    /// The session is set up and the microphone is streaming.
    pub voice_listening: bool,
    /// Live transcript while the user talks.
    pub voice_partial: String,
    /// Model audio is playing.
    pub voice_speaking: bool,
    /// What the voice footer shows ("Connecting…", "Listening…", an error).
    pub voice_status: Option<String>,
    /// Input level 0..1 for the little equalizer.
    pub voice_level: f32,
    /// Last time the Java audio bridge was polled (throttles the JNI call).
    pub voice_polled_at: Option<std::time::Instant>,
    /// Current turn's user transcript, pushed to the chat on turn complete.
    pub voice_user_text: String,
    /// Current turn's spoken reply transcript, pushed on turn complete.
    pub voice_reply_text: String,
    /// Last time either side said anything; a listening session with no
    /// activity for 60 seconds ends so the mic and Live session do not stay
    /// open indefinitely.
    pub voice_last_activity: Option<std::time::Instant>,
    /// Talk was tapped and the fresh forecast/observation fetch is in flight.
    pub voice_pending_start: bool,
    /// When that fetch began (used for the fallback timeout).
    pub voice_pending_at: Option<std::time::Instant>,
    /// A rain/snow totals fetch is part of the pre-session wait.
    pub voice_totals_wanted: bool,

    /// Playhead advance accumulator for LoopState::Playing.
    _acc: f32,
}

impl Default for AppState {
    fn default() -> Self {
        // Scripted screenshots: `STORMSCOPE_LAYERS=mrms,goes` opens the data
        // overlays at launch, the same spirit as `STORMSCOPE_3D_OPEN`.
        let layers = std::env::var("STORMSCOPE_LAYERS").unwrap_or_default();
        // AI endpoint: the Settings popup's saved values, seeded by the
        // environment. The base URL and model default to OpenCode Go
        // (`deepseek-v4.1-flash`); the key is left for the user to paste.
        let ai = load_ai_prefs();
        let ai_base_url = std::env::var("STORMSCOPE_AI_BASE_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or(ai.base_url.clone())
            .unwrap_or_else(|| radar_data::ai::OPENCODE_GO_BASE.to_string());
        let ai_model = std::env::var("STORMSCOPE_AI_MODEL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or(ai.model.clone())
            .unwrap_or_else(|| radar_data::ai::OPENCODE_GO_MODEL.to_string());
        let ai_api_key = std::env::var(radar_data::ai::OPENCODE_GO_KEY_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| std::env::var("STORMSCOPE_AI_KEY").ok())
            .or(ai.api_key.clone())
            .unwrap_or_default();
        let gemini_api_key = std::env::var("GEMINI_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or(ai.gemini_api_key.clone())
            .unwrap_or_default();
        let gemini_model = std::env::var("STORMSCOPE_GEMINI_MODEL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            // The old saved default predates the extended-thinking model; treat
            // it as "not chosen" so existing configs pick up the new default.
            .or_else(|| {
                ai.gemini_model
                    .clone()
                    .filter(|model| model.trim() != "gemini-3.8-live")
            })
            .unwrap_or_else(|| "gemini-3.8-live-extended-thinking".to_string());
        let gemini_voice = ai
            .gemini_voice
            .clone()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "Aoede".to_string());
        let voice = load_voice_alert_prefs();
        let site = radar_core::sites::site(radar_core::DEFAULT_SITE)
            .unwrap_or_else(|| radar_core::sites::SITES.first().expect("site catalog"));
        Self {
            mode: Mode::Live,
            archive_date: chrono::Utc::now()
                .with_timezone(crate::timefmt::central())
                .date_naive(),
            archive_popup_open: false,
            product_help_open: false,
            archive_crawl: None,
            archive_warnings: Vec::new(),
            archive_warning_view: Vec::new(),
            archive_warnings_date: None,
            archive_warnings_requested: None,
            archive_jump: None,
            load_progress: None,
            site_mismatch_logged: None,
            archive_detail: None,
            archive_texts: std::collections::HashMap::new(),
            archive_text_requested: std::collections::HashSet::new(),
            archive_hover: None,
            archive_hover_at: 0.0,
            archive_focus: None,
            map_controls_open: false,
            view3d_controls_open: false,
            archive_minute: {
                use chrono::Timelike;
                let now = chrono::Utc::now().with_timezone(crate::timefmt::central());
                now.hour() * 60 + now.minute()
            },
            loop_state: LoopState::Paused,
            fps: 8.0,
            site_id: site.id.to_string(),
            moment: Moment::Reflectivity,
            tilt_deg: 0.5,
            opacity: 1.0,
            // Suppresses the clear-air clutter/haze that reads as a green
            // Snow lives at 10-25 dBZ: a 30 dBZ floor (the convective
            // setting) hid everything but the heaviest winter cores, so the
            // default sits at 10. Raise it from the map's Controls window
            // when summer clutter needs thinning.
            min_dbz: 20.0,
            view: MapView {
                center_lat: site.latitude,
                center_lon: site.longitude,
                zoom: 7.5,
                width_px: 1280.0,
                height_px: 800.0,
            },
            ring: VecDeque::with_capacity(RING_CAPACITY),
            playhead: 0,
            ring_bytes: 0,
            ring_budget_bytes: RING_BUDGET_BYTES,
            alerts: Vec::new(),
            alerts_slow: Vec::new(),
            alerts_fast: Vec::new(),
            all_alerts: Vec::new(),
            show_alerts: true,
            alerts_popup_open: false,
            alerts_tab: AlertsTab::Station,
            alerts_filter: String::new(),
            voice_alerts: voice.enabled,
            voice_tornado: voice.tornado,
            voice_severe_thunderstorm: voice.severe_thunderstorm,
            voice_watches: voice.watches,
            voice_other_warnings: voice.other_warnings,
            voice_model: voice.model,
            storm_watch_enabled: false,
            storm_watch_polled_at: f64::NEG_INFINITY,
            alerts_hover: None,
            alerts_map_stack: Vec::new(),
            alerts_fill: 0.45,
            outlook_hover: None,
            outlook_hover_at: 0.0,
            alerts_map_hover_at: 0.0,
            alerts_focus: None,
            alerts_focus_at: 0.0,
            alert_detail: None,
            forecast: None,
            observation: None,
            lsrs: Vec::new(),
            show_lsrs: true,
            lsr_hover: None,
            lsr_hover_at: 0.0,
            lsr_detail: None,
            mcds: Vec::new(),
            show_mcds: true,
            mcd_hover: None,
            mcd_hover_at: 0.0,
            mcd_detail: None,
            forecast_open: false,
            forecast_pick_mode: false,
            forecast_reopen_after_pick: false,
            forecast_point: None,
            forecast_point_gps: false,
            forecast_site: String::new(),
            forecast_requested: None,
            forecast_fetched_at: None,
            observation_fetched_at: None,
            uv_index: None,
            gps_fix: None,
            gps_polled_at: f64::NEG_INFINITY,
            gps_applied: false,
            gps_waiting: false,
            ime_inset_pt: 0.0,
            ime_polled_at: f64::NEG_INFINITY,
            totals_open: false,
            totals: Vec::new(),
            totals_target: None,
            totals_requested: None,
            totals_fetched_at: None,
            show_totals: true,
            totals_hover: None,
            totals_hover_at: 0.0,
            view3d_open: std::env::var_os("STORMSCOPE_3D_OPEN").is_some(),
            pane_max: if std::env::var_os("STORMSCOPE_3D_OPEN").is_some_and(|v| v == "max") {
                PaneMax::View3d
            } else {
                PaneMax::None
            },
            pane_split: 0.5,
            view3d_yaw: 0.0,
            view3d_pitch: 38.0,
            view3d_dist_km: 0.0,
            view3d_target_x_km: 0.0,
            view3d_target_y_km: 0.0,
            view3d_target_z_km: 0.0,
            // Real storms are ~13-15 km tall over a 300 km-wide scan. A
            // gentle default (×6) keeps the ground scale near the 2D map's
            // while still lifting the structure; the slider goes to ×20 for
            // when the vertical is the story.
            view3d_ve: 6.0,
            view3d_alpha: 1.0,
            view3d_threshold: 5.0,
            view3d_moment: Moment::Reflectivity,
            view3d_detail: radar_render::MeshDetail::Fine,
            view3d_tilt_lo: None,
            view3d_tilt_hi: None,
            view3d_alt_lo_ft: 0.0,
            view3d_alt_hi_ft: 65_000.0,
            view3d_section: false,
            view3d_section_az: 90.0,
            view3d_iso: [false, false, false],
            view3d_shell_preset: ShellPreset::Convective,
            view3d_probe: true,
            map_probe: true,
            dbz_crosshair: false,
            dbz_crosshair_pos: None,
            show_melting_layer: false,
            show_mrms: layers.contains("mrms"),
            mrms_opacity: 0.55,
            mrms: None,
            mrms_requested_at: None,
            // Lightning is on by default; STORMSCOPE_LAYERS still acts as an
            // explicit layer whitelist when set.
            show_lightning: layers.is_empty() || layers.contains("lightning"),
            lightning: Vec::new(),
            lightning_pending: false,
            lightning_requested_at: None,
            lightning_last_key: None,
            lightning_satellite: None,
            show_goes: layers.contains("goes"),
            goes_opacity: 0.55,
            goes_layer: radar_data::goes::GoesLayer::Infrared,
            goes: None,
            goes_pending: None,
            goes_requested_at: None,
            show_futurecast: layers.contains("futurecast"),
            futurecast_opacity: 0.8,
            futurecast: None,
            futurecast_pending: false,
            futurecast_requested_at: None,
            futurecast_site: String::new(),
            futurecast_extended: std::env::var_os("STORMSCOPE_FUTURECAST_48").is_some(),
            futurecast_precip_type: true,
            futurecast_playing: layers.contains("futurecast"),
            futurecast_last_step: 0.0,
            view3d_alerts: true,
            view3d_alerts_fill: 0.45,
            view3d_alerts_top_kft: 30.0,
            // A typical central-US supercell motion: east/southeast at 30 kt.
            srm_dir_deg: 275.0,
            srm_speed_kt: 30.0,
            srm_cache: None,
            srm_pending: None,
            srm_shown_key: None,
            show_tracks: true,
            outlook: None,
            outlooks: Vec::new(),
            // On by default with the outline-only look (fill 0.0): the risk
            // boundaries show over the radar without tinting the echoes.
            show_outlook: true,
            outlook_day: 1,
            outlook_type: radar_core::outlooks::OutlookType::Categorical,
            outlook_all_types: true,
            outlook_opacity: 0.0,
            outlook_detail_open: false,
            outlook_detail_dn: None,
            outlook_discussion: None,
            outlook_discussion_requested: None,
            status_text: "starting…".to_string(),
            archive_total: 0,
            archive_start: 0,
            archive_len: 0,
            cache_radar_bytes: None,
            cache_tile_bytes: None,
            cached_scans: Vec::new(),
            cached_scans_site: String::new(),
            cached_scans_at: None,
            cached_days: Vec::new(),
            cached_days_site: String::new(),
            cached_days_at: None,
            downloads_open: false,
            downloads_confirm: None,
            markers: load_markers(),
            pending_marker: None,
            pending_marker_label: String::new(),
            fullscreen: false,
            fullscreen_press_handled: false,
            marker_focus_pending: false,
            text_focus: false,
            text_focus_text: String::new(),
            ime_was_focused: false,
            ime_last_text: None,
            add_marker_mode: false,
            sidebar_pinned: false,
            sidebar_dismissed: false,
            measure_mode: false,
            measure_a: None,
            measure_b: None,
            measure_drag: None,
            measure_label_offset: None,
            measure_expanded: false,
            measure_bubble_rect: None,
            ai_open: false,
            ai_compact: false,
            ai_settings_open: false,
            ai_base_url,
            ai_model,
            ai_api_key,
            ai_show_key: false,
            ai_remember_key: ai.remember_key.unwrap_or(ai.api_key.is_some()),
            ai_pending: false,
            ai_error: None,
            ai_report: None,
            ai_last_question: None,
            ai_question: String::new(),
            ai_messages: load_ai_chat(),
            ai_window_size: egui::Vec2::ZERO,
            ai_resize_drag: None,
            ai_question_ctx: None,
            ai_look_depth: 0,
            ai_looking_at: None,
            ai_pending_look: None,
            ai_pending_site: None,
            ai_pending_site_at: None,
            ai_pending_site_volumes: 0,
            ai_data_depth: 0,
            ai_pending_data: Vec::new(),
            ai_pending_data_at: None,
            ai_last_look: Vec::new(),
            ai_snapshot: None,
            ai_generated_at: None,
            ai_test_pending: false,
            ai_test_status: None,
            update_available: None,
            update_dialog_open: false,
            update_check_pending: false,
            update_download_pending: false,
            update_downloaded: None,
            update_status: None,
            update_checked_at: None,
            ai_actions: None,
            ai_view_before: None,
            ai_view_restore_at: None,
            ai_annotations: Vec::new(),
            gemini_api_key,
            gemini_show_key: false,
            gemini_remember_key: ai
                .gemini_remember_key
                .unwrap_or(ai.gemini_api_key.is_some()),
            gemini_model,
            gemini_voice,
            voice_convo: false,
            voice_listening: false,
            voice_partial: String::new(),
            voice_speaking: false,
            voice_status: None,
            voice_level: 0.0,
            voice_polled_at: None,
            voice_user_text: String::new(),
            voice_reply_text: String::new(),
            voice_last_activity: None,
            voice_pending_start: false,
            voice_pending_at: None,
            voice_totals_wanted: false,
            _acc: 0.0,
        }
    }
}

/// `~/.config/stormscope` on Unix, `%APPDATA%\stormscope` on Windows, where
/// the app's own small JSON files live.
fn config_dir() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        if let Some(base) = std::env::var_os("APPDATA") {
            return std::path::PathBuf::from(base).join("stormscope");
        }
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
                .join(".config")
        });
    base.join("stormscope")
}

/// Open a file for private writing: 0600 on Unix (the AI config can hold an
/// API key), the default per-user ACL on Windows.
fn private_writer(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

/// `~/.config/stormscope/markers.json`, where user pins are persisted.
fn markers_path() -> std::path::PathBuf {
    config_dir().join("markers.json")
}

/// `~/.config/stormscope/voice-alerts.json`: the master switch plus one
/// switch per alert family. The previous single-switch `voice-alerts` file is
/// still read once for migration.
fn voice_alerts_path() -> std::path::PathBuf {
    config_dir().join("voice-alerts.json")
}

fn legacy_voice_alerts_path() -> std::path::PathBuf {
    config_dir().join("voice-alerts")
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct VoiceAlertPrefs {
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default = "default_true")]
    tornado: bool,
    #[serde(default = "default_true")]
    severe_thunderstorm: bool,
    #[serde(default = "default_true")]
    watches: bool,
    #[serde(default = "default_true")]
    other_warnings: bool,
    /// Piper voice id; `None` follows the best installed voice.
    #[serde(default)]
    model: Option<String>,
}

impl Default for VoiceAlertPrefs {
    fn default() -> Self {
        Self {
            enabled: true,
            tornado: true,
            severe_thunderstorm: true,
            watches: true,
            other_warnings: true,
            model: None,
        }
    }
}

fn default_true() -> bool {
    true
}

fn load_voice_alert_prefs() -> VoiceAlertPrefs {
    if let Ok(text) = std::fs::read_to_string(voice_alerts_path())
        && let Ok(prefs) = serde_json::from_str(&text)
    {
        return prefs;
    }
    // Migrate the old plain on/off file, if the user had used it.
    let enabled = std::fs::read_to_string(legacy_voice_alerts_path())
        .map(|text| !matches!(text.trim(), "off" | "false" | "0"))
        .unwrap_or(true);
    VoiceAlertPrefs {
        enabled,
        ..Default::default()
    }
}

/// `~/.config/stormscope/ai.json`: endpoint, model and (only with the
/// "remember" box ticked) the API key.
fn ai_path() -> std::path::PathBuf {
    config_dir().join("ai.json")
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct AiPrefs {
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    remember_key: Option<bool>,
    #[serde(default)]
    gemini_api_key: Option<String>,
    #[serde(default)]
    gemini_model: Option<String>,
    #[serde(default)]
    gemini_voice: Option<String>,
    #[serde(default)]
    gemini_remember_key: Option<bool>,
}

/// `~/.config/stormscope/ai-chat.json`: the last conversation, capped, so a
/// restart does not lose what was said and answered.
fn ai_chat_path() -> std::path::PathBuf {
    config_dir().join("ai-chat.json")
}

fn load_ai_chat() -> Vec<AiChatMessage> {
    match std::fs::read_to_string(ai_chat_path()) {
        Ok(s) => {
            let mut messages: Vec<AiChatMessage> = serde_json::from_str(&s).unwrap_or_default();
            if messages.len() > 60 {
                let cut = messages.len() - 60;
                messages.drain(..cut);
            }
            messages
        }
        Err(_) => Vec::new(),
    }
}

fn load_ai_prefs() -> AiPrefs {
    match std::fs::read_to_string(ai_path()) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => AiPrefs::default(),
    }
}

fn load_markers() -> Vec<MapMarker> {
    match std::fs::read_to_string(markers_path()) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

impl AppState {
    /// Age of the loaded surface observation in seconds (None if never one).
    pub fn observation_age_secs(&self) -> Option<f64> {
        self.observation_fetched_at
            .map(|t| t.elapsed().as_secs_f64())
    }

    /// Persist the spoken-alert switches (best-effort).
    pub fn persist_voice_alerts(&self) {
        let path = voice_alerts_path();
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            log::warn!("voice alerts dir: {e}");
            return;
        }
        let prefs = VoiceAlertPrefs {
            enabled: self.voice_alerts,
            tornado: self.voice_tornado,
            severe_thunderstorm: self.voice_severe_thunderstorm,
            watches: self.voice_watches,
            other_warnings: self.voice_other_warnings,
            model: self.voice_model.clone(),
        };
        let Ok(text) = serde_json::to_string_pretty(&prefs) else {
            log::warn!("voice alerts serialize failed");
            return;
        };
        if let Err(e) = std::fs::write(&path, text) {
            log::warn!("voice alerts write: {e}");
        }
    }

    /// Write markers to disk (best-effort; a failed write is logged, not
    /// fatal — losing pins shouldn't lose the session).
    pub fn persist_markers(&self) {
        let path = markers_path();
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                log::warn!("markers dir: {e}");
                return;
            }
        }
        match serde_json::to_string_pretty(&self.markers) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    log::warn!("markers write: {e}");
                }
            }
            Err(e) => log::warn!("markers serialize: {e}"),
        }
    }

    /// Write the conversation to disk (0600), capped. Best-effort: a failed
    /// write must never lose the session.
    pub fn persist_ai_chat(&self) {
        let Ok(json) = serde_json::to_string_pretty(&self.ai_messages) else {
            return;
        };
        let path = ai_chat_path();
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                log::warn!("AI chat dir: {e}");
                return;
            }
        }
        use std::io::Write;
        match private_writer(&path) {
            Ok(mut file) => {
                if let Err(e) = file.write_all(json.as_bytes()) {
                    log::warn!("AI chat write: {e}");
                }
            }
            Err(e) => log::warn!("AI chat open: {e}"),
        }
    }

    /// Write the AI endpoint configuration. The key is only written when the
    /// user ticked "remember"; the file is created 0600 either way.
    pub fn persist_ai(&self) {
        let prefs = AiPrefs {
            base_url: Some(self.ai_base_url.clone()),
            model: Some(self.ai_model.clone()),
            api_key: self.ai_remember_key.then(|| self.ai_api_key.clone()),
            remember_key: Some(self.ai_remember_key),
            gemini_api_key: self
                .gemini_remember_key
                .then(|| self.gemini_api_key.clone()),
            gemini_model: Some(self.gemini_model.clone()),
            gemini_voice: Some(self.gemini_voice.clone()),
            gemini_remember_key: Some(self.gemini_remember_key),
        };
        let Ok(json) = serde_json::to_string_pretty(&prefs) else {
            return;
        };
        let path = ai_path();
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                log::warn!("AI settings dir: {e}");
                return;
            }
        }
        use std::io::Write;
        match private_writer(&path) {
            Ok(mut file) => {
                if let Err(e) = file.write_all(json.as_bytes()) {
                    log::warn!("AI settings write: {e}");
                }
            }
            Err(e) => log::warn!("AI settings open: {e}"),
        }
    }

    /// The RadioSite-independent AI config the background worker needs.
    pub fn ai_config(&self) -> radar_data::ai::AiConfig {
        radar_data::ai::AiConfig {
            base_url: self.ai_base_url.clone(),
            model: self.ai_model.clone(),
            api_key: self.ai_api_key.clone(),
            ..Default::default()
        }
    }

    /// Current frame at the playhead, if any volume is loaded.
    pub fn current(&self) -> Option<&Frame> {
        self.ring.get(self.playhead)
    }

    /// The volume the display paths (map raster, 3D pane, probe) read for
    /// the selected product.
    ///
    /// Derived column products (VIL / Echo Tops / MEHS) ride inside the
    /// frame's volume — attached on the decode thread — so this is simply
    /// the frame. Storm-relative velocity cannot: it depends on the motion
    /// setting, so it comes from the motion-keyed cache, and while a new
    /// derivation is still building the last one stays on screen (one
    /// derivation behind is far better than a blank pane).
    pub fn display_volume(&self) -> Option<Arc<Volume>> {
        self.display_volume_for(self.moment)
    }

    /// The volume for a specific display product: the frame volume, or the
    /// motion-keyed SRM cache when that product is requested. This lets the
    /// 3D pane show a different moment from the 2D map.
    pub fn display_volume_for(&self, moment: Moment) -> Option<Arc<Volume>> {
        let frame = self.current()?;
        if moment == Moment::StormRelativeVelocity {
            return self.srm_cache.as_ref().map(|(_, v)| Arc::clone(v));
        }
        Some(Arc::clone(&frame.volume))
    }

    /// Key of the SRM derivation the current frame + motion setting needs.
    pub fn srm_key(&self) -> Option<u64> {
        let frame = self.current()?;
        Some(srm_key_of(
            &frame.volume,
            self.srm_dir_deg,
            self.srm_speed_kt,
        ))
    }

    /// Whether the selected product is one of the derived ones.
    pub fn derived_product_selected(&self) -> bool {
        matches!(
            self.moment,
            Moment::Vil | Moment::EchoTops | Moment::Mesh | Moment::StormRelativeVelocity
        )
    }

    /// Whether the radar is scanning in precip or clear-air mode: VCP table
    /// first, else the median scan-to-scan cadence in the ring (precip
    /// volumes arrive every ~4-6 min, clear-air every ~9-12 min).
    pub fn scan_mode(&self) -> radar_core::vcp::ScanMode {
        let vcp = self.current().and_then(|f| f.volume.meta.vcp);
        let mut dts: Vec<f64> = (1..self.ring.len())
            .map(|i| {
                (self.ring[i].volume_start - self.ring[i - 1].volume_start).num_seconds() as f64
                    / 60.0
            })
            .filter(|d| (0.0..120.0).contains(d))
            .collect();
        dts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = if dts.is_empty() {
            None
        } else {
            Some(dts[dts.len() / 2])
        };
        radar_core::vcp::ScanMode::detect(vcp, median)
    }

    /// Push a newly decoded volume into the ring, keeping it ordered oldest
    /// -> newest and evicting the oldest when full.
    ///
    /// Volumes are inserted at their time position rather than blindly
    /// appended: delivery order isn't guaranteed (a backfill of scans already
    /// already have on disk races the live poller), so a late or stale scan
    /// must neither be silently dropped nor clobber the playhead. In Live
    /// mode the playhead follows the newest frame; otherwise it stays on the
    /// frame being viewed.
    pub fn push_volume(&mut self, volume: Arc<Volume>) {
        // Frames are georeferenced to the selected station — another
        // station's sweeps would draw in the wrong place. "UNKNOWN" is
        // tolerated because a headerless stream can omit the site id.
        if volume.meta.site_id != self.site_id && volume.meta.site_id != "UNKNOWN" {
            // Dropping silently is how a UI/stream desync reads in the
            // field: the map stays empty while the decoder hums along.
            // One warn per distinct mismatched site, so it is visible.
            if self.site_mismatch_logged.as_deref() != Some(volume.meta.site_id.as_str()) {
                log::warn!(
                    "dropping {} volumes: the app is watching {} — pick {} in the site selector to resync",
                    volume.meta.site_id,
                    self.site_id,
                    volume.meta.site_id
                );
                self.site_mismatch_logged = Some(volume.meta.site_id.clone());
            }
            return;
        }
        self.site_mismatch_logged = None;
        let start = volume.meta.volume_start;
        if self.ring.iter().any(|f| f.volume_start == start) {
            return; // duplicate
        }
        let pos = self.ring.partition_point(|f| f.volume_start < start);
        let bytes = volume_bytes(&volume);
        if self.mode != Mode::Live && !self.ring.is_empty() && pos <= self.playhead {
            // Older scans landing before the current frame must not move the
            // view off it.
            self.playhead += 1;
        }
        self.ring.insert(
            pos,
            Frame {
                volume_start: start,
                bytes,
                volume,
            },
        );
        self.ring_bytes += bytes;
        // Trim: the count cap AND the memory budget. The budget is what
        // actually binds on precip VCPs (150-200 MB/volume), and it is the
        // difference between a 5 GB overnight footprint and a ~3 GB one —
        // which matters because this app shares RAM (and, on this Intel GPU,
        // GEM/VRAM) with whatever else the desktop runs.
        while self.ring.len() > RING_CAPACITY
            || (self.ring.len() > MIN_RING_FRAMES && self.ring_bytes > self.ring_budget_bytes)
        {
            let Some(old) = self.ring.pop_front() else {
                break;
            };
            self.ring_bytes = self.ring_bytes.saturating_sub(old.bytes);
            self.playhead = self.playhead.saturating_sub(1);
        }
        if self.mode == Mode::Live {
            self.playhead = self.ring.len().saturating_sub(1);
        } else {
            self.playhead = self.playhead.min(self.ring.len().saturating_sub(1));
        }
    }

    pub fn newest_start(&self) -> Option<DateTime<Utc>> {
        self.ring.back().map(|f| f.volume_start)
    }

    /// Latency of the newest frame vs now, for the status bar.
    pub fn latency(&self) -> Option<chrono::Duration> {
        self.newest_start().map(|t| Utc::now() - t)
    }

    pub fn step_forward(&mut self) {
        if self.playhead + 1 < self.ring.len() {
            self.playhead += 1;
        }
    }

    pub fn step_back(&mut self) {
        self.playhead = self.playhead.saturating_sub(1);
    }

    /// Step the FutureCast scrubber by `delta` frames, clamping at the ends.
    /// A manual step pauses playback so the arrow key result is not
    /// immediately overwritten by the auto-loop.
    pub fn futurecast_step(&mut self, delta: isize) {
        let Some(fc) = self.futurecast.as_mut() else {
            return;
        };
        if fc.plan.is_empty() {
            return;
        }
        let last = fc.plan.len() - 1;
        let next = (fc.selected as isize + delta).clamp(0, last as isize) as usize;
        if next != fc.selected {
            fc.selected = next;
            self.futurecast_playing = false;
            self.futurecast_last_step = 0.0;
        }
    }

    /// Advance the playhead on a timer when playing. Called each frame with
    /// the elapsed time since last tick.
    pub fn tick(&mut self, dt: std::time::Duration) {
        if self.loop_state != LoopState::Playing || self.ring.len() < 2 {
            return;
        }
        self._acc += dt.as_secs_f32();
        let frame_time = 1.0 / self.fps.max(0.1);
        while self._acc >= frame_time {
            self._acc -= frame_time;
            if self.playhead + 1 < self.ring.len() {
                self.playhead += 1;
            } else {
                self.playhead = 0; // loop around
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The outlook layer starts on the product everyone reads by default.
    #[test]
    fn default_outlook_type_is_categorical() {
        assert_eq!(
            AppState::default().outlook_type,
            radar_core::outlooks::OutlookType::Categorical
        );
    }
    use radar_core::VolumeMeta;

    fn at(minute: u32) -> DateTime<Utc> {
        chrono::NaiveDate::from_ymd_opt(2026, 10, 13)
            .unwrap()
            .and_hms_opt(12, minute, 0)
            .unwrap()
            .and_utc()
    }

    fn vol(site: &str, start: DateTime<Utc>) -> Arc<Volume> {
        Arc::new(Volume {
            meta: VolumeMeta {
                site_id: site.to_string(),
                volume_start: start,
                vcp: None,
                sequence_number: None,
                sweep_count: 0,
                moments: vec![],
            },
            sweeps: Vec::new(),
        })
    }

    fn state() -> AppState {
        let mut s = AppState::default();
        s.site_id = "KTWX".into();
        s
    }

    fn order(s: &AppState) -> Vec<DateTime<Utc>> {
        s.ring.iter().map(|f| f.volume_start).collect()
    }

    /// Out-of-order arrivals must still land in time order: a backfill of
    /// cached scans races the live poller, and dropping a late frame would
    /// lose data the app already paid to decode.
    #[test]
    fn push_volume_inserts_by_time_not_arrival_order() {
        let mut s = state();
        s.push_volume(vol("KTWX", at(50)));
        s.push_volume(vol("KTWX", at(10))); // stale / late arrival
        assert_eq!(order(&s), vec![at(10), at(50)]);
        // Live mode still shows the newest frame.
        assert_eq!(s.playhead, 1);
        assert_eq!(s.current().map(|f| f.volume_start), Some(at(50)));
    }

    #[test]
    fn push_volume_ignores_duplicates_and_other_sites() {
        let mut s = state();
        s.push_volume(vol("KTWX", at(10)));
        s.push_volume(vol("KTWX", at(10))); // duplicate
        s.push_volume(vol("KTLX", at(20))); // georeferenced elsewhere
        assert_eq!(s.ring.len(), 1, "dup + foreign site dropped");
        // A stream that can't tell us the site must not be thrown away.
        s.push_volume(vol("UNKNOWN", at(30)));
        assert_eq!(s.ring.len(), 2);
        assert_eq!(s.newest_start(), Some(at(30)));
    }

    /// The display volume follows the product: derived column products ride
    /// in the frame, SRM comes from the motion-keyed cache (its absence is
    /// what the panes show as "deriving…"), and the key changes whenever the
    /// frame or the motion setting does.
    #[test]
    fn display_volume_tracks_the_selected_product() {
        let mut s = state();
        s.push_volume(vol("KTWX", at(10)));
        let frame_vol = Arc::clone(&s.ring[0].volume);
        // Any base or column product reads the frame's volume, including the
        // Arc identity the raster/3D signatures key on.
        assert_eq!(
            Arc::as_ptr(&s.display_volume().expect("frame volume")),
            Arc::as_ptr(&frame_vol)
        );
        s.moment = radar_core::Moment::StormRelativeVelocity;
        assert!(s.display_volume().is_none(), "no SRM derived yet");

        let key = s.srm_key().expect("key for the current frame");
        let srm = Arc::new(Volume {
            meta: VolumeMeta {
                site_id: "KTWX".into(),
                volume_start: at(10),
                vcp: None,
                sequence_number: None,
                sweep_count: 0,
                moments: vec![radar_core::Moment::StormRelativeVelocity],
            },
            sweeps: Vec::new(),
        });
        s.srm_cache = Some((key, Arc::clone(&srm)));
        assert_eq!(
            Arc::as_ptr(&s.display_volume().expect("cached SRM")),
            Arc::as_ptr(&srm)
        );
        // Motion changes make a new key: the pane re-derives, and until it
        // lands the previous volume keeps drawing.
        let mut other = state();
        other.push_volume(vol("KTWX", at(10)));
        other.srm_dir_deg = s.srm_dir_deg + 10.0;
        assert_ne!(s.srm_key(), other.srm_key());
        assert!(s.derived_product_selected());
    }

    /// The station sets are merged for display: the sweep set keeps its
    /// order, and fast-lane alerts the sweep has not caught up with yet are
    /// appended once (never duplicated when both have them).
    #[test]
    fn merged_alerts_dedupe_by_id() {
        let alert = |id: &str, event: &str| radar_core::warnings::Warning {
            id: id.into(),
            event: event.into(),
            severity: "Severe".into(),
            urgency: "Expected".into(),
            expires: Utc::now(),
            office: "OUN".into(),
            headline: String::new(),
            description: String::new(),
            instruction: String::new(),
            area: String::new(),
            polygon: None,
        };
        let slow = vec![alert("a", "Tornado Watch"), alert("b", "Flood Warning")];
        let fast = vec![
            alert("b", "Flood Warning"),   // also in the sweep: deduped
            alert("c", "Tornado Warning"), // new, just issued
        ];
        let merged = merge_alerts(&slow, &fast);
        let ids: Vec<&str> = merged.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    /// Scrubbing back in Archive mode must not be yanked forward when an older
    /// scan backfills in ahead of the playhead.
    #[test]
    fn archive_mode_keeps_the_selected_frame_when_backfilling() {
        let mut s = state();
        s.mode = Mode::Archive;
        s.push_volume(vol("KTWX", at(50)));
        s.push_volume(vol("KTWX", at(40)));
        s.push_volume(vol("KTWX", at(30)));
        s.playhead = 0; // user scrubs to the oldest frame (minute 30)
        s.push_volume(vol("KTWX", at(10))); // older scan backfills in
        assert_eq!(s.ring.len(), 4);
        assert_eq!(s.current().map(|f| f.volume_start), Some(at(30)));
    }

    /// The ring must respect the memory budget: with a tiny budget the oldest
    /// frames are evicted (but never below MIN_RING_FRAMES), and the
    /// accounting tracks what is resident.
    #[test]
    fn observation_age_tracks_freshness() {
        use std::time::Duration;
        let mut state = AppState::default();
        assert!(state.observation_age_secs().is_none());
        state.observation_fetched_at = Some(std::time::Instant::now());
        assert!(state.observation_age_secs().unwrap() < 1.0);
        state.observation_fetched_at = Some(std::time::Instant::now() - Duration::from_secs(180));
        assert!(state.observation_age_secs().unwrap() > 120.0);
    }

    #[test]
    fn ring_evicts_by_memory_budget() {
        let mut s = state();
        // ~1 MB per volume.
        let big = Arc::new(Volume {
            meta: VolumeMeta {
                site_id: "KTWX".into(),
                volume_start: at(0),
                vcp: None,
                sequence_number: None,
                sweep_count: 1,
                moments: vec![],
            },
            sweeps: vec![radar_core::Sweep {
                elevation_deg: 0.5,
                elevation_measured_deg: 0.5,
                azimuth_width_deg: 0.5,
                azimuths_deg: vec![0.0; 10],
                moments: vec![radar_core::MomentData {
                    kind: radar_core::Moment::Reflectivity,
                    calibration: radar_core::MomentCalibration::default(),
                    range_start_m: 0.0,
                    gate_size_m: 250.0,
                    gate_count: 500_000,
                    ray_count: 1,
                    data: vec![0i16; 500_000],
                }],
            }],
        });
        let per_frame = volume_bytes(&big);
        assert!(
            per_frame >= 1_000_000,
            "fixture should be ~1 MB, got {per_frame}"
        );

        // Budget for ~4 frames, with a floor of MIN_RING_FRAMES (8).
        s.ring_budget_bytes = per_frame * 4;
        for m in 0..30u32 {
            let mut v = (*big).clone();
            v.meta.volume_start = at(m % 59);
            s.push_volume(Arc::new(v));
        }
        // Floor wins over the budget until the frame count exceeds it.
        assert_eq!(s.ring.len(), MIN_RING_FRAMES);
        assert!(s.ring_bytes <= per_frame * MIN_RING_FRAMES + per_frame);
        assert_eq!(s.ring_bytes, s.ring.iter().map(|f| f.bytes).sum::<usize>());
        // The newest frame is what Live mode shows.
        assert_eq!(s.playhead, s.ring.len() - 1);
    }
}

/// Isosurface threshold presets: the convective set, and a winter set for the
/// 10-30 dBZ world snow lives in.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ShellPreset {
    #[default]
    Convective,
    Winter,
}

impl ShellPreset {
    pub fn dbz(self) -> [f32; 3] {
        match self {
            ShellPreset::Convective => [30.0, 50.0, 65.0],
            ShellPreset::Winter => [10.0, 20.0, 30.0],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ShellPreset::Convective => "Convective",
            ShellPreset::Winter => "Winter",
        }
    }

    pub const ALL: [ShellPreset; 2] = [ShellPreset::Convective, ShellPreset::Winter];
}
