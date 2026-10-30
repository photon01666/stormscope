//! Live AI tracking: the deterministic per-scan assessment, the rolling
//! memory of previous volume scans, and the feed/speech state.
//!
//! The AI never calculates the arrival time itself. This module samples the
//! volume and the storm tracks, resolves which location counts as "me", and
//! hands the model a compact, remembered series to narrate.

use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use radar_core::sites::RadarSite;
use radar_core::warnings::Warning;
use radar_core::{Moment, Volume};
use radar_render::StormTrack;

use crate::app::MarkerEta;
use crate::state::AppState;
use crate::ui::alerts::ring_contains;

/// How many live updates the window keeps visible.
pub const MAX_FEED: usize = 40;
/// How many scans the model can remember. Six gives a ~30-minute trend while
/// keeping the live prompt compact.
pub const MAX_MEMORY: usize = 12;
/// How many remembered scans are sent to the model on each request.
pub const MEMORY_IN_PROMPT: usize = 6;
/// Fallback radar coverage when a volume has no readable ranges.
const FALLBACK_COVERAGE_KM: f64 = 250.0;
/// ETA thresholds that change the spoken alert.
const SPEAK_APPROACH_MIN: f64 = 30.0;
const SPEAK_IMMINENT_MIN: f64 = 10.0;
/// Live tracking always derives motion from the lowest cut so successive
/// memories compare like with like, regardless of the product/tilt on screen.
pub const TRACK_TILT_DEG: f32 = 0.5;
/// A quiet period this long resets the speech threshold, so a later storm can
/// trigger the 30-minute announcement again.
const SPEECH_RESET: Duration = Duration::from_secs(15 * 60);
/// A coherent low-tilt return must reach this median to count as precipitation
/// overhead. A single 20 dBZ gate is often clear-air, insect, virga or clutter.
const SURFACE_PRECIP_ENTER_DBZ: f32 = 25.0;
/// Once precipitation is overhead, keep it until the return falls below this.
/// This deadband stops a scan-to-scan 18/20 dBZ wobble from flapping the feed.
const SURFACE_PRECIP_EXIT_DBZ: f32 = 18.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationSource {
    Gps,
    Marker,
    Station,
}

impl LocationSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Gps => "gps",
            Self::Marker => "marker",
            Self::Station => "selected_station",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LiveLocation {
    /// Human label (a marker name, "My location", or the site id).
    pub label: String,
    pub lat: f64,
    pub lon: f64,
    pub source: LocationSource,
    /// Radius of the selected station's data coverage, km.
    pub coverage_km: f64,
}

impl LiveLocation {
    /// Resolve "me" for the selected radar:
    /// 1. GPS inside the station's data coverage;
    /// 2. a marker inside that coverage (prefer one named Home/Me);
    /// 3. the selected radar site itself.
    pub fn resolve(state: &AppState, site: &RadarSite, volume: &Volume) -> Self {
        let coverage_km = coverage_km(volume);
        let inside = |lat: f64, lon: f64| {
            radar_render::surface_distance_m(site.longitude, site.latitude, lon, lat) / 1000.0
                <= coverage_km
        };

        if let Some((lat, lon)) = state.gps_fix.filter(|(lat, lon)| inside(*lat, *lon)) {
            return Self {
                label: "My location".to_string(),
                lat,
                lon,
                source: LocationSource::Gps,
                coverage_km,
            };
        }

        let marker = state
            .markers
            .iter()
            .filter(|m| inside(m.lat, m.lon))
            .min_by_key(|m| {
                let label = m.label.trim().to_ascii_lowercase();
                let named_me = label == "me" || label == "home" || label == "my location";
                // A marker explicitly named Home/Me wins; otherwise the first
                // marker in the list is the fallback.
                if named_me { 0usize } else { 1 }
            });
        if let Some(marker) = marker {
            return Self {
                label: marker.display(),
                lat: marker.lat,
                lon: marker.lon,
                source: LocationSource::Marker,
                coverage_km,
            };
        }

        Self {
            label: site.id.to_string(),
            lat: site.latitude,
            lon: site.longitude,
            source: LocationSource::Station,
            coverage_km,
        }
    }

    /// The label to use in a spoken sentence.
    pub fn spoken_label(&self) -> String {
        match self.source {
            LocationSource::Gps => "your location".to_string(),
            LocationSource::Marker => format!("your {} marker", self.label),
            LocationSource::Station => format!("the {} radar site", self.label),
        }
    }

