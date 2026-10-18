//! Derived products computed from a volume's base moments — the fields
//! analysis viewers derive on the fly (GR2Analyst's five: Echo Tops, VIL,
//! VILD, POSH, MEHS; we ship the three that matter most for storm work:
//! VIL, Echo Tops, MEHS), plus storm-relative velocity from VEL.
//!
//! Each builder returns a synthetic single-moment **sweep** whose rays,
//! gates and geometry match the host cut, so the existing raster, 3D mesh,
//! sampler and probe paths consume it with no special cases: the map draws
//! it like any product, the 3D pane treats it like any moment.
//!
//! VIL, Echo Tops and MEHS depend only on the reflectivity volume, so they
//! are attached once, right after decode ([`attach_derived`]) — playback and
//! product switching then cost nothing. Storm-relative velocity depends on
//! the storm motion the user dials in, so it is built on demand
//! ([`storm_relative_volume`]).
//!
//! Formulas:
//!
//! * **VIL** (kg/m²) — Amburn & Wolf (1997): integrating
//!   `3.44e-6 · Z^(4/7)` over height (Z linear reflectivity), trapezoidal
//!   between beam heights at the column's slant range. Missing levels count
//!   as zero water, exactly like the operational algorithm.
//! * **Echo Tops** (kft) — height of the highest 18.5 dBZ echo, linearly
//!   interpolated between the two cuts that bracket the threshold.
//! * **MEHS** (in) — Witt et al. (1998): `0.1 · √VIL`.
//! * **SRM** (m/s) — radial velocity minus the storm motion projected onto
//!   each radial: `SRV = V − (u·sin(az) + v·cos(az))`.

use crate::sampling::{Profile, VolumeSampler, beam_height_km};
use crate::{Moment, MomentCalibration, MomentData, Sweep, Volume};

/// Kilofeet per kilometre — Echo Tops are stored in kilofeet (the
/// operational product unit, and the app's 3D pane speaks feet).
const KFT_PER_KM: f32 = 3.280_84;
/// Knot → m/s.
const KT_TO_MS: f32 = 0.514_444;
/// Echo-top threshold: the operational 18.5 dBZ.
const ECHO_TOP_DBZ: f32 = 18.5;
/// The VIL constant from Amburn & Wolf (1997).
const VIL_CONST: f32 = 3.44e-6;
/// Threading only pays past this many host cells.
const PARALLEL_MIN_CELLS: usize = 60_000;

/// ICD-style encoding for the derived fields: DN 0 stays "below threshold",
/// physical = (dn - offset) / scale.
fn cal(scale: f32, offset: f32) -> MomentCalibration {
    MomentCalibration {
        scale,
        offset,
        threshold: 0.0,
    }
}

/// Linear reflectivity to the VIL integrand `Z^(4/7)`.
///
/// `Z = 10^(dBZ/10)`, so `Z^(4/7) = 10^(dBZ · 4/70)`. The exponent form
/// avoids a squaring round trip and stays finite for absurd inputs.
fn z_to_water(dbz: f32) -> f32 {
    10f32.powf(dbz.clamp(-32.0, 95.0) * (4.0 / 70.0))
}

/// The host cut's shape: everything a derived sweep needs to mirror, with no
/// borrow on the volume (so derived sweeps can be appended while building).
#[derive(Clone, Debug)]
struct SweepShape {
    elevation_deg: f32,
    elevation_measured_deg: f32,
    azimuth_width_deg: f32,
    azimuths_deg: Vec<f32>,
    ray_count: usize,
    gate_count: usize,
    gate_size_m: f32,
    range_start_m: f32,
}

impl SweepShape {
    fn of(sweep: &Sweep, md: &MomentData) -> Self {
        Self {
            elevation_deg: sweep.elevation_deg,
            elevation_measured_deg: sweep.elevation_measured_deg,
            azimuth_width_deg: sweep.azimuth_width_deg,
            azimuths_deg: sweep.azimuths_deg.clone(),
            ray_count: md.ray_count,
            gate_count: md.gate_count,
            gate_size_m: md.gate_size_m,
            range_start_m: md.range_start_m,
        }
    }

