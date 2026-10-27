//! Low-level severe-weather signatures derived from velocity and MEHS.
//!
//! These are single-radar, single-scan detections meant to point the eye at a
//! feature, not to replace a mesocyclone algorithm or a warning decision.
//! Every result carries the raw velocity difference so the UI can rank it.

use chrono::{DateTime, Utc};
use std::collections::HashMap;

use crate::{Moment, Volume};

#[derive(Clone, Debug)]
pub struct RotationSignature {
    pub lat: f64,
    pub lon: f64,
    pub range_km: f64,
    pub azimuth_deg: f64,
    pub elevation_deg: f32,
    /// Inbound/outbound velocity difference across the couplet, m/s.
    pub delta_v_ms: f64,
    /// Azimuthal shear, s⁻¹.
    pub shear_s: f64,
    /// Strong, low-level gate-to-gate couplet.
    pub tvs: bool,
}

#[derive(Clone, Debug)]
pub struct HailCore {
    pub lat: f64,
    pub lon: f64,
    pub mehs_in: f64,
    pub time: DateTime<Utc>,
}

/// Detect the strongest low-level azimuthal velocity couplets.
pub fn rotation_signatures(
    volume: &Volume,
    site_lat: f64,
    site_lon: f64,
) -> Vec<RotationSignature> {
    let mut out = Vec::new();
    for sweep in &volume.sweeps {
        if sweep.elevation_deg > 4.0 {
            continue;
        }
        let Some(md) = sweep
            .moment(Moment::DealiasedVelocity)
            .or_else(|| sweep.moment(Moment::Velocity))
        else {
            continue;
        };
        if md.ray_count < 8 || md.gate_count < 4 {
            continue;
        }
        let ref_pair = volume
            .sweeps
            .iter()
            .find(|s| {
                s.moment(Moment::Reflectivity).is_some()
                    && (s.elevation_deg - sweep.elevation_deg).abs() < 0.25
            })
            .and_then(|s| s.moment(Moment::Reflectivity).map(|md| (s, md)));
        let az_width = sweep.azimuth_width_deg.max(0.1) as f64;
        let az_span_rad = (az_width * 4.0).to_radians();
        for ray in 0..md.ray_count {
            let az = f64::from(
                sweep
                    .azimuths_deg
                    .get(ray)
                    .copied()
                    .unwrap_or(ray as f32 * sweep.azimuth_width_deg),
            )
            .to_radians();
            for gate in 0..md.gate_count {
                let range_m = f64::from(md.range_start_m + gate as f32 * md.gate_size_m);
                let range_km = range_m / 1000.0;
                if !(5.0..=100.0).contains(&range_km) {
                    continue;
                }
                let r0 = (ray + md.ray_count - 2) % md.ray_count;
                let r1 = (ray + 2) % md.ray_count;
                let (Some(v0), Some(v1)) = (md.value(r0, gate), md.value(r1, gate)) else {
                    continue;
                };
                // Rotation needs opposite-signed inbound/outbound gates, not
                // just a strong radial gradient (which could be divergence or
                // a noisy dealias residue).
                if v0 * v1 >= 0.0 {
                    continue;
                }
                // Saturated gates are usually folded/range-ambiguous. A real
                // extreme couplet can exist, but this simple detector should
                // stay silent rather than paint a false TVS in clutter.
                if v0.abs() > 60.0 || v1.abs() > 60.0 {
                    continue;
                }
                let dv = (f64::from(v1) - f64::from(v0)).abs();
                if !(20.0..=70.0).contains(&dv) {
                    continue;
                }
                let shear = dv / (range_m * az_span_rad).max(1.0);
                if shear < 0.01 {
                    continue;
                }
                // A couplet in light echo or in a no-data hole is usually
                // ground clutter / AP / a folded gate, not a mesocyclone.
                // REF and VEL azimuth lists can be rotated relative to each
                // other, so sample REF by azimuth, not by the VEL ray index.
                let Some((ref_sweep, rf)) = ref_pair else {
                    continue;
                };
                let ref_ray = nearest_azimuth_ray(ref_sweep, az.to_degrees());
                let rg = ((range_m - f64::from(rf.range_start_m)) / f64::from(rf.gate_size_m))
                    .round() as isize;
                if rg < 0 || rg as usize >= rf.gate_count {
                    continue;
                }
                let Some(rdbz) = rf.value(ref_ray, rg as usize) else {
                    continue;
                };
                if rdbz < 40.0 {
                    continue;
                }
                let (lat, lon) = latlon(site_lat, site_lon, range_km, az.to_degrees());
                out.push(RotationSignature {
                    lat,
                    lon,
                    range_km,
                    azimuth_deg: az.to_degrees().rem_euclid(360.0),
                    elevation_deg: sweep.elevation_deg,
                    delta_v_ms: dv,
                    shear_s: shear,
                    tvs: dv >= 40.0
                        && shear >= 0.02
                        && sweep.elevation_deg <= 1.6
                        && range_km <= 70.0,
                });
            }
        }
    }
    out.sort_by(|a, b| {
        b.delta_v_ms
            .partial_cmp(&a.delta_v_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Keep the strongest few signatures, separated enough to avoid marking
    // every gate in one couplet.
    let mut kept: Vec<RotationSignature> = Vec::new();
    for sig in out {
        if kept
            .iter()
            .any(|k| radar_distance_km(k.lat, k.lon, sig.lat, sig.lon) < 6.0)
        {
            continue;
        }
        kept.push(sig);
        if kept.len() >= 6 {
            break;
        }
    }
    kept
}

/// Current MEHS hail cores (one per small grid cell).
pub fn hail_cores(volume: &Volume, site_lat: f64, site_lon: f64) -> Vec<HailCore> {
    let Some(sweep) = volume
        .sweeps
        .iter()
        .find(|s| s.moment(Moment::Mesh).is_some())
    else {
        return Vec::new();
    };
    let Some(md) = sweep.moment(Moment::Mesh) else {
        return Vec::new();
    };
    let mut bins: HashMap<(i32, i32), HailCore> = HashMap::new();
    for ray in 0..md.ray_count {
        let az = f64::from(
            sweep
                .azimuths_deg
                .get(ray)
                .copied()
                .unwrap_or(ray as f32 * sweep.azimuth_width_deg),
        )
        .to_radians();
        for gate in 0..md.gate_count {
            let Some(v) = md.value(ray, gate) else {
                continue;
            };
            if f64::from(v) < 0.75 {
                continue;
            }
            let range_km = f64::from(md.range_start_m + gate as f32 * md.gate_size_m) / 1000.0;
            let (lat, lon) = latlon(site_lat, site_lon, range_km, az.to_degrees());
            let key = ((lat * 20.0).round() as i32, (lon * 20.0).round() as i32);
            let core = HailCore {
                lat,
                lon,
                mehs_in: f64::from(v),
                time: volume.meta.volume_start,
            };
            bins.entry(key)
                .and_modify(|old| {
                    if core.mehs_in > old.mehs_in {
                        *old = core.clone();
                    }
                })
                .or_insert(core);
        }
    }
    let mut out: Vec<HailCore> = bins.into_values().collect();
    out.sort_by(|a, b| {
        b.mehs_in
            .partial_cmp(&a.mehs_in)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(16);
    out
}

/// Accumulate hail cores across the animation ring into a crude swath.
pub fn hail_swath(volumes: &[&Volume], site_lat: f64, site_lon: f64) -> Vec<HailCore> {
    let mut bins: HashMap<(i32, i32), HailCore> = HashMap::new();
    for volume in volumes {
        for core in hail_cores(volume, site_lat, site_lon) {
            let key = (
                (core.lat * 12.5).round() as i32,
                (core.lon * 12.5).round() as i32,
            );
            bins.entry(key)
                .and_modify(|old| {
                    if core.mehs_in > old.mehs_in {
                        *old = core.clone();
                    }
                })
                .or_insert(core);
        }
    }
    let mut out: Vec<HailCore> = bins.into_values().collect();
    out.sort_by(|a, b| a.time.cmp(&b.time));
    out.truncate(60);
    out
}

fn nearest_azimuth_ray(sweep: &crate::Sweep, az_deg: f64) -> usize {
    if sweep.azimuths_deg.is_empty() {
        return 0;
    }
    let target = az_deg.rem_euclid(360.0);
    sweep
        .azimuths_deg
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            angular_diff(**a as f64, target)
                .partial_cmp(&angular_diff(**b as f64, target))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(0)
}

fn angular_diff(a_deg: f64, b_deg: f64) -> f64 {
    let d = (a_deg - b_deg).rem_euclid(360.0);
    d.min(360.0 - d)
}

fn latlon(site_lat: f64, site_lon: f64, range_km: f64, az_deg: f64) -> (f64, f64) {
    let az = az_deg.to_radians();
    let dlat = range_km / 111.32 * az.cos();
    let dlon = range_km / (111.32 * site_lat.to_radians().cos().max(0.2)) * az.sin();
    (site_lat + dlat, site_lon + dlon)
}

fn radar_distance_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let dy = (lat2 - lat1) * 111.32;
    let dx = (lon2 - lon1) * 111.32 * ((lat1 + lat2) * 0.5).to_radians().cos();
    (dx * dx + dy * dy).sqrt()
}
