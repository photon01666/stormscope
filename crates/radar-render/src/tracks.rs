//! Derived storm tracks: estimate cell motion by matching reflectivity blobs
//! between two consecutive volume scans (a light TREC-style tracker).
//!
//! Pipeline (pure functions, unit-tested):
//! 1. Subsample each frame's lowest REF sweep onto a coarse Cartesian km-grid
//!    centered on the radar (cell max in dBZ).
//! 2. Label connected blobs (>= [`TRACK_DBZ`], >= [`MIN_CELLS`] cells).
//! 3. Match each blob in the current frame to the nearest blob in the
//!    previous frame within the physically plausible displacement for the
//!    scan-to-scan interval.
//! 4. Displacement / dt becomes the storm's velocity vector.
//!
//! Tracks are view-independent (lat/lon + velocity); the map layer draws
//! arrows and a nowcast extrapolation.

use crate::projection::gate_latlon;
use radar_core::{Moment, Sweep, Volume};

/// Cells at or above this reflectivity participate in tracking (storm cores).
const TRACK_DBZ: f32 = 40.0;
/// Grid cell size, km. 4 km balances cell structure vs. noise.
const CELL_KM: f64 = 4.0;
/// Half-extent of the tracking grid around the radar, km (~WSR-88D range).
const EXTENT_KM: f64 = 230.0;
/// Minimum blob size in grid cells (4 cells = 64 km² of >=40 dBZ core).
const MIN_CELLS: usize = 4;
/// Maximum plausible scan-to-scan displacement factor: 3 km/min = 180 km/h.
const MAX_KM_PER_MIN: f64 = 3.0;
/// Do not emit tracks slower than this (avoids jitter on parked cells).
const MIN_SPEED_KMH: f64 = 10.0;
/// A cell counts as "reaching" a point when its closest approach passes
/// within this distance (~a storm core's width).
pub const ETA_RADIUS_KM: f64 = 12.0;
/// No ETA beyond this horizon — steering currents change, and the number
/// stops meaning anything.
pub const ETA_MAX_MIN: f64 = 120.0;

/// One tracked storm cell: centroid plus motion.
#[derive(Debug, Clone, PartialEq)]
pub struct StormTrack {
    /// Cell centroid now (WGS84).
    pub lat: f64,
    pub lon: f64,
    /// Velocity, km/min east / north.
    pub vel_e: f64,
    pub vel_n: f64,
    /// Ground speed, km/h.
    pub speed_kmh: f64,
    /// Direction of motion in degrees (0 = north, clockwise).
    pub bearing_deg: f64,
    /// Peak reflectivity in the cell, dBZ.
    pub max_dbz: f32,
    /// Peak reflectivity of the same cell in the previous scan, dBZ.
    pub prev_max_dbz: f32,
    /// Number of 4 km grid cells in the current core.
    pub cells: usize,
    /// Number of 4 km grid cells in the previous core.
    pub prev_cells: usize,
}

impl StormTrack {
    /// Change in peak reflectivity since the previous scan, dBZ.
    pub fn delta_dbz(&self) -> f32 {
        self.max_dbz - self.prev_max_dbz
    }

    /// Change in core area since the previous scan, grid cells.
    pub fn delta_cells(&self) -> isize {
        self.cells as isize - self.prev_cells as isize
    }

    /// A compact trend label for the map (`↑ 6 dBZ`, `↓ 3 dBZ`, `steady`).
    pub fn trend_label(&self) -> String {
        let d = self.delta_dbz();
        if d >= 4.0 {
            format!("↑ {d:.0} dBZ")
        } else if d <= -4.0 {
            format!("↓ {:.0} dBZ", d.abs())
        } else {
            "steady".to_string()
        }
    }
}

/// Grid dimensions in cells for one axis.
fn grid_n() -> usize {
    ((2.0 * EXTENT_KM) / CELL_KM).ceil() as usize
}

