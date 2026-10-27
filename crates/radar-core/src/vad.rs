//! Velocity-azimuth-display (VAD) wind profiles.
//!
//! At a fixed range, radial velocity varies as `v_r = u sin(az) + v cos(az)`.
//! A least-squares fit over one sweep therefore gives the horizontal wind at
//! that sweep's beam height. Stormscope samples a 20 km ring (falling back
//! through other rings) to build one level per elevation.

use crate::sampling::beam_height_km;
use crate::{Moment, Volume};

#[derive(Clone, Debug)]
pub struct VadLevel {
    pub height_m: f64,
    /// Eastward wind, m/s.
    pub u_ms: f64,
    /// Northward wind, m/s.
    pub v_ms: f64,
    pub speed_ms: f64,
    /// Meteorological direction the wind comes from, degrees.
    pub dir_deg: f64,
    /// RMS residual of the sinusoid fit, m/s.
    pub rms_ms: f64,
    pub gates: usize,
    pub range_km: f64,
}

#[derive(Clone, Debug, Default)]
pub struct VadProfile {
    pub levels: Vec<VadLevel>,
}

impl VadProfile {
    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }
}

pub fn vad_profile(volume: &Volume) -> VadProfile {
    let mut levels = Vec::new();
    for sweep in &volume.sweeps {
        let Some(level) = vad_level(sweep) else {
            continue;
        };
        levels.push(level);
    }
    levels.sort_by(|a, b| {
        a.height_m
            .partial_cmp(&b.height_m)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    VadProfile { levels }
}

fn vad_level(sweep: &crate::Sweep) -> Option<VadLevel> {
    let md = sweep
        .moment(Moment::DealiasedVelocity)
        .or_else(|| sweep.moment(Moment::Velocity))?;
    if md.ray_count < 30 || md.gate_count == 0 {
        return None;
    }
    // Prefer a 20 km ring; high elevations may not reach it, so try wider
    // rings before giving up on the cut.
    for target_km in [20.0f64, 30.0, 40.0, 15.0, 50.0] {
        if let Some(level) = fit_ring(md, sweep, target_km) {
            return Some(level);
        }
    }
    None
}

fn fit_ring(md: &crate::MomentData, sweep: &crate::Sweep, target_km: f64) -> Option<VadLevel> {
    let target_m = target_km * 1000.0;
    let half_width_m = 2500.0;
    let (mut suu, mut svv, mut suv, mut sru, mut srv) = (0.0, 0.0, 0.0, 0.0, 0.0);
    let (mut n, mut rsum, mut rr) = (0.0, 0.0, 0.0);
    for ray in 0..md.ray_count {
        let az = f64::from(
            sweep
                .azimuths_deg
                .get(ray)
                .copied()
                .unwrap_or(ray as f32 * sweep.azimuth_width_deg),
        )
        .to_radians();
        let (s, c) = (az.sin(), az.cos());
        for gate in 0..md.gate_count {
            let range_m = md.range_start_m as f64 + gate as f64 * md.gate_size_m as f64;
            if (range_m - target_m).abs() > half_width_m {
                continue;
            }
            let Some(vr) = md.value(ray, gate).map(f64::from) else {
                continue;
            };
            suu += s * s;
            svv += c * c;
            suv += s * c;
            sru += vr * s;
            srv += vr * c;
            n += 1.0;
            rsum += range_m;
            rr += range_m * range_m;
        }
    }
    if n < 20.0 {
        return None;
    }
    let det = suu * svv - suv * suv;
    if det.abs() < 1e-9 {
        return None;
    }
    let u = (sru * svv - srv * suv) / det;
    let v = (srv * suu - sru * suv) / det;
    let speed = u.hypot(v);
    let dir = (270.0 - v.atan2(u).to_degrees()).rem_euclid(360.0);
    let mean_range = rsum / n;
    // RMS residual around the fitted sinusoid.
    let mut err = 0.0;
    for ray in 0..md.ray_count {
        let az = f64::from(
            sweep
                .azimuths_deg
                .get(ray)
                .copied()
                .unwrap_or(ray as f32 * sweep.azimuth_width_deg),
        )
        .to_radians();
        let (s, c) = (az.sin(), az.cos());
        for gate in 0..md.gate_count {
            let range_m = md.range_start_m as f64 + gate as f64 * md.gate_size_m as f64;
            if (range_m - target_m).abs() > half_width_m {
                continue;
            }
            let Some(vr) = md.value(ray, gate).map(f64::from) else {
                continue;
            };
            let model = u * s + v * c;
            err += (vr - model) * (vr - model);
        }
    }
    let rms = (err / n).sqrt();
    if !speed.is_finite() || rms > 12.0 {
        return None;
    }
    let height_m = f64::from(beam_height_km(
        (mean_range / 1000.0) as f32,
        sweep.elevation_deg,
    )) * 1000.0;
    let _ = rr;
    Some(VadLevel {
        height_m,
        u_ms: u,
        v_ms: v,
        speed_ms: speed,
        dir_deg: dir,
        rms_ms: rms,
        gates: n as usize,
        range_km: mean_range / 1000.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MomentCalibration, MomentData, Sweep, VolumeMeta};
    use chrono::Utc;

    #[test]
    fn fits_a_uniform_wind_from_a_synthetic_ring() {
        let rays = 360;
        let gates = 100;
        let cal = MomentCalibration {
            scale: 1.0,
            offset: 129.0,
            threshold: 0.0,
        };
        let mut data = vec![0i16; rays * gates];
        // Wind from the west at 10 m/s -> u = +10, v = 0.
        for ray in 0..rays {
            let az = (ray as f64).to_radians();
            let vr = 10.0 * az.sin();
            for gate in 0..gates {
                data[ray * gates + gate] = ((vr + 129.0).round()) as i16;
            }
        }
        let vol = Volume {
            meta: VolumeMeta {
                site_id: "KTWX".into(),
                volume_start: Utc::now(),
                vcp: None,
                sequence_number: None,
                sweep_count: 1,
                moments: vec![Moment::Velocity],
            },
            sweeps: vec![Sweep {
                elevation_deg: 0.5,
                elevation_measured_deg: 0.5,
                azimuth_width_deg: 1.0,
                azimuths_deg: (0..rays).map(|i| i as f32).collect(),
                moments: vec![MomentData {
                    kind: Moment::Velocity,
                    calibration: cal,
                    range_start_m: 0.0,
                    gate_size_m: 250.0,
                    gate_count: gates,
                    ray_count: rays,
                    data,
                }],
            }],
        };
        let p = vad_profile(&vol);
        assert!(!p.is_empty());
        let l = &p.levels[0];
        assert!((l.u_ms - 10.0).abs() < 1.0, "u={}", l.u_ms);
        assert!(l.v_ms.abs() < 1.0, "v={}", l.v_ms);
        assert!((l.dir_deg - 270.0).abs() < 3.0, "dir={}", l.dir_deg);
    }
}
