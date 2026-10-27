//! Display-level quality control for reflectivity.
//!
//! The map can apply a dual-pol clutter mask, a KDP-based attenuation
//! correction and a manual calibration bias without changing the decoded
//! volume. This is intentionally a display product: the raw REF stays
//! available, and the transform is recomputed only when the map rasterises.

use crate::sampling::beam_height_km;
use crate::terrain::TerrainGrid;
use crate::{Moment, MomentData, Sweep, Volume};

#[derive(Clone, Copy, Debug, Default)]
pub struct QcConfig {
    pub clutter_filter: bool,
    pub attenuation_correction: bool,
    /// Manual calibration bias, dB.
    pub bias_db: f32,
}

impl QcConfig {
    pub fn active(self) -> bool {
        self.clutter_filter || self.attenuation_correction || self.bias_db.abs() > 0.01
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct QcStats {
    pub total: usize,
    pub clutter_masked: usize,
    pub attenuation_corrected: usize,
    pub bias_applied: usize,
    pub blockage_masked: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct QcTerrain<'a> {
    pub grid: &'a TerrainGrid,
    pub site_lat: f64,
    pub site_lon: f64,
    pub site_elev_m: f64,
}

pub fn qc_reflectivity_sweep(
    volume: &Volume,
    sweep: &Sweep,
    cfg: QcConfig,
    terrain: Option<QcTerrain<'_>>,
) -> (Sweep, QcStats) {
    let Some(md) = sweep.moment(Moment::Reflectivity) else {
        return (sweep.clone(), QcStats::default());
    };
    let cc = matching_sweep(volume, sweep.elevation_deg, Moment::CorrelationCoefficient);
    let kdp = matching_sweep(
        volume,
        sweep.elevation_deg,
        Moment::SpecificDifferentialPhase,
    );
    let mut data = md.data.clone();
    let mut stats = QcStats {
        total: data.len(),
        ..QcStats::default()
    };
    if !cfg.active() {
        return (sweep.clone(), stats);
    }
    let dr_km = f64::from(md.gate_size_m) / 1000.0;
    for ray in 0..md.ray_count {
        let az = sweep
            .azimuths_deg
            .get(ray)
            .copied()
            .unwrap_or(ray as f32 * sweep.azimuth_width_deg)
            .rem_euclid(360.0);
        let cc_ray = cc.map(|(s, _)| nearest_azimuth_ray(s, az));
        let kdp_ray = kdp.map(|(s, _)| nearest_azimuth_ray(s, az));
        let mut attenuation = 0.0;
        for gate in 0..md.gate_count {
            let idx = ray * md.gate_count + gate;
            let dn = md.data[idx];
            if dn == 0 {
                continue;
            }
            let z = (f64::from(dn) - f64::from(md.calibration.offset))
                / f64::from(md.calibration.scale).max(1e-6);
            let range_m = f64::from(md.range_start_m + gate as f32 * md.gate_size_m);

            if let Some(t) = terrain {
                let range_km = range_m / 1000.0;
                let (lat, lon) =
                    latlon(t.site_lat, t.site_lon, range_km, f64::from(az.to_degrees()));
                if let Some(ground) = t.grid.sample(lat, lon) {
                    let beam = t.site_elev_m
                        + f64::from(beam_height_km(range_km as f32, sweep.elevation_deg)) * 1000.0;
                    if f64::from(ground) > beam + 50.0 {
                        data[idx] = 0;
                        stats.blockage_masked += 1;
                        continue;
                    }
                }
            }

            if cfg.clutter_filter {
                if let (Some((_, s)), Some(r)) = (cc, cc_ray) {
                    let Some(ccv) = sample_at_range(s, r, range_m) else {
                        continue;
                    };
                    // Low CC in weak echo is the classic ground-clutter /
                    // biological signature. Do not mask strong hail/debris.
                    if ccv < 0.80 && z < 35.0 {
                        data[idx] = 0;
                        stats.clutter_masked += 1;
                        continue;
                    }
                }
            }

            let mut corrected = z;
            if cfg.attenuation_correction {
                if attenuation > 0.0 {
                    corrected += attenuation;
                    stats.attenuation_corrected += 1;
                }
                if let (Some((_, s)), Some(r)) = (kdp, kdp_ray) {
                    if let Some(k) = sample_at_range(s, r, range_m) {
                        // Approximate S-band specific attenuation. Keep the
                        // coefficient and KDP cap conservative: the decoded
                        // KDP field can contain extreme values, and this is a
                        // display correction, not a retrieval.
                        let kdp = f64::from(k).clamp(0.0, 10.0);
                        let a = 0.0015 * kdp;
                        attenuation += 2.0 * a * dr_km;
                    }
                }
            }
            if cfg.bias_db.abs() > 0.01 {
                corrected += f64::from(cfg.bias_db);
                stats.bias_applied += 1;
            }
            let out = (corrected * f64::from(md.calibration.scale)
                + f64::from(md.calibration.offset))
            .round();
            data[idx] = out.clamp(1.0, i16::MAX as f64) as i16;
        }
    }
    (
        Sweep {
            elevation_deg: sweep.elevation_deg,
            elevation_measured_deg: sweep.elevation_measured_deg,
            azimuth_width_deg: sweep.azimuth_width_deg,
            azimuths_deg: sweep.azimuths_deg.clone(),
            moments: vec![MomentData {
                kind: Moment::Reflectivity,
                calibration: md.calibration,
                range_start_m: md.range_start_m,
                gate_size_m: md.gate_size_m,
                gate_count: md.gate_count,
                ray_count: md.ray_count,
                data,
            }],
        },
        stats,
    )
}

fn matching_sweep(volume: &Volume, elev: f32, kind: Moment) -> Option<(&Sweep, &MomentData)> {
    volume
        .sweeps
        .iter()
        .find(|s| s.moment(kind).is_some() && (s.elevation_deg - elev).abs() < 0.25)
        .and_then(|s| s.moment(kind).map(|md| (s, md)))
}

fn sample_at_range(md: &MomentData, ray: usize, range_m: f64) -> Option<f32> {
    let gate =
        ((range_m - f64::from(md.range_start_m)) / f64::from(md.gate_size_m)).round() as isize;
    if gate < 0 || gate as usize >= md.gate_count {
        return None;
    }
    md.value(ray, gate as usize)
}

fn latlon(site_lat: f64, site_lon: f64, range_km: f64, az_deg: f64) -> (f64, f64) {
    let az = az_deg.to_radians();
    let dlat = range_km / 111.32 * az.cos();
    let dlon = range_km / (111.32 * site_lat.to_radians().cos().max(0.2)) * az.sin();
    (site_lat + dlat, site_lon + dlon)
}

fn nearest_azimuth_ray(sweep: &Sweep, az_deg: f32) -> usize {
    if sweep.azimuths_deg.is_empty() {
        return 0;
    }
    let target = az_deg.rem_euclid(360.0) as f64;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MomentCalibration, VolumeMeta};
    use chrono::Utc;

    #[test]
    fn bias_shifts_reflectivity_and_keeps_shape() {
        let cal = MomentCalibration {
            scale: 1.0,
            offset: 66.0,
            threshold: 0.0,
        };
        let sweep = Sweep {
            elevation_deg: 0.5,
            elevation_measured_deg: 0.5,
            azimuth_width_deg: 1.0,
            azimuths_deg: vec![0.0; 2],
            moments: vec![MomentData {
                kind: Moment::Reflectivity,
                calibration: cal,
                range_start_m: 0.0,
                gate_size_m: 250.0,
                gate_count: 2,
                ray_count: 2,
                data: vec![86, 86, 86, 86],
            }],
        };
        let vol = Volume {
            meta: VolumeMeta {
                site_id: "KTWX".into(),
                volume_start: Utc::now(),
                vcp: None,
                sequence_number: None,
                sweep_count: 1,
                moments: vec![Moment::Reflectivity],
            },
            sweeps: vec![sweep.clone()],
        };
        let (out, stats) = qc_reflectivity_sweep(
            &vol,
            &sweep,
            QcConfig {
                bias_db: 2.0,
                ..QcConfig::default()
            },
            None,
        );
        assert_eq!(out.moments[0].data, vec![88, 88, 88, 88]);
        assert_eq!(stats.bias_applied, 4);
    }
}