/// Build a max-dBZ grid for one frame. Returns `None` when the sweep or the
/// REF moment is missing.
fn build_grid(sweep: &Sweep, radar_lat: f64, radar_lon: f64) -> Option<Vec<f32>> {
    let md = sweep.moment(Moment::Reflectivity)?;
    if md.ray_count == 0 || md.gate_count == 0 {
        return None;
    }
    let n = grid_n();
    let mut grid = vec![f32::MIN; n * n];
    let elev = if sweep.elevation_measured_deg.is_finite() && sweep.elevation_measured_deg != 0.0 {
        sweep.elevation_measured_deg
    } else {
        sweep.elevation_deg
    };
    let m_per_deg_lat = 110.574;
    let m_per_deg_lon = 111.320 * radar_lat.to_radians().cos();

    // Subsample: every 3rd ray x every 5th gate is plenty for a 4 km grid.
    for ray in (0..md.ray_count).step_by(3) {
        let Some(&az) = sweep.azimuths_deg.get(ray) else {
            continue;
        };
        if !az.is_finite() {
            continue;
        }
        for gate in (0..md.gate_count).step_by(5) {
            let Some(v) = md.value(ray, gate) else {
                continue;
            };
            if v < TRACK_DBZ {
                continue;
            }
            let slant = md.range_start_m + (gate as f32 + 0.5) * md.gate_size_m;
            if slant > (EXTENT_KM * 1000.0) as f32 {
                continue;
            }
            let (lat, lon) = gate_latlon(radar_lat, radar_lon, az, elev, slant);
            let e_km = (lon - radar_lon) * m_per_deg_lon;
            let n_km = (lat - radar_lat) * m_per_deg_lat;
            let cx = ((e_km + EXTENT_KM) / CELL_KM) as i64;
            let cy = ((n_km + EXTENT_KM) / CELL_KM) as i64;
            if cx < 0 || cy < 0 || cx >= n as i64 || cy >= n as i64 {
                continue;
            }
            let idx = cy as usize * n + cx as usize;
            if v > grid[idx] {
                grid[idx] = v;
            }
        }
    }
    Some(grid)
}

struct Blob {
    /// Mean east/north position, km relative to radar.
    e: f64,
    n: f64,
    max_dbz: f32,
    /// Connected cells in the 4 km tracking grid.
    cells: usize,
}

/// Connected-component labeling (4-connectivity) of cells >= TRACK_DBZ.
fn find_blobs(grid: &[f32], n: usize) -> Vec<Blob> {
    let mut visited = vec![false; n * n];
    let mut blobs = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for start in 0..n * n {
        if visited[start] || grid[start] < TRACK_DBZ {
            continue;
        }
        visited[start] = true;
        stack.clear();
        stack.push(start);
        let (mut sum_e, mut sum_n, mut cnt) = (0.0f64, 0.0f64, 0usize);
        let mut max_dbz = f32::MIN;
        while let Some(idx) = stack.pop() {
            let (cx, cy) = (idx % n, idx / n);
            // Cell center, km relative to radar.
            let e = (cx as f64 + 0.5) * CELL_KM - EXTENT_KM;
            let nn = (cy as f64 + 0.5) * CELL_KM - EXTENT_KM;
            sum_e += e;
            sum_n += nn;
            cnt += 1;
            max_dbz = max_dbz.max(grid[idx]);
            for (dx, dy) in [(-1isize, 0), (1, 0), (0, -1), (0, 1)] {
                let nx = cx as isize + dx;
                let ny = cy as isize + dy;
                if nx < 0 || ny < 0 || nx >= n as isize || ny >= n as isize {
                    continue;
                }
                let ni = ny as usize * n + nx as usize;
                if !visited[ni] && grid[ni] >= TRACK_DBZ {
                    visited[ni] = true;
                    stack.push(ni);
                }
            }
        }
        if cnt >= MIN_CELLS {
            blobs.push(Blob {
                e: sum_e / cnt as f64,
                n: sum_n / cnt as f64,
                max_dbz,
                cells: cnt,
            });
        }
    }
    blobs
}