    fn azimuth(&self, ray: usize) -> f32 {
        self.azimuths_deg
            .get(ray)
            .copied()
            .unwrap_or(ray as f32 * self.azimuth_width_deg)
    }
}

/// Build a synthetic sweep from a host shape, encoding `values` (physical)
/// with `calibration`.
fn synth(
    shape: &SweepShape,
    kind: Moment,
    calibration: MomentCalibration,
    values: &[f32],
) -> Sweep {
    let data = values
        .iter()
        .map(|&v| {
            if v.is_finite() {
                (v * calibration.scale + calibration.offset)
                    .round()
                    .clamp(1.0, 32_000.0) as i16
            } else {
                0
            }
        })
        .collect();
    Sweep {
        elevation_deg: shape.elevation_deg,
        elevation_measured_deg: shape.elevation_measured_deg,
        azimuth_width_deg: shape.azimuth_width_deg,
        azimuths_deg: shape.azimuths_deg.clone(),
        moments: vec![MomentData {
            kind,
            calibration,
            range_start_m: shape.range_start_m,
            gate_size_m: shape.gate_size_m,
            gate_count: shape.gate_count,
            ray_count: shape.ray_count,
            data,
        }],
    }
}

/// The host cut every column product is written on: the lowest REF cut —
/// beam heights only rise with elevation, so the lowest cut has the widest,
/// least obstructed view of each column.
fn host_ref_cut(volume: &Volume) -> Option<SweepShape> {
    volume
        .sweeps
        .iter()
        .filter_map(|s| {
            s.moment(Moment::Reflectivity)
                .map(|m| (s.elevation_deg, s, m))
        })
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, s, m)| SweepShape::of(s, m))
}

