//! Application state: which site/product/tilt are selected, the animation
//! ring buffer of decoded volumes, live-vs-archive mode, and the current
//! map view + playhead.

use chrono::{DateTime, Utc};
use radar_core::{Moment, Volume};
use radar_render::MapView;
use std::collections::VecDeque;
use std::sync::Arc;

/// How many decoded volumes we keep resident for looping. Radars produce a
/// volume every ~4-6 min, so 24 frames ~= the last ~2 hours.
/// How many decoded volumes we keep resident for looping. 24 scans ≈ 2 hours
/// of wall-clock radar time. The user's machine has 62 GB RAM and a decoded
/// volume is ~150 MB, so this lands around 3-4 GB — comfortable.
pub const RING_CAPACITY: usize = 24;

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

    // Status surfaced in the status bar (filled by the coordinator).
    pub status_text: String,
    /// Archive window position (from `DataMsg::ArchiveInfo`): `total` scans in
    /// the day, `[archive_start, archive_start+archive_len)` loaded.
    pub archive_total: usize,
    pub archive_start: usize,
    pub archive_len: usize,

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
            status_text: "starting…".to_string(),
            archive_total: 0,
            archive_start: 0,
            archive_len: 0,
            _acc: 0.0,
        }
    }
}

impl AppState {
    /// Current frame at the playhead, if any volume is loaded.
    pub fn current(&self) -> Option<&Frame> {
        self.ring.get(self.playhead)
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
