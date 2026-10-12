//! Application state: which site/product/tilt are selected, the animation
//! ring buffer of decoded volumes, live-vs-archive mode, and the current
//! map view + playhead.

use chrono::{DateTime, Utc};
use radar_core::{Moment, Volume};
use radar_render::MapView;
use std::collections::VecDeque;
use std::sync::Arc;

/// How many decoded volumes we keep resident for looping. 24 scans ≈ 2 hours
/// of wall-clock radar time. The user's machine has 62 GB RAM and a decoded
/// volume is ~150 MB, so this lands around 3-4 GB — comfortable.
pub const RING_CAPACITY: usize = 24;

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

/// One frame in the animation ring.
pub struct Frame {
    pub volume_start: DateTime<Utc>,
    pub volume: Arc<Volume>,
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

    // Alerts
    pub alerts: Vec<radar_core::warnings::Warning>,
    pub show_alerts: bool,
    /// Derived storm tracks overlay (cell motion arrows + nowcast lines).
    pub show_tracks: bool,

    // Status surfaced in the status bar (filled by the coordinator).
    pub status_text: String,
    /// Archive window position (from `DataMsg::ArchiveInfo`): `total` scans in
    /// the day, `[archive_start, archive_start+archive_len)` loaded.
    pub archive_total: usize,
    pub archive_start: usize,
    pub archive_len: usize,

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
            opacity: 0.85,
            min_dbz: 5.0,
            view: MapView {
                center_lat: site.latitude,
                center_lon: site.longitude,
                zoom: 7.5,
                width_px: 1280.0,
                height_px: 800.0,
            },
            ring: VecDeque::with_capacity(RING_CAPACITY),
            playhead: 0,
            alerts: Vec::new(),
            show_alerts: true,
            show_tracks: true,
            status_text: "starting…".to_string(),
            archive_total: 0,
            archive_start: 0,
            archive_len: 0,
            markers: load_markers(),
            pending_marker: None,
            pending_marker_label: String::new(),
            add_marker_mode: false,
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
        let median = if dts.is_empty() { None } else { Some(dts[dts.len() / 2]) };
        radar_core::vcp::ScanMode::detect(vcp, median)
    }

    /// Push a newly decoded volume onto the ring, keeping it ordered oldest
    /// -> newest and evicting the oldest when full. In Live mode the playhead
    /// jumps to the newest frame; otherwise it stays put.
    pub fn push_volume(&mut self, volume: Arc<Volume>) {
        let start = volume.meta.volume_start;
        // Ignore out-of-order / duplicate volumes.
        if let Some(last) = self.ring.back() {
            if start <= last.volume_start {
                return;
            }
        }
        self.ring.push_back(Frame { volume_start: start, volume });
        while self.ring.len() > RING_CAPACITY {
            self.ring.pop_front();
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