/// One derived value per (ray, gate) of the host cut, from a column closure.
///
/// The per-ray columns are independent, and a full-resolution host cut is
/// ~1.3 M columns × 14 cuts of work — threaded across the machine's cores
/// into row chunks of the output (a serial pass is ~350 ms; the decode
/// thread should not pay that).
fn column_map(
    volume: &Volume,
    shape: &SweepShape,
    calc: impl Fn(&Profile<'_>, f32) -> f32 + Sync,
) -> Vec<f32> {
    let (gates, rays) = (shape.gate_count, shape.ray_count);
    let mut values = vec![f32::NAN; rays * gates];
    if gates == 0 || rays == 0 {
        return values;
    }
    let sampler = VolumeSampler::new(volume, Moment::Reflectivity);
    let run_row = |row: &mut [f32], ray: usize| {
        let profile = sampler.profile(shape.azimuth(ray));
        for (g, out) in row.iter_mut().enumerate() {
            let slant = (shape.range_start_m + (g as f32 + 0.5) * shape.gate_size_m) / 1000.0;
            *out = calc(&profile, slant);
        }
    };

    let threads = if rays * gates >= PARALLEL_MIN_CELLS {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(8)
    } else {
        1
    };
    if threads > 1 {
        let chunk = rays.div_ceil(threads);
        let run_row = &run_row;
        std::thread::scope(|s| {
            for (i, chunk_values) in values.chunks_mut(chunk * gates).enumerate() {
                let base = i * chunk;
                s.spawn(move || {
                    for (local, row) in chunk_values.chunks_mut(gates).enumerate() {
                        run_row(row, base + local);
                    }
                });
            }
        });
    } else {
        for (ray, row) in values.chunks_mut(gates).enumerate() {
            run_row(row, ray);
        }
    }
    values
}

/// The VIL column integral for one (azimuth, slant range) column.
fn vil_column(profile: &Profile<'_>, slant_km: f32) -> f32 {
    let mut sum = 0.0f32;
    let mut any = false;
    let mut prev: Option<(f32, f32)> = None;
    for k in 0..profile.cut_count() {
        let elev = profile.cut_elevation_deg(k).unwrap_or(0.0);
        let h = beam_height_km(slant_km, elev);
        let v = profile.value_at_slant(k, slant_km);
        if v.is_some() {
            any = true;
        }
        let f = v.map(z_to_water).unwrap_or(0.0);
        if let Some((ph, pf)) = prev {
            let dh_m = (h - ph).max(0.0) * 1000.0;
            sum += VIL_CONST * 0.5 * (pf + f) * dh_m;
        }
        prev = Some((h, f));
    }
    if any { sum } else { f32::NAN }
}

/// The echo-top height (kft) for one column: the highest 18.5 dBZ crossing.
fn echo_top_column(profile: &Profile<'_>, slant_km: f32) -> f32 {
    let mut top = f32::NAN;
    let mut prev: Option<(f32, f32)> = None;
    for k in 0..profile.cut_count() {
        let Some(v) = profile.value_at_slant(k, slant_km) else {
            continue;
        };
        let h = beam_height_km(slant_km, profile.cut_elevation_deg(k).unwrap_or(0.0));
        if v >= ECHO_TOP_DBZ {
            // At or above the threshold: the top is at least here (any
            // earlier crossing was below this cut).
            top = h;
        } else if let Some((ph, pv)) = prev.filter(|(_, pv)| *pv >= ECHO_TOP_DBZ) {
            // First level back below: interpolate where it crossed.
            let t = ((pv - ECHO_TOP_DBZ) / (pv - v)).clamp(0.0, 1.0);
            top = ph + t * (h - ph);
        }
        prev = Some((h, v));
    }
    top * KFT_PER_KM // km → kft
}

/// VIL (kg/m²) per (ray, gate), a column quantity written on the lowest REF
/// cut.
pub fn vil_sweep(volume: &Volume) -> Option<Sweep> {
    let shape = host_ref_cut(volume)?;
    let values = column_map(volume, &shape, vil_column);
    Some(synth(&shape, Moment::Vil, cal(100.0, 0.0), &values))
}

/// Echo Tops (kft): the top of the ≥18.5 dBZ echo per column.
pub fn echo_tops_sweep(volume: &Volume) -> Option<Sweep> {
    let shape = host_ref_cut(volume)?;
    let values = column_map(volume, &shape, echo_top_column);
    Some(synth(&shape, Moment::EchoTops, cal(10.0, 0.0), &values))
}

/// Maximum Estimated Hail Size (inches): `0.1 · √VIL` (Witt et al. 1998).
pub fn mehs_sweep(volume: &Volume) -> Option<Sweep> {
    let shape = host_ref_cut(volume)?;
    let values = column_map(volume, &shape, vil_column);
    let mehs: Vec<f32> = values
        .iter()
        .map(|&v| {
            if v.is_finite() {
                0.1 * v.max(0.0).sqrt()
            } else {
                f32::NAN
            }
        })
        .collect();
    Some(synth(&shape, Moment::Mesh, cal(100.0, 0.0), &mehs))
}

/// Attach VIL, Echo Tops and MEHS to a freshly decoded volume (idempotent).
///
/// Called from the decode paths so the products exist by the time the volume
/// reaches the UI: product switching and playback then never pay for a
/// derivation, at the cost of ~8 MB per resident volume.
pub fn attach_derived(volume: &mut Volume) {
    if volume
        .sweeps
        .iter()
        .any(|s| s.moment(Moment::Vil).is_some())
    {
        return;
    }
    let Some(shape) = host_ref_cut(volume) else {
        return; // no reflectivity in this volume: nothing to derive
    };

    // VIL and MEHS share one column pass; MEHS is a pure function of VIL.
    let vil_values = column_map(volume, &shape, vil_column);
    let mehs_values: Vec<f32> = vil_values
        .iter()
        .map(|&v| {
            if v.is_finite() {
                0.1 * v.max(0.0).sqrt()
            } else {
                f32::NAN
            }
        })
        .collect();
    let tops_values = column_map(volume, &shape, echo_top_column);

    volume
        .sweeps
        .push(synth(&shape, Moment::Vil, cal(100.0, 0.0), &vil_values));
    volume
        .sweeps
        .push(synth(&shape, Moment::Mesh, cal(100.0, 0.0), &mehs_values));
    volume.sweeps.push(synth(
        &shape,
        Moment::EchoTops,
        cal(10.0, 0.0),
        &tops_values,
    ));

    for m in [Moment::Vil, Moment::Mesh, Moment::EchoTops] {
        if !volume.meta.moments.contains(&m) {
            volume.meta.moments.push(m);
        }
    }
}

/// Storm-relative velocity sweeps: one synthetic `StormRelativeVelocity`
/// sweep per VEL sweep, with the station's VEL calibration.
///
/// `motion_toward_deg` is the direction the storms are moving *toward*
/// (degrees clockwise from north) and `speed_kt` the motion speed.
pub fn storm_relative_sweeps(volume: &Volume, motion_toward_deg: f32, speed_kt: f32) -> Vec<Sweep> {
    let s_ms = speed_kt * KT_TO_MS;
    let th = motion_toward_deg.to_radians();
    let (u, v) = (s_ms * th.sin(), s_ms * th.cos());
    let mut out = Vec::new();
    for sweep in &volume.sweeps {
        let Some(md) = sweep.moment(Moment::Velocity) else {
            continue;
        };
        let mut data = vec![0i16; md.ray_count * md.gate_count];
        for ray in 0..md.ray_count {
            let az = sweep
                .azimuths_deg
                .get(ray)
                .copied()
                .unwrap_or(ray as f32 * sweep.azimuth_width_deg)
                .to_radians();
            // Storm motion projected onto this radial (positive = away).
            let storm_radial = u * az.sin() + v * az.cos();
            for gate in 0..md.gate_count {
                let dn = md.data[ray * md.gate_count + gate];
                if dn == 0 {
                    continue;
                }
                let vel = (f32::from(dn) - md.calibration.offset) / md.calibration.scale;
                let srv = vel - storm_radial;
                data[ray * md.gate_count + gate] = (srv * md.calibration.scale
                    + md.calibration.offset)
                    .round()
                    .clamp(1.0, 32_000.0) as i16;
            }
        }
        out.push(Sweep {
            elevation_deg: sweep.elevation_deg,
            elevation_measured_deg: sweep.elevation_measured_deg,
            azimuth_width_deg: sweep.azimuth_width_deg,
            azimuths_deg: sweep.azimuths_deg.clone(),
            moments: vec![MomentData {
                kind: Moment::StormRelativeVelocity,
                calibration: md.calibration,
                range_start_m: md.range_start_m,
                gate_size_m: md.gate_size_m,
                gate_count: md.gate_count,
                ray_count: md.ray_count,
                data,
            }],
        });
    }
    out
}

/// A display volume carrying only the storm-relative sweeps for one motion
/// setting — what the map/3D/probe read while SRM is the selected product.
pub fn storm_relative_volume(
    volume: &Volume,
    motion_toward_deg: f32,
    speed_kt: f32,
) -> Option<Volume> {
    let sweeps = storm_relative_sweeps(volume, motion_toward_deg, speed_kt);
    if sweeps.is_empty() {
        return None;
    }
    let mut meta = volume.meta.clone();
    meta.moments = vec![Moment::StormRelativeVelocity];
    meta.sweep_count = sweeps.len();
    Some(Volume { meta, sweeps })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VolumeMeta;
    use crate::sampling::beam_height_km as h;
    use chrono::TimeZone;

    /// A volume with a constant reflectivity column on every cut.
    fn volume_with(dbz: f32, elevations: &[f32], ray_count: usize, gate_count: usize) -> Volume {
        let cal = MomentCalibration {
            scale: 1.0,
            offset: 2.0,
            threshold: 0.0,
        };
        let sweeps = elevations
            .iter()
            .map(|&elev| {
                let azs: Vec<f32> = (0..ray_count)
                    .map(|i| i as f32 * 360.0 / ray_count as f32)
                    .collect();
                let data = vec![(dbz + 2.0) as i16; ray_count * gate_count];
                Sweep {
                    elevation_deg: elev,
                    elevation_measured_deg: elev,
                    azimuth_width_deg: 360.0 / ray_count as f32,
                    azimuths_deg: azs,
                    moments: vec![MomentData {
                        kind: Moment::Reflectivity,
                        calibration: cal,
                        range_start_m: 0.0,
                        gate_size_m: 250.0,
                        gate_count,
                        ray_count,
                        data,
                    }],
                }
            })
            .collect();
        Volume {
            meta: VolumeMeta {
                site_id: "KTWX".into(),
                volume_start: chrono::Utc.with_ymd_and_hms(2026, 10, 18, 0, 0, 0).unwrap(),
                vcp: Some(212),
                sequence_number: Some(1),
                sweep_count: elevations.len(),
                moments: vec![Moment::Reflectivity],
            },
            sweeps,
        }
    }

    /// VIL integrates `3.44e-6·Z^(4/7)` over the beam heights at the column's
    /// slant range: with a constant profile that is exactly the integral
    /// between the lowest and highest cut.
    #[test]
    fn vil_integrates_the_column_between_cuts() {
        let vol = volume_with(50.0, &[0.5, 1.5, 2.5], 360, 200);
        let vil = vil_sweep(&vol).expect("VIL sweep");
        assert_eq!(vil.moments[0].kind, Moment::Vil);
        assert_eq!(vil.moments[0].gate_count, 200);
        assert_eq!(vil.moments[0].ray_count, 360);

        // Host gate 119 → slant 29.875 km.
        let slant = 119.5 * 0.25;
        let expect = VIL_CONST
            * 10f32.powf(50.0 * (4.0 / 70.0))
            * ((h(slant, 2.5) - h(slant, 0.5)) * 1000.0);
        let got = vil.moments[0].value(0, 119).expect("VIL at gate 119");
        assert!(
            (got - expect).abs() < 0.05 * expect.max(0.1),
            "VIL {got} vs expected {expect}"
        );
        // Every column that carries echo carries VIL.
        assert!(vil.moments[0].value(359, 100).is_some());
    }

    /// Echo tops: with 50 dBZ on every cut the top is the *highest* cut's
    /// beam height (in kft); a column whose top cut is below the threshold
    /// interpolates the crossing instead.
    #[test]
    fn echo_tops_finds_the_highest_crossing() {
        let vol = volume_with(50.0, &[0.5, 1.5, 2.5], 360, 200);
        let tops = echo_tops_sweep(&vol).expect("tops sweep");
        let slant = 119.5 * 0.25;
        let expect = h(slant, 2.5) * KFT_PER_KM;
        let got = tops.moments[0].value(0, 119).expect("tops at gate 119");
        // The stored value is quantized at 0.1 kft (scale 10).
        assert!(
            (got - expect).abs() < 0.11,
            "tops {got} kft vs expected {expect}"
        );

        // Only the low cut reaches the threshold: the crossing lies between
        // the first two cuts, where 18.5 dBZ sits just above 18.3.
        let mut weak = volume_with(50.0, &[0.5, 1.5, 2.5], 360, 200);
        for s in weak.sweeps.iter_mut().skip(1) {
            for d in s.moments[0].data.iter_mut() {
                *d = (18.3 + 2.0) as i16;
            }
        }
        let tops = echo_tops_sweep(&weak).unwrap();
        let t = tops.moments[0].value(0, 119).unwrap();
        let (h0, h1) = (h(slant, 0.5), h(slant, 1.5));
        let frac = (50.0 - 18.5) / (50.0 - 18.3);
        let expect = (h0 + frac * (h1 - h0)) * KFT_PER_KM;
        assert!((t - expect).abs() < 0.11, "tops {t} vs crossing {expect}");
    }

    /// MEHS is 0.1·√VIL, in inches, on the same column.
    #[test]
    fn mehs_is_the_square_root_of_vil() {
        let vol = volume_with(50.0, &[0.5, 1.5, 2.5], 360, 200);
        let vil = vil_sweep(&vol).unwrap();
        let mehs = mehs_sweep(&vol).unwrap();
        assert_eq!(mehs.moments[0].kind, Moment::Mesh);
        let v_vil = vil.moments[0].value(0, 119).unwrap();
        let v_mehs = mehs.moments[0].value(0, 119).unwrap();
        assert!(
            (v_mehs - 0.1 * v_vil.sqrt()).abs() < 0.02,
            "MEHS {v_mehs} vs 0.1·√{v_vil}"
        );
    }

    /// Attaching is one call, idempotent, and registers the moments.
    #[test]
    fn attach_derived_registers_and_is_idempotent() {
        let mut vol = volume_with(40.0, &[0.5, 1.5, 2.5], 90, 100);
        let before = vol.sweeps.len();
        attach_derived(&mut vol);
        assert_eq!(vol.sweeps.len(), before + 3);
        for m in [Moment::Vil, Moment::EchoTops, Moment::Mesh] {
            assert!(vol.meta.moments.contains(&m), "{m:?} missing");
            assert!(vol.sweeps.iter().any(|s| s.moment(m).is_some()));
        }
        attach_derived(&mut vol); // second call must not duplicate
        assert_eq!(vol.sweeps.len(), before + 3);
    }

    /// A volume without reflectivity derives nothing.
    #[test]
    fn attach_derived_skips_volumes_without_reflectivity() {
        let mut vol = volume_with(40.0, &[0.5], 8, 8);
        vol.sweeps.clear();
        attach_derived(&mut vol);
        assert!(vol.sweeps.is_empty());
        assert!(!vol.meta.moments.contains(&Moment::Vil));
    }

    /// SRM removes the storm motion's radial component: with 10 m/s of
    /// uniform velocity and 10 m/s of storm motion toward the east, the east
    /// radial reads 0, the west radial +20, north/south +10.
    #[test]
    fn storm_relative_velocity_subtracts_the_motion() {
        let cal = MomentCalibration {
            scale: 2.0,
            offset: 2.0,
            threshold: 0.0,
        };
        // 10 m/s → DN = 10·2 + 2 = 22.
        let rays = [0.0f32, 90.0, 180.0, 270.0];
        let vol = Volume {
            meta: VolumeMeta {
                site_id: "KTWX".into(),
                volume_start: chrono::Utc.with_ymd_and_hms(2026, 10, 18, 0, 0, 0).unwrap(),
                vcp: None,
                sequence_number: None,
                sweep_count: 1,
                moments: vec![Moment::Velocity],
            },
            sweeps: vec![Sweep {
                elevation_deg: 0.5,
                elevation_measured_deg: 0.5,
                azimuth_width_deg: 90.0,
                azimuths_deg: rays.to_vec(),
                moments: vec![MomentData {
                    kind: Moment::Velocity,
                    calibration: cal,
                    range_start_m: 0.0,
                    gate_size_m: 250.0,
                    gate_count: 4,
                    ray_count: 4,
                    data: vec![22i16; 16],
                }],
            }],
        };
        let motion_kt = 10.0 / KT_TO_MS;
        let srm = storm_relative_volume(&vol, 90.0, motion_kt).expect("SRM volume");
        assert_eq!(srm.sweeps.len(), 1);
        assert_eq!(srm.meta.moments, vec![Moment::StormRelativeVelocity]);
        let md = srm.sweeps[0]
            .moment(Moment::StormRelativeVelocity)
            .expect("SRM moment");
        assert!((md.value(0, 0).unwrap() - 10.0).abs() < 0.6, "north ray");
        assert!((md.value(1, 0).unwrap() - 0.0).abs() < 0.6, "east ray");
        assert!((md.value(2, 0).unwrap() - 10.0).abs() < 0.6, "south ray");
        assert!((md.value(3, 0).unwrap() - 20.0).abs() < 0.6, "west ray");
        // No velocity in the volume → no SRM volume.
        let mut empty = vol.clone();
        empty.sweeps.clear();
        assert!(storm_relative_volume(&empty, 90.0, 10.0).is_none());
    }
}