/// Compute storm tracks between the previous and current volume scans of one
/// site at one tilt. Returns an empty vec when the pair is unusable
/// (missing data, wrong tilt, dt outside 1..45 min).
pub fn compute_tracks(
    prev: &Volume,
    cur: &Volume,
    tilt_deg: f32,
    radar_lat: f64,
    radar_lon: f64,
) -> Vec<StormTrack> {
    let dt = (cur.meta.volume_start - prev.meta.volume_start).num_seconds() as f64 / 60.0;
    if !(1.0..=45.0).contains(&dt) {
        return Vec::new();
    }
    let (Some(sp), Some(sc)) = (
        prev.sweep_index_for_tilt_moment(tilt_deg, Moment::Reflectivity),
        cur.sweep_index_for_tilt_moment(tilt_deg, Moment::Reflectivity),
    ) else {
        return Vec::new();
    };
    let (Some(g_prev), Some(g_cur)) = (
        build_grid(&prev.sweeps[sp], radar_lat, radar_lon),
        build_grid(&cur.sweeps[sc], radar_lat, radar_lon),
    ) else {
        return Vec::new();
    };
    let n = grid_n();
    let prev_blobs = find_blobs(&g_prev, n);
    let cur_blobs = find_blobs(&g_cur, n);
    if prev_blobs.is_empty() {
        return Vec::new();
    }

    let m_per_deg_lat = 110.574;
    let m_per_deg_lon = 111.320 * radar_lat.to_radians().cos();
    let max_disp = MAX_KM_PER_MIN * dt;
    let mut used = vec![false; prev_blobs.len()];
    let mut out = Vec::new();

    for cb in &cur_blobs {
        // Nearest unused previous blob within the plausible displacement.
        let mut best: Option<(usize, f64)> = None;
        for (i, pb) in prev_blobs.iter().enumerate() {
            if used[i] {
                continue;
            }
            let d = ((cb.e - pb.e).powi(2) + (cb.n - pb.n).powi(2)).sqrt();
            if d <= max_disp && best.map_or(true, |(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        let Some((i, _)) = best else { continue };
        used[i] = true;
        let pb = &prev_blobs[i];
        let vel_e = (cb.e - pb.e) / dt; // km/min
        let vel_n = (cb.n - pb.n) / dt;
        let kmh = (vel_e.hypot(vel_n) * 60.0).round();
        if kmh < MIN_SPEED_KMH || kmh > 190.0 {
            continue;
        }
        let bearing = {
            let deg = vel_e.atan2(vel_n).to_degrees(); // atan2(east, north)
            deg.rem_euclid(360.0)
        };
        out.push(StormTrack {
            lat: radar_lat + cb.n / m_per_deg_lat,
            lon: radar_lon + cb.e / m_per_deg_lon,
            vel_e,
            vel_n,
            speed_kmh: kmh,
            bearing_deg: bearing,
            max_dbz: cb.max_dbz,
            prev_max_dbz: pb.max_dbz,
            cells: cb.cells,
            prev_cells: pb.cells,
        });
    }
    out
}

/// One time step of a tracked cell's history.
#[derive(Clone, Debug, PartialEq)]
pub struct StormHistoryPoint {
    pub time: chrono::DateTime<chrono::Utc>,
    pub lat: f64,
    pub lon: f64,
    pub max_dbz: f32,
    /// Core area in km² (4 km tracking cells).
    pub area_km2: f64,
    pub speed_kmh: f64,
    pub bearing_deg: f64,
}

/// A cell's points across the loaded animation ring.
#[derive(Clone, Debug, PartialEq)]
pub struct StormHistory {
    pub points: Vec<StormHistoryPoint>,
}

impl StormHistory {
    pub fn latest(&self) -> Option<&StormHistoryPoint> {
        self.points.last()
    }

    pub fn delta_dbz(&self) -> f32 {
        match (self.points.first(), self.points.last()) {
            (Some(a), Some(b)) => b.max_dbz - a.max_dbz,
            _ => 0.0,
        }
    }

    pub fn delta_area_km2(&self) -> f64 {
        match (self.points.first(), self.points.last()) {
            (Some(a), Some(b)) => b.area_km2 - a.area_km2,
            _ => 0.0,
        }
    }
}

/// Build cell histories across consecutive volumes. The matcher is
/// intentionally light: a cell is the nearest last point within 25 km, which
/// is plenty for a short animation-ring trend and avoids inventing IDs.
pub fn storm_histories(
    volumes: &[&Volume],
    tilt_deg: f32,
    radar_lat: f64,
    radar_lon: f64,
) -> Vec<StormHistory> {
    if volumes.len() < 2 {
        return Vec::new();
    }
    let mut chains: Vec<StormHistory> = Vec::new();
    for pair in volumes.windows(2) {
        let tracks = compute_tracks(pair[0], pair[1], tilt_deg, radar_lat, radar_lon);
        let time = pair[1].meta.volume_start;
        let mut used = vec![false; chains.len()];
        for track in tracks {
            // A previous track may have created a new chain; keep the
            // one-to-one matcher in step with it.
            used.resize(chains.len(), false);
            let mut best: Option<(usize, f64)> = None;
            for (i, chain) in chains.iter().enumerate() {
                if used[i] {
                    continue;
                }
                let Some(last) = chain.points.last() else {
                    continue;
                };
                let d = surface_km(last.lat, last.lon, track.lat, track.lon);
                if d <= 25.0 && best.map_or(true, |(_, bd)| d < bd) {
                    best = Some((i, d));
                }
            }
            let point = StormHistoryPoint {
                time,
                lat: track.lat,
                lon: track.lon,
                max_dbz: track.max_dbz,
                area_km2: track.cells as f64 * 16.0,
                speed_kmh: track.speed_kmh,
                bearing_deg: track.bearing_deg,
            };
            if let Some((i, _)) = best {
                used[i] = true;
                chains[i].points.push(point);
            } else {
                chains.push(StormHistory {
                    points: vec![point],
                });
            }
        }
    }
    chains.retain(|c| c.points.len() >= 2);
    chains.sort_by(|a, b| {
        let aa = a.latest().map(|p| p.max_dbz).unwrap_or(0.0);
        let bb = b.latest().map(|p| p.max_dbz).unwrap_or(0.0);
        bb.partial_cmp(&aa).unwrap_or(std::cmp::Ordering::Equal)
    });
    chains.truncate(32);
    chains
}

fn surface_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let dy = (lat2 - lat1) * 110.574;
    let dx = (lon2 - lon1) * 111.320 * ((lat1 + lat2) * 0.5).to_radians().cos();
    (dx * dx + dy * dy).sqrt()
}

/// Lat/lon of a point `minutes` ahead along a track (the nowcast target).
pub fn extrapolate(t: &StormTrack, minutes: f64) -> (f64, f64) {
    let m_per_deg_lat = 110.574;
    let m_per_deg_lon = 111.320 * t.lat.to_radians().cos();
    (
        t.lat + (t.vel_n * minutes) / m_per_deg_lat,
        t.lon + (t.vel_e * minutes) / m_per_deg_lon,
    )
}

/// What a tracked cell will do about a fixed point: minutes until closest
/// approach plus the miss distance there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellEta {
    /// Minutes until closest approach (always >= 0).
    pub minutes: f64,
    /// Miss distance at closest approach, km (<= [`ETA_RADIUS_KM`]).
    pub miss_km: f64,
    /// Cell ground speed, km/h.
    pub speed_kmh: f64,
}

/// Time until a tracked cell reaches `(lon, lat)` — `None` when it never
/// will: parked, moving away, missing by more than [`ETA_RADIUS_KM`], or
/// farther out than [`ETA_MAX_MIN`].
///
/// Closest-approach on the local tangent plane: with relative position `r`
/// (cell − point, km) and velocity `v` (km/min), `t* = −(r·v)/(v·v)`.
/// Same km frame as the tracker's grid, so the numbers agree with it.
pub fn eta_to_point(t: &StormTrack, lon: f64, lat: f64) -> Option<CellEta> {
    let speed = t.vel_e.hypot(t.vel_n); // km/min
    if speed * 60.0 < MIN_SPEED_KMH {
        return None; // parked (compute_tracks filters these too)
    }
    let kx = 111.320 * lat.to_radians().cos();
    let ky = 110.574;
    let rx = (t.lon - lon) * kx;
    let ry = (t.lat - lat) * ky;
    let vv = t.vel_e * t.vel_e + t.vel_n * t.vel_n;
    let tmin = -(rx * t.vel_e + ry * t.vel_n) / vv;
    if tmin < 0.0 || tmin > ETA_MAX_MIN {
        return None;
    }
    let miss = (rx + t.vel_e * tmin).hypot(ry + t.vel_n * tmin);
    if miss > ETA_RADIUS_KM {
        return None;
    }
    Some(CellEta {
        minutes: tmin,
        miss_km: miss,
        speed_kmh: speed * 60.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};

    const RADAR_LAT: f64 = 35.0;
    const RADAR_LON: f64 = -97.0;

    /// One synthetic volume: a high-REF core centered on `center_gate` along
    /// the 45° ray (NE), ~12° az wide and 9 gates deep (well over MIN_CELLS).
    fn vol_with_core(center_gate: usize, minutes_from_epoch: i64) -> Volume {
        let rays = 360usize; // 1° spacing
        let gates = 120usize; // 1 km gates, 0.5 km first gate
        let mut azimuths = Vec::with_capacity(rays);
        let mut data = vec![0i16; rays * gates];
        for r in 0..rays {
            let az = r as f32;
            azimuths.push(az);
            for g in 0..gates {
                let az_ok = (az - 45.0).abs() <= 6.0;
                let g_ok = g + 6 >= center_gate && g <= center_gate + 6;
                // Calibration: value = (dn - 2) / 1 -> dn 47 == 45 dBZ.
                if az_ok && g_ok {
                    data[r * gates + g] = 47;
                }
            }
        }
        let sweep = Sweep {
            elevation_deg: 0.5,
            elevation_measured_deg: 0.5,
            azimuth_width_deg: 1.0,
            azimuths_deg: azimuths,
            moments: vec![radar_core::MomentData {
                kind: Moment::Reflectivity,
                calibration: radar_core::MomentCalibration::default(),
                range_start_m: 500.0,
                gate_size_m: 1000.0,
                gate_count: gates,
                ray_count: rays,
                data,
            }],
        };
        Volume {
            meta: radar_core::VolumeMeta {
                site_id: "KTEST".into(),
                volume_start: Utc.with_ymd_and_hms(2026, 10, 12, 12, 0, 0).unwrap()
                    + Duration::minutes(minutes_from_epoch),
                vcp: Some(12),
                sequence_number: None,
                sweep_count: 1,
                moments: vec![Moment::Reflectivity],
            },
            sweeps: vec![sweep],
        }
    }

    /// A core moving outward along the 45° ray (10 km in 5 min -> ~120 km/h
    /// toward the NE) must produce one track with matching speed and bearing.
    #[test]
    fn tracks_detect_outward_motion() {
        let prev = vol_with_core(50, 0);
        let cur = vol_with_core(60, 5);
        let tracks = compute_tracks(&prev, &cur, 0.5, RADAR_LAT, RADAR_LON);
        assert_eq!(tracks.len(), 1, "expected exactly one tracked cell");
        let t = &tracks[0];
        // 10 km / 5 min along az 45 => speed ~120 km/h, bearing ~45°.
        assert!(
            (90.0..=150.0).contains(&t.speed_kmh),
            "speed {} km/h out of range",
            t.speed_kmh
        );
        assert!(
            (30.0..=60.0).contains(&t.bearing_deg),
            "bearing {} deg out of range",
            t.bearing_deg
        );
        assert!(t.max_dbz >= 40.0, "max_dbz {}", t.max_dbz);
    }

    /// A pair whose dt is outside 1..45 minutes must be rejected.
    #[test]
    fn tracks_reject_bad_dt() {
        let prev = vol_with_core(50, 0);
        let cur = vol_with_core(60, 60); // 60 min later
        assert!(compute_tracks(&prev, &cur, 0.5, RADAR_LAT, RADAR_LON).is_empty());
    }

    /// No echo change -> no meaningful motion -> no tracks (stationary cells
    /// are filtered by MIN_SPEED_KMH).
    #[test]
    fn tracks_ignore_stationary_cores() {
        let prev = vol_with_core(50, 0);
        let cur = vol_with_core(50, 5);
        assert!(compute_tracks(&prev, &cur, 0.5, RADAR_LAT, RADAR_LON).is_empty());
    }

    fn head_on_track() -> StormTrack {
        // 60 km/h due east, 60 km west of the target.
        StormTrack {
            lat: 35.0,
            lon: -97.0 - 60.0 / (111.320 * 35.0f64.to_radians().cos()),
            vel_e: 1.0, // km/min
            vel_n: 0.0,
            speed_kmh: 60.0,
            bearing_deg: 90.0,
            max_dbz: 55.0,
            prev_max_dbz: 50.0,
            cells: 20,
            prev_cells: 18,
        }
    }

    #[test]
    fn histories_chain_across_three_scans() {
        let v0 = vol_with_core(20, 0);
        let v1 = vol_with_core(30, 5);
        let v2 = vol_with_core(40, 10);
        let histories = storm_histories(&[&v0, &v1, &v2], 0.5, RADAR_LAT, RADAR_LON);
        assert_eq!(histories.len(), 1);
        assert_eq!(histories[0].points.len(), 2);
        assert!(histories[0].latest().is_some());
    }

    #[test]
    fn trend_label_reports_growth_and_decay() {
        let mut t = head_on_track();
        t.prev_max_dbz = 49.0;
        assert_eq!(t.trend_label(), "↑ 6 dBZ");
        t.prev_max_dbz = 59.0;
        assert_eq!(t.trend_label(), "↓ 4 dBZ");
        t.prev_max_dbz = 54.0;
        assert_eq!(t.trend_label(), "steady");
        assert_eq!(t.delta_cells(), 2);
    }

    #[test]
    fn eta_counts_down_a_head_on_cell() {
        let t = head_on_track();
        let e = eta_to_point(&t, -97.0, 35.0).expect("head-on cell must have an ETA");
        assert!((e.minutes - 60.0).abs() < 0.5, "got {} min", e.minutes);
        assert!(e.miss_km < 0.5, "got {} km miss", e.miss_km);
        assert!((e.speed_kmh - 60.0).abs() < 1e-6);
    }

    #[test]
    fn eta_rejects_misses_and_departures() {
        let t = head_on_track();
        // 30 km north of the path: outside the arrival radius.
        assert!(eta_to_point(&t, -97.0, 35.0 + 30.0 / 110.574).is_none());
        // Behind the cell (it already passed): no ETA.
        assert!(eta_to_point(&t, -97.0 - 2.0, 35.0).is_none());
        // Parked cell: no ETA.
        let mut parked = t.clone();
        parked.vel_e = 0.0;
        parked.speed_kmh = 0.0;
        assert!(eta_to_point(&parked, -97.0, 35.0).is_none());
        // Farther than the horizon at this speed (60 km/h -> 120 km in 2 h).
        let mut far = t.clone();
        far.lon -= 3.0;
        assert!(eta_to_point(&far, -97.0, 35.0).is_none());
    }
}