    /// The label to use in the feed. The station fallback is explicit so the
    /// UI never implies the station is the spotter's location.
    pub fn feed_label(&self) -> String {
        match self.source {
            LocationSource::Gps => "My location".to_string(),
            LocationSource::Marker => self.label.clone(),
            LocationSource::Station => format!("{} (selected station)", self.label),
        }
    }
}

/// The earliest tracked core heading toward a point.
#[derive(Debug, Clone)]
pub struct LiveEta {
    pub minutes: f64,
    pub speed_mph: f64,
    pub heading: &'static str,
    pub bearing_deg: f64,
    pub miss_mi: f64,
    pub peak_dbz: f32,
    pub delta_dbz: f32,
    pub delta_cells: isize,
}

#[derive(Debug, Clone)]
pub struct LiveNearest {
    pub distance_mi: f64,
    pub direction: &'static str,
    pub speed_mph: f64,
    pub heading: &'static str,
    pub peak_dbz: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ThreatLevel {
    Quiet,
    Watch,
    Threat,
}

impl ThreatLevel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Quiet => "Quiet",
            Self::Watch => "Watch",
            Self::Threat => "Threat",
        }
    }

    pub fn rgb(self) -> [u8; 3] {
        match self {
            Self::Quiet => [120, 200, 255],
            Self::Watch => [255, 205, 80],
            Self::Threat => [255, 90, 90],
        }
    }
}

/// One deterministic scan assessment. The UI can show this without waiting
/// for (or trusting) the model.
#[derive(Debug, Clone)]
pub struct LiveAssessment {
    pub location: LiveLocation,
    pub eta: Option<LiveEta>,
    /// Median of a 5x5 low-tilt neighborhood at the primary point. This is a
    /// radar return, not proof of precipitation reaching the ground.
    pub surface_dbz: Option<f32>,
    /// True only when the low-tilt return is coherent and strong enough to
    /// call precipitation overhead, with hysteresis across scans.
    pub surface_precip: bool,
    pub warning: Option<String>,
    pub nearest: Option<LiveNearest>,
    pub threat: ThreatLevel,
    pub headline: String,
    /// The nearest tracked cores, retained so the remembered scan carries
    /// actual data and not only prose.
    pub cells: Vec<LiveCell>,
}

