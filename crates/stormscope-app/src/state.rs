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
pub const RING_CAPACITY: usize = 24;

/// Resident-memory budget for the animation ring. Evicting oldest-first down
/// to this keeps the app's footprint predictable: leaving it open all night
/// must not push the system into swap (and, on a shared-memory GPU, into
/// driver buffer-eviction stalls that stutter the whole desktop).
pub const RING_BUDGET_BYTES: usize = 3 * 1024 * 1024 * 1024;

/// Never trim below this many frames — a loop needs a few scans to be a loop.
pub const MIN_RING_FRAMES: usize = 8;

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
}

/// Which set the alerts browser is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertsTab {
    /// Every active alert nationwide.
    All,
    /// Only alerts whose polygon lies near the selected radar site.
    Station,
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

/// One frame in the animation ring.
pub struct Frame {
    pub volume_start: DateTime<Utc>,
    pub volume: Arc<Volume>,
    /// Approximate resident bytes of `volume` (ring budget accounting).
    pub bytes: usize,
}

pub struct AppState {
    pub mode: Mode,
    /// Selected archive day (Central-date; the ◀/▶ controls in Archive mode).
    pub archive_date: chrono::NaiveDate,
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
    /// Alerts near the selected site (what the map draws).
    pub alerts: Vec<radar_core::warnings::Warning>,
    /// Every active alert nationwide, for the alerts browser.
    pub all_alerts: Vec<radar_core::warnings::Warning>,
    pub show_alerts: bool,
    /// Alerts browser visibility and which tab is showing.
    pub alerts_popup_open: bool,
    pub alerts_tab: AlertsTab,
    /// Search box contents for the alerts browser.
    pub alerts_filter: String,
    /// Alert id under the pointer in the alerts browser: the map glows that
    /// polygon so a list row can be matched to the warning on screen.
    pub alerts_hover: Option<String>,
    /// Alert id under the pointer *on the map*: same glow, so pointing at a
    /// warning shows what it is without opening anything.
    pub alerts_map_hover: Option<String>,
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

    // NWS forecast (station by default, or a point picked on the map).
    pub forecast: Option<radar_core::forecast::Forecast>,
    /// Current conditions from the nearest reporting station (same point as
    /// the forecast above).
    pub observation: Option<radar_core::forecast::Observation>,
    pub forecast_open: bool,
    /// Armed by the window's "select on map" control: the next map click
    /// becomes the forecast point.
    pub forecast_pick_mode: bool,
    /// Manually picked forecast point; `None` = follow the selected station.
    pub forecast_point: Option<(f64, f64)>,
    /// Station id the request/result corresponds to, so the window can tell
    /// when a site switch needs a refetch.
    pub forecast_site: String,
    /// Coordinates already requested (dedupes sends while one is in flight).
    pub forecast_requested: Option<(f64, f64)>,
    /// Derived storm tracks overlay (cell motion arrows + nowcast lines).
    pub show_tracks: bool,

    // SPC convective outlook overlay (Day 1/2/3 categorical risk areas).
    pub outlook: Option<radar_core::outlooks::Outlook>,
    pub show_outlook: bool,
    pub outlook_day: u8,
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

    // User-placed map markers.
    pub markers: Vec<MapMarker>,
    /// Index of a marker whose label popup is open, if any.
    pub pending_marker: Option<usize>,
    /// Draft text for the open label popup. MUST persist across frames —
    /// egui's TextEdit is immediate-mode: cloning the stored label each frame
    /// would erase every keystroke on the next repaint.
    pub pending_marker_label: String,
    /// When true the next map click drops a new marker (the "＋ Place"
    /// toggle; Shift+click works regardless).
    pub add_marker_mode: bool,
    /// Keep the controls sidebar open instead of auto-hiding it.
    /// Unpinned (default) the sidebar slides away and reappears when the
    /// pointer touches the left edge.
    pub sidebar_pinned: bool,

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

    /// Playhead advance accumulator for LoopState::Playing.
    _acc: f32,
}

impl Default for AppState {
    fn default() -> Self {
        let site = radar_core::sites::site(radar_core::DEFAULT_SITE)
            .unwrap_or_else(|| radar_core::sites::SITES.first().expect("site catalog"));
        Self {
            mode: Mode::Live,
            archive_date: chrono::Utc::now()
                .with_timezone(crate::timefmt::central())
                .date_naive(),
            loop_state: LoopState::Paused,
            fps: 8.0,
            site_id: site.id.to_string(),
            moment: Moment::Reflectivity,
            tilt_deg: 0.5,
            opacity: 0.50,
            // Suppresses the clear-air clutter/haze that reads as a green
            // blob: only meaningful echoes light up by default.
            min_dbz: 30.0,
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
            all_alerts: Vec::new(),
            show_alerts: true,
            alerts_popup_open: false,
            alerts_tab: AlertsTab::Station,
            alerts_filter: String::new(),
            alerts_hover: None,
            alerts_map_hover: None,
            outlook_hover: None,
            outlook_hover_at: 0.0,
            alerts_map_hover_at: 0.0,
            alerts_focus: None,
            alerts_focus_at: 0.0,
            alert_detail: None,
            forecast: None,
            observation: None,
            forecast_open: false,
            forecast_pick_mode: false,
            forecast_point: None,
            forecast_site: String::new(),
            forecast_requested: None,
            show_tracks: true,
            outlook: None,
            // On by default with the outline-only look (fill 0.0): the risk
            // boundaries show over the radar without tinting the echoes.
            show_outlook: true,
            outlook_day: 1,
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
            markers: load_markers(),
            pending_marker: None,
            pending_marker_label: String::new(),
            add_marker_mode: false,
            sidebar_pinned: false,
            measure_mode: false,
            measure_a: None,
            measure_b: None,
            measure_drag: None,
            measure_label_offset: None,
            measure_expanded: false,
            measure_bubble_rect: None,
            _acc: 0.0,
        }
    }
}

/// `~/.config/stormscope/markers.json`, where user pins are persisted.
fn markers_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
                .join(".config")
        });
    base.join("stormscope").join("markers.json")
}

fn load_markers() -> Vec<MapMarker> {
    match std::fs::read_to_string(markers_path()) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

impl AppState {
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

    /// Current frame at the playhead, if any volume is loaded.
    pub fn current(&self) -> Option<&Frame> {
        self.ring.get(self.playhead)
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
            return;
        }
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
