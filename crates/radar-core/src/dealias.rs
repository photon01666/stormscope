//! Lightweight velocity dealiasing for base VEL sweeps.
//!
//! The algorithm is deliberately simple: unwrap along the range direction,
//! then remove whole-fold offsets between adjacent azimuths. It is not a
//! research-grade dealiaser, but it makes the rotation couplets usable when a
//! scan has folded gates.

use crate::{Moment, MomentData, Sweep, Volume};

/// WSR-88D velocity data are laid out around this Nyquist. The actual value
/// varies with VCP/PRF, but this is a useful common scale and the unwrapping
/// works in whole 2*Nyquist folds.
const DEFAULT_NYQUIST_MS: f64 = 25.0;

/// Add one `DealiasedVelocity` sweep for every base VEL sweep. Idempotent.
pub fn attach_dealiased(volume: &mut Volume) {
    if volume
        .sweeps
        .iter()
        .any(|s| s.moment(Moment::DealiasedVelocity).is_some())
    {
        return;
    }
    let mut new_sweeps: Vec<Sweep> = Vec::new();
    for sweep in &volume.sweeps {
        if let Some(ds) = dealias_sweep(sweep) {
            new_sweeps.push(ds);
        }
    }
    if new_sweeps.is_empty() {
        return;
    }
    volume.sweeps.extend(new_sweeps);
    if !volume.meta.moments.contains(&Moment::DealiasedVelocity) {
        volume.meta.moments.push(Moment::DealiasedVelocity);
    }
}

fn dealias_sweep(sweep: &Sweep) -> Option<Sweep> {
    let md = sweep.moment(Moment::Velocity)?;
    if md.ray_count == 0 || md.gate_count == 0 {
        return None;
    }
    let fold = 2.0 * DEFAULT_NYQUIST_MS;
    let (rays, gates) = (md.ray_count, md.gate_count);
    let mut raw: Vec<Vec<Option<f64>>> = vec![vec![None; gates]; rays];
    for ray in 0..rays {
        for gate in 0..gates {
            let dn = md.data[ray * gates + gate];
            if dn == 0 {
                continue;
            }
            raw[ray][gate] = Some(
                (f64::from(dn) - f64::from(md.calibration.offset))
                    / f64::from(md.calibration.scale),
            );
        }
    }

    // Two local passes: choose the fold offset that best matches the eight
    // surrounding gates. This avoids the cumulative drift of whole-ray
    // unwrapping while still fixing most range/azimuth folds.
    let mut out = raw.clone();
    for pass in 0..2 {
        let src = if pass == 0 { &raw } else { &out };
        let mut next = out.clone();
        for ray in 0..rays {
            for gate in 0..gates {
                let Some(base) = raw[ray][gate] else {
                    continue;
                };
                let mut best = base;
                let mut best_err = f64::INFINITY;
                for k in -2..=2 {
                    let cand = base + k as f64 * fold;
                    let mut err = 0.0;
                    let mut n = 0.0;
                    for (dr, dg) in [
                        (-1isize, 0isize),
                        (1, 0),
                        (0, -1),
                        (0, 1),
                        (-1, -1),
                        (-1, 1),
                        (1, -1),
                        (1, 1),
                    ] {
                        let rr = (ray as isize + dr).rem_euclid(rays as isize) as usize;
                        let gg = gate as isize + dg;
                        if gg < 0 || gg >= gates as isize {
                            continue;
                        }
                        if let Some(v) = src[rr][gg as usize] {
                            let d = cand - v;
                            err += d * d;
                            n += 1.0;
                        }
                    }
                    if n > 0.0 && err < best_err {
                        best_err = err;
                        best = cand;
                    }
                }
                // Never invent an implausible velocity; an extreme value is
                // more likely a bad unwrap than real flow.
                next[ray][gate] = Some(if best.abs() > 100.0 { base } else { best });
            }
        }
        out = next;
    }

    let mut data = vec![0i16; rays * gates];
    let scale = f64::from(md.calibration.scale).max(1e-6);
    let offset = f64::from(md.calibration.offset);
    for ray in 0..rays {
        for gate in 0..gates {
            if let Some(v) = out[ray][gate] {
                let dn = (v * scale + offset).round();
                if dn.is_finite() {
                    data[ray * gates + gate] = dn.clamp(1.0, i16::MAX as f64) as i16;
                }
            }
        }
    }

    Some(Sweep {
        elevation_deg: sweep.elevation_deg,
        elevation_measured_deg: sweep.elevation_measured_deg,
        azimuth_width_deg: sweep.azimuth_width_deg,
        azimuths_deg: sweep.azimuths_deg.clone(),
        moments: vec![MomentData {
            kind: Moment::DealiasedVelocity,
            calibration: md.calibration,
            range_start_m: md.range_start_m,
            gate_size_m: md.gate_size_m,
            gate_count: gates,
            ray_count: rays,
            data,
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MomentCalibration, VolumeMeta};
    use chrono::Utc;

    fn volume_with_velocity(vals: Vec<f32>) -> Volume {
        let rays = 4;
        let gates = vals.len() / rays;
        let cal = MomentCalibration {
            scale: 1.0,
            offset: 129.0,
            threshold: 0.0,
        };
        let data = vals.iter().map(|v| (v + 129.0) as i16).collect();
        Volume {
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
        }
    }

    #[test]
    fn produces_one_dealiased_sweep_per_velocity_sweep() {
        let mut vol = volume_with_velocity(vec![0.0; 40]);
        attach_dealiased(&mut vol);
        assert_eq!(vol.sweeps.len(), 2);
        assert!(vol.sweeps[1].moment(Moment::DealiasedVelocity).is_some());
        attach_dealiased(&mut vol);
        assert_eq!(vol.sweeps.len(), 2, "idempotent");
    }
}