impl LiveAssessment {
    /// The threshold used by the spoken alert. Warnings are intentionally not
    /// included here: the existing warning voice path owns those, so they do
    /// not get announced twice.
    fn speech_bucket(&self) -> Option<SpeechBucket> {
        if self.surface_precip {
            return Some(SpeechBucket::Overhead);
        }
        let minutes = self.eta.as_ref()?.minutes;
        if minutes <= SPEAK_IMMINENT_MIN {
            Some(SpeechBucket::Imminent)
        } else if minutes <= SPEAK_APPROACH_MIN {
            Some(SpeechBucket::Approaching)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SpeechBucket {
    Approaching,
    Imminent,
    Overhead,
}

/// One tracked cell remembered from a scan, so the next prompt can compare
/// actual positions and strengths rather than only prose.
#[derive(Debug, Clone)]
pub struct LiveCell {
    pub lat: f64,
    pub lon: f64,
    pub distance_mi: f64,
    pub peak_dbz: f32,
    pub delta_dbz: f32,
    pub delta_cells: isize,
    pub speed_mph: f64,
    pub heading: &'static str,
}

/// The model's remembered view of one previous volume scan.
#[derive(Debug, Clone)]
pub struct LiveMemory {
    pub scan: DateTime<Utc>,
    pub scan_number: u64,
    pub location: String,
    pub source: LocationSource,
    pub eta_min: Option<f64>,
    pub threat: ThreatLevel,
    pub warning: Option<String>,
    pub cells: Vec<LiveCell>,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct LiveEntry {
    pub scan: DateTime<Utc>,
    pub site: String,
    pub generated_at: DateTime<Utc>,
    pub location: String,
    pub source: LocationSource,
    pub eta_min: Option<f64>,
    pub threat: ThreatLevel,
    pub headline: String,
    pub body: String,
    pub error: bool,
    pub latency_ms: u64,
}

#[derive(Debug, Clone)]
pub struct LiveRequest {
    pub seq: u64,
    pub scan: DateTime<Utc>,
    pub site: String,
    pub started: Instant,
    pub assessment: LiveAssessment,
    /// Active alert ids when this request was built. On success these become
    /// the new seen set; a warning issued while the request is in flight is
    /// deliberately left unseen so the next scan reports it.
    pub alerts_at_start: HashSet<String>,
}

#[derive(Default)]
pub struct LiveAi {
    pub feed: VecDeque<LiveEntry>,
    pub memory: VecDeque<LiveMemory>,
    pub last_scan: Option<DateTime<Utc>>,
    pub last_site: Option<String>,
    pub in_flight: Option<LiveRequest>,
    pub last_assessment: Option<LiveAssessment>,
    pub unread: u32,
    pub failures: u8,
    pub retry_at: Option<Instant>,
    pub seen_alerts: HashSet<String>,
    pub scan_number: u64,
    pub prev_tracks: Vec<StormTrack>,
    /// Hysteresis state for "precipitation overhead" across scans.
    pub overhead: bool,
    seq: u64,
    last_speech: Option<SpeechBucket>,
    quiet_since: Option<Instant>,
}

impl LiveAi {
    /// A site switch invalidates trends, the remembered cells and any request
    /// that was already in flight. The visible feed is kept as history.
    pub fn reset_for_site(&mut self, site: &str) {
        self.last_site = Some(site.to_string());
        self.last_scan = None;
        self.memory.clear();
        self.prev_tracks.clear();
        self.last_assessment = None;
        self.seen_alerts.clear();
        self.in_flight = None;
        self.failures = 0;
        self.retry_at = None;
        self.last_speech = None;
        self.quiet_since = None;
        self.scan_number = 0;
        self.overhead = false;
        self.seq = self.seq.wrapping_add(1);
    }

    pub fn stop(&mut self) {
        self.in_flight = None;
        self.retry_at = None;
        self.last_scan = None;
        self.failures = 0;
        self.overhead = false;
        self.seq = self.seq.wrapping_add(1);
    }

    pub fn clear_feed(&mut self) {
        self.feed.clear();
        self.unread = 0;
    }

    pub fn mark_alerts(&mut self, alerts: &[Warning]) {
        self.seen_alerts = alerts.iter().map(|w| w.id.clone()).collect();
    }

    pub fn new_warning_events(&self, alerts: &[Warning], now: DateTime<Utc>) -> Vec<String> {
        let mut events: Vec<String> = alerts
            .iter()
            .filter(|w| w.expires > now && !self.seen_alerts.contains(&w.id))
            .map(|w| w.event.clone())
            .collect();
        events.sort();
        events.dedup();
        events
    }

    pub fn begin(
        &mut self,
        scan: DateTime<Utc>,
        site: String,
        assessment: LiveAssessment,
        alerts: &[Warning],
    ) -> LiveRequest {
        self.seq = self.seq.wrapping_add(1);
        let request = LiveRequest {
            seq: self.seq,
            scan,
            site: site.clone(),
            started: Instant::now(),
            assessment,
            alerts_at_start: alerts.iter().map(|w| w.id.clone()).collect(),
        };
        self.in_flight = Some(request.clone());
        self.last_scan = Some(scan);
        self.last_site = Some(site);
        request
    }

    pub fn push(&mut self, entry: LiveEntry) {
        self.unread = self.unread.saturating_add(1);
        self.feed.push_back(entry);
        while self.feed.len() > MAX_FEED {
            self.feed.pop_front();
        }
    }

    pub fn remember(&mut self, scan: DateTime<Utc>, assessment: &LiveAssessment, summary: &str) {
        self.scan_number = self.scan_number.saturating_add(1);
        let memory = LiveMemory {
            scan,
            scan_number: self.scan_number,
            location: assessment.location.feed_label(),
            source: assessment.location.source,
            eta_min: assessment.eta.as_ref().map(|e| e.minutes),
            threat: assessment.threat,
            warning: assessment.warning.clone(),
            cells: assessment.cells.clone(),
            summary: summary.chars().take(1200).collect(),
        };
        self.memory.push_back(memory);
        while self.memory.len() > MAX_MEMORY {
            self.memory.pop_front();
        }
    }

    /// Return a spoken alert only when the threat crosses a new threshold.
    pub fn speech_for(&mut self, assessment: &LiveAssessment) -> Option<String> {
        // Warnings already have their own voice path; do not read the same
        // warning twice. Live speech is only for the arrival thresholds.
        if assessment.warning.is_some() {
            return None;
        }
        let bucket = assessment.speech_bucket();
        if bucket.is_none() {
            let quiet_long_enough = self
                .quiet_since
                .is_none_or(|since| since.elapsed() >= SPEECH_RESET);
            if quiet_long_enough {
                self.last_speech = None;
            }
            self.quiet_since.get_or_insert_with(Instant::now);
            return None;
        }
        self.quiet_since = None;
        if self
            .last_speech
            .is_some_and(|previous| bucket <= Some(previous))
        {
            return None;
        }
        self.last_speech = bucket;
        let location = assessment.location.spoken_label();
        if assessment.surface_precip {
            return Some(format!(
                "Live AI: precipitation is overhead at {location}. Check the latest scan."
            ));
        }
        let eta = assessment.eta.as_ref()?;
        let minutes = eta.minutes.round().max(0.0) as i64;
        Some(format!(
            "Live AI: a storm core is projected to reach {location} in {minutes} minutes, \
             moving {} at {} miles per hour.",
            eta.heading,
            eta.speed_mph.round().max(0.0) as i64
        ))
    }

    /// The most recent successful assessment, if any.
    pub fn latest_assessment(&self) -> Option<&LiveAssessment> {
        self.last_assessment.as_ref()
    }

    /// A short status line for the Live tab.
    pub fn status_text(&self, configured: bool, enabled: bool) -> String {
        if !enabled {
            return "Off".to_string();
        }
        if !configured {
            return "Configure the AI endpoint to start".to_string();
        }
        if let Some(request) = &self.in_flight {
            return format!("Analyzing {} scan…", scan_clock(&request.scan));
        }
        if let Some(at) = self.retry_at {
            let wait = at.saturating_duration_since(Instant::now());
            if !wait.is_zero() {
                return format!("Retrying in {}s", wait.as_secs().max(1));
            }
        }
        if let Some(scan) = self.last_scan {
            return format!("Last scan {} · watching for the next", scan_clock(&scan));
        }
        "Waiting for a volume scan".to_string()
    }

    pub fn in_flight_seq(&self) -> Option<u64> {
        self.in_flight.as_ref().map(|r| r.seq)
    }
}

/// One scan clock label in the local timezone ("2:35 PM").
pub fn scan_clock(scan: &DateTime<Utc>) -> String {
    scan.with_timezone(crate::timefmt::central())
        .format("%-I:%M %p")
        .to_string()
}

/// Data coverage of the loaded volume, in km. The selected radar's actual
/// gate range is the honest boundary for "inside this station's view".
pub fn coverage_km(volume: &Volume) -> f64 {
    let range = volume
        .sweeps
        .iter()
        .flat_map(|sweep| sweep.moments.iter())
        .map(|m| (m.range_start_m + m.gate_count as f32 * m.gate_size_m) / 1000.0)
        .fold(0.0f32, f32::max) as f64;
    if range.is_finite() && range > 1.0 {
        range
    } else {
        FALLBACK_COVERAGE_KM
    }
}

/// Compute the earliest arrival for one point from the current tracks.
pub fn eta_at(tracks: &[StormTrack], lat: f64, lon: f64) -> Option<LiveEta> {
    tracks
        .iter()
        .filter_map(|track| radar_render::eta_to_point(track, lon, lat).map(|eta| (eta, track)))
        .min_by(|a, b| {
            a.0.minutes
                .partial_cmp(&b.0.minutes)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(eta, track)| LiveEta {
            minutes: eta.minutes,
            speed_mph: track.speed_kmh * 0.621_371,
            heading: compass16(track.bearing_deg),
            bearing_deg: track.bearing_deg,
            miss_mi: eta.miss_km * 0.621_371,
            peak_dbz: track.max_dbz,
            delta_dbz: track.delta_dbz(),
            delta_cells: track.delta_cells(),
        })
}

/// Arrival estimates aligned with `markers` (the same ordering the map uses).
pub fn marker_etas(
    tracks: &[StormTrack],
    markers: &[crate::state::MapMarker],
) -> Vec<Option<MarkerEta>> {
    markers
        .iter()
        .map(|marker| {
            eta_at(tracks, marker.lat, marker.lon).map(|eta| MarkerEta {
                minutes: eta.minutes,
                speed_mph: eta.speed_mph,
                compass: eta.heading,
            })
        })
        .collect()
}

/// Build the deterministic assessment for the primary location.
pub fn assess(
    volume: &Volume,
    site: &RadarSite,
    tracks: &[StormTrack],
    alerts: &[Warning],
    location: &LiveLocation,
    now: DateTime<Utc>,
    previous_overhead: bool,
) -> LiveAssessment {
    let eta = eta_at(tracks, location.lat, location.lon);
    let surface_dbz = surface_dbz(volume, site, location.lat, location.lon);
    let surface_precip = overhead_now(surface_dbz, previous_overhead);
    let warning = alerts
        .iter()
        .filter(|w| w.expires > now)
        .find(|w| {
            w.polygon.as_ref().is_some_and(|rings| {
                rings
                    .iter()
                    .any(|ring| ring_contains(ring, location.lon, location.lat))
            })
        })
        .map(|w| w.event.clone());
    let nearest = tracks
        .iter()
        .map(|track| {
            let distance_mi =
                radar_render::surface_distance_m(location.lon, location.lat, track.lon, track.lat)
                    / 1609.344;
            (distance_mi, track)
        })
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(distance_mi, track)| LiveNearest {
            distance_mi,
            direction: compass16(radar_render::initial_bearing_deg(
                location.lon,
                location.lat,
                track.lon,
                track.lat,
            )),
            speed_mph: track.speed_kmh * 0.621_371,
            heading: compass16(track.bearing_deg),
            peak_dbz: track.max_dbz,
        });

    let threat = if warning.is_some()
        || eta
            .as_ref()
            .is_some_and(|e| e.minutes <= SPEAK_APPROACH_MIN)
    {
        ThreatLevel::Threat
    } else if eta.as_ref().is_some_and(|e| e.minutes <= 60.0)
        || surface_precip
        || nearest.as_ref().is_some_and(|n| n.distance_mi <= 10.0)
    {
        ThreatLevel::Watch
    } else {
        ThreatLevel::Quiet
    };

    let headline = if let Some(event) = &warning {
        format!("Warning in effect at {}: {event}", location.feed_label())
    } else if let Some(eta) = &eta {
        format!(
            "Core arrival ~{} · {} at {:.0} mph",
            fmt_minutes(eta.minutes),
            eta.heading,
            eta.speed_mph
        )
    } else if surface_precip {
        format!("Precipitation overhead at {}", location.feed_label())
    } else if let Some(nearest) = &nearest {
        format!(
            "No tracked core heading to {} · nearest cell {:.0} mi {}",
            location.feed_label(),
            nearest.distance_mi,
            nearest.direction
        )
    } else {
        format!("No tracked storm cores near {}", location.feed_label())
    };

    LiveAssessment {
        location: location.clone(),
        eta,
        surface_dbz,
        surface_precip,
        warning,
        nearest,
        threat,
        headline,
        cells: memory_cells(tracks, location.lat, location.lon),
    }
}

/// Cells nearest the primary location, retained on each scan so the model can
/// compare real positions and strengths rather than only its own prose.
pub fn memory_cells(tracks: &[StormTrack], lat: f64, lon: f64) -> Vec<LiveCell> {
    let mut cells: Vec<(f64, LiveCell)> = tracks
        .iter()
        .map(|track| {
            let distance_mi =
                radar_render::surface_distance_m(lon, lat, track.lon, track.lat) / 1609.344;
            (
                distance_mi,
                LiveCell {
                    lat: track.lat,
                    lon: track.lon,
                    distance_mi,
                    peak_dbz: track.max_dbz,
                    delta_dbz: track.delta_dbz(),
                    delta_cells: track.delta_cells(),
                    speed_mph: track.speed_kmh * 0.621_371,
                    heading: compass16(track.bearing_deg),
                },
            )
        })
        .collect();
    cells.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    cells.into_iter().take(6).map(|(_, cell)| cell).collect()
}

/// Blend fresh cell velocities 50/50 with the previous set, matched by
/// proximity. Live ETA is a trend, and raw scan-to-scan centroids jitter
/// enough to swing arrival times by several minutes. This is a private copy
/// of the map's smoother so live analysis never disturbs the map's own track
/// cache.
pub fn smooth_tracks(prev: &[StormTrack], fresh: Vec<StormTrack>) -> Vec<StormTrack> {
    const MATCH_KM: f64 = 25.0;
    let mut used = vec![false; prev.len()];
    fresh
        .into_iter()
        .map(|mut track| {
            let mut best: Option<(usize, f64)> = None;
            for (i, previous) in prev.iter().enumerate() {
                if used[i] {
                    continue;
                }
                let distance = radar_render::surface_distance_m(
                    track.lon,
                    track.lat,
                    previous.lon,
                    previous.lat,
                ) / 1000.0;
                if distance <= MATCH_KM
                    && best.map_or(true, |(_, best_distance)| distance < best_distance)
                {
                    best = Some((i, distance));
                }
            }
            if let Some((i, _)) = best {
                used[i] = true;
                let previous = &prev[i];
                track.vel_e = 0.5 * track.vel_e + 0.5 * previous.vel_e;
                track.vel_n = 0.5 * track.vel_n + 0.5 * previous.vel_n;
                track.speed_kmh = (track.vel_e.hypot(track.vel_n) * 60.0).round();
                track.bearing_deg = track
                    .vel_e
                    .atan2(track.vel_n)
                    .to_degrees()
                    .rem_euclid(360.0);
            }
            track
        })
        .collect()
}

/// Apply the precipitation-overhead deadband. Start at 25 dBZ, but once the
/// flag is on keep it until the coherent return drops below 18 dBZ.
fn overhead_now(dbz: Option<f32>, previous: bool) -> bool {
    match dbz {
        Some(dbz) if previous => dbz >= SURFACE_PRECIP_EXIT_DBZ,
        Some(dbz) => dbz >= SURFACE_PRECIP_ENTER_DBZ,
        None => false,
    }
}

/// Median reflectivity of a 5x5 gate neighborhood on the **lowest** elevation
/// sweep that carries reflectivity.
///
/// The old implementation returned the first finite gate from any sweep. That
/// let a single 20 dBZ speckle (or a higher-tilt echo aloft) become
/// "precipitation overhead". Requiring a coherent low-tilt neighborhood and
/// ignoring higher sweeps removes that false positive.
fn surface_dbz(volume: &Volume, site: &RadarSite, lat: f64, lon: f64) -> Option<f32> {
    let range_km =
        radar_render::surface_distance_m(site.longitude, site.latitude, lon, lat) as f32 / 1000.0;
    let bearing = radar_render::initial_bearing_deg(site.longitude, site.latitude, lon, lat) as f32;

    let sweep = volume
        .sweeps
        .iter()
        .filter(|sweep| sweep.moment(Moment::Reflectivity).is_some())
        .min_by(|a, b| {
            let ea = if a.elevation_measured_deg.is_finite() && a.elevation_measured_deg != 0.0 {
                a.elevation_measured_deg
            } else {
                a.elevation_deg
            };
            let eb = if b.elevation_measured_deg.is_finite() && b.elevation_measured_deg != 0.0 {
                b.elevation_measured_deg
            } else {
                b.elevation_deg
            };
            ea.partial_cmp(&eb).unwrap_or(std::cmp::Ordering::Equal)
        })?;
    let md = sweep.moment(Moment::Reflectivity)?;
    let elevation =
        if sweep.elevation_measured_deg.is_finite() && sweep.elevation_measured_deg != 0.0 {
            sweep.elevation_measured_deg
        } else {
            sweep.elevation_deg
        };
    let height = radar_core::sampling::beam_height_km(range_km, elevation);
    let slant = range_km.hypot(height);
    let ray = radar_core::sampling::nearest_ray_index(&sweep.azimuths_deg, bearing)?;
    let gate = radar_core::sampling::gate_index_at_slant(md, slant)?;

    let mut values = Vec::with_capacity(25);
    for dr in -2i32..=2 {
        let r = (ray as i32 + dr).rem_euclid(md.ray_count as i32) as usize;
        for dg in -2i32..=2 {
            let g = gate as i32 + dg;
            if g < 0 || g as usize >= md.gate_count {
                continue;
            }
            if let Some(value) = md.value(r, g as usize).filter(|v| v.is_finite()) {
                values.push(value);
            }
        }
    }
    if values.len() < 9 {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // A coherent 40+ dBZ core is precipitation even if it is small relative
    // to a 5x5 window; do not let the median hide a real downpour.
    if let Some(max) = values.last().copied()
        && max >= 40.0
    {
        return Some(max);
    }
    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        Some((values[mid - 1] + values[mid]) * 0.5)
    } else {
        Some(values[mid])
    }
}

fn fmt_minutes(minutes: f64) -> String {
    let minutes = minutes.round().max(0.0) as i64;
    if minutes < 90 {
        format!("{minutes} min")
    } else {
        format!("{}h {:02}m", minutes / 60, minutes % 60)
    }
}

fn compass16(deg: f64) -> &'static str {
    const PTS: [&str; 16] = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW",
        "NW", "NNW",
    ];
    PTS[((deg.rem_euclid(360.0) / 22.5).round() as usize) % 16]
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use radar_core::{MomentCalibration, MomentData, Sweep, VolumeMeta};

    fn site() -> &'static RadarSite {
        radar_core::sites::site("KTWX").expect("KTWX")
    }

    fn volume_with_range(km: f32) -> Volume {
        let sweep = Sweep {
            elevation_deg: 0.5,
            elevation_measured_deg: 0.5,
            azimuth_width_deg: 1.0,
            azimuths_deg: vec![0.0, 90.0, 180.0, 270.0],
            moments: vec![MomentData {
                kind: Moment::Reflectivity,
                calibration: MomentCalibration::default(),
                range_start_m: 0.0,
                gate_size_m: 1000.0,
                gate_count: km as usize,
                ray_count: 4,
                data: vec![0; 4 * km as usize],
            }],
        };
        Volume {
            meta: VolumeMeta {
                site_id: "KTWX".into(),
                volume_start: Utc.with_ymd_and_hms(2026, 10, 28, 12, 0, 0).unwrap(),
                sweep_count: 1,
                moments: vec![Moment::Reflectivity],
                vcp: Some(12),
                sequence_number: None,
            },
            sweeps: vec![sweep],
        }
    }

    #[test]
    fn station_is_the_fallback_when_no_marker_is_in_coverage() {
        let volume = volume_with_range(100.0);
        let mut state = AppState::default();
        state.markers = vec![crate::state::MapMarker {
            lat: site().latitude + 2.0,
            lon: site().longitude + 2.0,
            label: "Far away".into(),
        }];
        let location = LiveLocation::resolve(&state, site(), &volume);
        assert_eq!(location.source, LocationSource::Station);
        assert_eq!(location.label, "KTWX");
    }

    #[test]
    fn home_marker_beats_other_in_coverage_markers() {
        let volume = volume_with_range(500.0);
        let mut state = AppState::default();
        state.markers = vec![
            crate::state::MapMarker {
                lat: site().latitude + 0.1,
                lon: site().longitude + 0.1,
                label: "Other".into(),
            },
            crate::state::MapMarker {
                lat: site().latitude + 0.2,
                lon: site().longitude + 0.2,
                label: "Home".into(),
            },
        ];
        let location = LiveLocation::resolve(&state, site(), &volume);
        assert_eq!(location.source, LocationSource::Marker);
        assert_eq!(location.label, "Home");
    }

    #[test]
    fn speech_only_crosses_each_threshold_once() {
        let mut live = LiveAi::default();
        let location = LiveLocation {
            label: "Home".into(),
            lat: 35.0,
            lon: -97.0,
            source: LocationSource::Marker,
            coverage_km: 400.0,
        };
        let mut assessment = LiveAssessment {
            location,
            eta: Some(LiveEta {
                minutes: 45.0,
                speed_mph: 30.0,
                heading: "NE",
                bearing_deg: 45.0,
                miss_mi: 1.0,
                peak_dbz: 50.0,
                delta_dbz: 0.0,
                delta_cells: 0,
            }),
            surface_dbz: None,
            surface_precip: false,
            warning: None,
            nearest: None,
            threat: ThreatLevel::Watch,
            headline: String::new(),
            cells: Vec::new(),
        };
        assert!(live.speech_for(&assessment).is_none());
        assessment.eta.as_mut().unwrap().minutes = 28.0;
        assert!(live.speech_for(&assessment).is_some());
        assert!(live.speech_for(&assessment).is_none());
        assessment.eta.as_mut().unwrap().minutes = 8.0;
        assert!(live.speech_for(&assessment).is_some());
    }

    #[test]
    fn surface_dbz_ignores_a_higher_tilt_echo_when_the_low_tilt_is_empty() {
        let mut volume = volume_with_range(100.0);
        let mut high = volume.sweeps[0].clone();
        high.elevation_deg = 1.5;
        high.elevation_measured_deg = 1.5;
        high.moments[0].data.fill(62); // 60 dBZ
        volume.sweeps.push(high);
        let lat = site().latitude;
        let lon = site().longitude + 0.5;
        assert_eq!(surface_dbz(&volume, site(), lat, lon), None);
    }

    #[test]
    fn surface_dbz_uses_the_low_tilt_median_not_a_higher_sweep() {
        let mut volume = volume_with_range(100.0);
        volume.sweeps[0].moments[0].data.fill(32); // 30 dBZ
        let mut high = volume.sweeps[0].clone();
        high.elevation_deg = 1.5;
        high.elevation_measured_deg = 1.5;
        high.moments[0].data.fill(62); // 60 dBZ
        volume.sweeps.push(high);
        let lat = site().latitude;
        let lon = site().longitude + 0.5;
        assert_eq!(surface_dbz(&volume, site(), lat, lon), Some(30.0));
    }

    #[test]
    fn overhead_uses_hysteresis_and_rejects_single_gate_returns() {
        assert!(!overhead_now(Some(24.9), false));
        assert!(overhead_now(Some(25.0), false));
        assert!(overhead_now(Some(20.0), true));
        assert!(overhead_now(Some(18.0), true));
        assert!(!overhead_now(Some(17.9), true));
        assert!(!overhead_now(None, true));
    }

    #[test]
    fn station_fallback_is_not_spoken_as_your_location() {
        let location = LiveLocation {
            label: "KTWX".into(),
            lat: 35.0,
            lon: -97.0,
            source: LocationSource::Station,
            coverage_km: 400.0,
        };
        assert_eq!(location.spoken_label(), "the KTWX radar site");
        assert_eq!(location.feed_label(), "KTWX (selected station)");
    }

    #[test]
    fn live_snapshot_names_the_station_fallback_and_carries_the_scan_series() {
        let volume = volume_with_range(100.0);
        let site = site();
        let mut state = AppState::default();
        state.markers.clear();
        state.gps_fix = None;
        let primary = LiveLocation::resolve(&state, site, &volume);
        assert_eq!(primary.source, LocationSource::Station);
        let assessment = assess(
            &volume,
            site,
            &[],
            &[],
            &primary,
            Utc.with_ymd_and_hms(2026, 10, 28, 12, 0, 0).unwrap(),
            false,
        );
        let memory = LiveMemory {
            scan: Utc.with_ymd_and_hms(2026, 10, 28, 11, 55, 0).unwrap(),
            scan_number: 1,
            location: "KTWX (selected station)".into(),
            source: LocationSource::Station,
            eta_min: Some(18.0),
            threat: ThreatLevel::Watch,
            warning: None,
            cells: Vec::new(),
            summary: "Earlier: a cell was 20 miles west.".into(),
        };
        let brief = crate::brief::build_live(&crate::brief::LiveBriefInputs {
            site,
            volume: &volume,
            min_dbz: 20.0,
            markers: &[],
            marker_etas: &[],
            tracks: &[],
            alerts: &[],
            outlook: None,
            forecast: None,
            observation: None,
            gps_fix: None,
            primary: &primary,
            assessment: &assessment,
            memory: std::slice::from_ref(&memory),
            new_warnings: &[],
            scan_number: 2,
            scan_interval_min: Some(5),
            now: Utc.with_ymd_and_hms(2026, 10, 28, 12, 0, 0).unwrap(),
        });
        assert!(brief.system.contains("ONE NEW FULL LEVEL II VOLUME SCAN"));
        assert!(!brief.system.contains("KTWX"));
        assert!(brief.user.contains("scan_series"));
        assert!(brief.user.contains("selected_station"));
        assert!(brief.user.contains("KTWX (selected station)"));
        assert!(brief.user.contains("history"));
        assert!(brief.user.contains("Earlier: a cell was 20 miles west."));
        assert!(brief.user.contains("lead with what changed"));
    }

    #[test]
    fn remembers_the_scan_series_and_caps_the_window() {
        let mut live = LiveAi::default();
        let location = LiveLocation {
            label: "Home".into(),
            lat: 35.0,
            lon: -97.0,
            source: LocationSource::Marker,
            coverage_km: 400.0,
        };
        let assessment = LiveAssessment {
            location,
            eta: None,
            surface_dbz: None,
            surface_precip: false,
            warning: None,
            nearest: None,
            threat: ThreatLevel::Quiet,
            headline: "No tracked cores".into(),
            cells: Vec::new(),
        };
        let base = Utc.with_ymd_and_hms(2026, 10, 28, 12, 0, 0).unwrap();
        for i in 0..15 {
            let scan = base + chrono::Duration::minutes(i * 5);
            live.remember(scan, &assessment, &format!("scan {i}"));
        }
        assert_eq!(live.scan_number, 15);
        assert_eq!(live.memory.len(), MAX_MEMORY);
        assert_eq!(
            live.memory.back().map(|m| m.summary.as_str()),
            Some("scan 14")
        );
        assert_eq!(live.memory.back().map(|m| m.scan_number), Some(15));
    }
}
