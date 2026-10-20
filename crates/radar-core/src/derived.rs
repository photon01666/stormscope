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

use crate::sampling::{
    Profile, VolumeSampler, beam_height_km, gate_index_at_slant, nearest_ray_index,
};
use crate::winter::HydrometeorClass;
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
fn host_ref(volume: &Volume) -> Option<(&Sweep, SweepShape)> {
    let sweep = volume
        .sweeps
        .iter()
        .filter(|s| s.moment(Moment::Reflectivity).is_some())
        .min_by(|a, b| {
            a.elevation_deg
                .partial_cmp(&b.elevation_deg)
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
    let md = sweep.moment(Moment::Reflectivity)?;
    Some((sweep, SweepShape::of(sweep, md)))
}

fn host_ref_cut(volume: &Volume) -> Option<SweepShape> {
    host_ref(volume).map(|(_, shape)| shape)
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
    fill_rows(&mut values, gates, |ray, row| {
        let profile = sampler.profile(shape.azimuth(ray));
        for (g, out) in row.iter_mut().enumerate() {
            let slant = (shape.range_start_m + g as f32 * shape.gate_size_m) / 1000.0;
            *out = calc(&profile, slant);
        }
    });
    values
}

/// Fill a ray-major value grid row by row, threaded over the rows (a
/// full-resolution host cut is ~1.3 M gates of work per product).
fn fill_rows(values: &mut [f32], gates: usize, run_row: impl Fn(usize, &mut [f32]) + Sync) {
    if gates == 0 || values.is_empty() {
        return;
    }
    let rays = values.len() / gates;
    let threads = if rays * gates >= PARALLEL_MIN_CELLS {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(8)
    } else {
        1
    };
    if threads > 1 && rays > 1 {
        let chunk = rays.div_ceil(threads);
        let run_row = &run_row;
        std::thread::scope(|s| {
            for (i, chunk_values) in values.chunks_mut(chunk * gates).enumerate() {
                let base = i * chunk;
                s.spawn(move || {
                    for (local, row) in chunk_values.chunks_mut(gates).enumerate() {
                        run_row(base + local, row);
                    }
                });
            }
        });
    } else {
        for (ray, row) in values.chunks_mut(gates).enumerate() {
            run_row(ray, row);
        }
    }
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

/// Hydrometeor classification on the lowest REF cut — the surface precip
/// type, which is the winter question ("is that snow, rain, or the melting
/// layer?"). Inputs are Z, ZDR, CC and KDP from the same cut; without
/// dual-pol the product does not exist.
pub fn hydrometeor_sweep(volume: &Volume) -> Option<Sweep> {
    let (sweep, shape) = host_ref(volume)?;
    let z_md = sweep.moment(Moment::Reflectivity)?;
    let zdr_md = sweep.moment(Moment::DifferentialReflectivity)?;
    let cc_md = sweep.moment(Moment::CorrelationCoefficient)?;
    let kdp_md = sweep.moment(Moment::SpecificDifferentialPhase);
    let (gates, rays) = (shape.gate_count, shape.ray_count);
    let mut values = vec![f32::NAN; rays * gates];
    fill_rows(&mut values, gates, |ray, row| {
        let Some(r) = nearest_ray_index(&sweep.azimuths_deg, shape.azimuth(ray)) else {
            return;
        };
        for (g, out) in row.iter_mut().enumerate() {
            let Some(z) = z_md.value(r, g) else { continue };
            let slant = (shape.range_start_m + g as f32 * shape.gate_size_m) / 1000.0;
            let sample =
                |md: &MomentData| gate_index_at_slant(md, slant).and_then(|gate| md.value(r, gate));
            let (Some(zdr), Some(cc)) = (sample(zdr_md), sample(cc_md)) else {
                continue;
            };
            let kdp = kdp_md.and_then(sample);
            if let Some(class) = classify_hydrometeor(z, zdr, cc, kdp) {
                *out = class.value();
            }
        }
    });
    Some(synth(&shape, Moment::Hydrometeor, cal(1.0, 0.0), &values))
}

/// The winter-focused decision tree over Z (dBZ), ZDR (dB), CC and KDP
/// (deg/km): a simplified, documented heuristic rather than the operational
/// fuzzy-logic HCA — the classes answer the questions winter radar work asks.
fn classify_hydrometeor(z: f32, zdr: f32, cc: f32, kdp: Option<f32>) -> Option<HydrometeorClass> {
    if !(z.is_finite() && zdr.is_finite() && cc.is_finite()) {
        return None;
    }
    let kdp = kdp.filter(|k| k.is_finite()).unwrap_or(0.0);
    // No consistent particle shape: debris, clutter, or a mixed target.
    if cc < 0.80 {
        return Some(HydrometeorClass::Clutter);
    }
    // The melting layer, wet snow and rain/snow mixtures all live here.
    if cc < 0.93 {
        return Some(HydrometeorClass::Mixed);
    }
    // Strong reflectivity with little shape preference: graupel or hail.
    if z >= 45.0 && zdr < 1.5 {
        return Some(HydrometeorClass::GraupelHail);
    }
    // Liquid: oblate drops read high ZDR; big drops/heavy rain also high KDP.
    if zdr >= 1.0 {
        return Some(if z >= 40.0 || kdp >= 1.5 {
            HydrometeorClass::HeavyRain
        } else {
            HydrometeorClass::Rain
        });
    }
    // Frozen, mostly round particles: wet snow keeps a little shape, dry snow
    // almost none, and ice crystals are both weak and compact.
    if zdr >= 0.35 {
        return Some(HydrometeorClass::WetSnow);
    }
    if z >= 10.0 {
        return Some(HydrometeorClass::DrySnow);
    }
    Some(HydrometeorClass::IceCrystals)
}

/// Snowfall rate (in/h) from the lowest cut's reflectivity, via a simple
/// winter Z-S relation anchored at 20 dBZ ≈ 0.5 in/h and 30 dBZ ≈ 1.5 in/h
/// (about a 10:1 snow ratio). Masked outside 5-38 dBZ: below is noise, above
/// is rain or hail, not snow. A hint, not a gauge.
pub fn snow_rate_sweep(volume: &Volume) -> Option<Sweep> {
    let shape = host_ref_cut(volume)?;
    let values = column_map(volume, &shape, |profile, slant| {
        let Some(z) = profile.value_at_slant(0, slant) else {
            return f32::NAN;
        };
        if !(5.0..=38.0).contains(&z) {
            return f32::NAN;
        }
        0.0556 * 10f32.powf(0.1 * z).powf(0.48)
    });
    Some(synth(&shape, Moment::SnowRate, cal(100.0, 0.0), &values))
}

/// Melting-layer height (kft): the core of the correlation-coefficient bright
/// band — the height of the lowest CC in the 0.4-7 km layer when that minimum
/// is a real dip (< 0.96). This is the radar's view of the 0 °C level (the
/// band's centre sits a few hundred feet below it).
pub fn melting_layer_sweep(volume: &Volume) -> Option<Sweep> {
    let shape = host_ref_cut(volume)?;
    let mut cuts: Vec<(f32, &MomentData)> = volume
        .sweeps
        .iter()
        .filter_map(|s| {
            let md = s.moment(Moment::CorrelationCoefficient)?;
            Some((s.elevation_measured_deg, md))
        })
        .collect();
    cuts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if cuts.len() < 3 {
        return None;
    }
    let (gates, rays) = (shape.gate_count, shape.ray_count);
    let mut values = vec![f32::NAN; rays * gates];
    fill_rows(&mut values, gates, |ray, row| {
        for (g, out) in row.iter_mut().enumerate() {
            let slant = (shape.range_start_m + g as f32 * shape.gate_size_m) / 1000.0;
            let mut best: Option<(f32, f32)> = None; // (cc, height km)
            for (elev, md) in &cuts {
                let h = beam_height_km(slant, *elev);
                if !(0.4..=7.0).contains(&h) {
                    continue;
                }
                if let Some(cc) =
                    gate_index_at_slant(md, slant).and_then(|gate| md.value(ray, gate))
                    && best.is_none_or(|(bc, _)| cc < bc)
                {
                    best = Some((cc, h));
                }
            }
            if let Some((cc, h)) = best
                && cc < 0.96
            {
                *out = h * KFT_PER_KM;
            }
        }
    });
    Some(synth(&shape, Moment::MeltingLayer, cal(10.0, 0.0), &values))
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
    // Winter products, from the same host cut.
    let hca = hydrometeor_sweep(volume);
    let snow = snow_rate_sweep(volume);
    let melt = melting_layer_sweep(volume);

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

    let mut added = vec![Moment::Vil, Moment::Mesh, Moment::EchoTops];
    for derived in [hca, snow, melt].into_iter().flatten() {
        added.push(derived.moments[0].kind);
        volume.sweeps.push(derived);
    }
    for m in added {
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

        // Host gate 119 → its centre at 29.75 km.
        let slant = 119.0 * 0.25;
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
        let slant = 119.0 * 0.25;
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

    /// Attaching is one call, idempotent, and registers the moments. Without
    /// dual-pol the winter classification and melting layer are absent; with
    /// it, all six products ride along.
    #[test]
    fn attach_derived_registers_and_is_idempotent() {
        let mut vol = volume_with(40.0, &[0.5, 1.5, 2.5], 90, 100);
        let before = vol.sweeps.len();
        attach_derived(&mut vol);
        // VIL, MEHS, TOPS, SNOW (no HCA/MELT without dual-pol).
        assert_eq!(vol.sweeps.len(), before + 4);
        for m in [
            Moment::Vil,
            Moment::EchoTops,
            Moment::Mesh,
            Moment::SnowRate,
        ] {
            assert!(vol.meta.moments.contains(&m), "{m:?} missing");
            assert!(vol.sweeps.iter().any(|s| s.moment(m).is_some()));
        }
        assert!(
            vol.sweeps
                .iter()
                .all(|s| s.moment(Moment::Hydrometeor).is_none())
        );
        attach_derived(&mut vol); // second call must not duplicate
        assert_eq!(vol.sweeps.len(), before + 4);

        // With dual-pol: the winter set joins.
        let mut dual = volume_with_dual(40.0, &[0.5, 1.5, 2.5], 90, 100, 0.5, 0.99);
        let before = dual.sweeps.len();
        attach_derived(&mut dual);
        assert_eq!(dual.sweeps.len(), before + 6);
        for m in [
            Moment::Vil,
            Moment::EchoTops,
            Moment::Mesh,
            Moment::SnowRate,
            Moment::Hydrometeor,
            Moment::MeltingLayer,
        ] {
            assert!(dual.meta.moments.contains(&m), "{m:?} missing");
        }
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

    /// A volume whose sweeps also carry constant ZDR/CC on the same grid
    /// (the dual-pol moments ride with REF on the lowest cut).
    fn volume_with_dual(
        dbz: f32,
        elevations: &[f32],
        ray_count: usize,
        gate_count: usize,
        zdr: f32,
        cc: f32,
    ) -> Volume {
        let mut vol = volume_with(dbz, elevations, ray_count, gate_count);
        for sweep in &mut vol.sweeps {
            let cal = MomentCalibration {
                scale: 100.0,
                offset: 0.0,
                threshold: 0.0,
            };
            sweep.moments.push(MomentData {
                kind: Moment::DifferentialReflectivity,
                calibration: cal,
                range_start_m: 0.0,
                gate_size_m: 250.0,
                gate_count,
                ray_count,
                data: vec![(zdr * 100.0).round() as i16; ray_count * gate_count],
            });
            sweep.moments.push(MomentData {
                kind: Moment::CorrelationCoefficient,
                calibration: cal,
                range_start_m: 0.0,
                gate_size_m: 250.0,
                gate_count,
                ray_count,
                data: vec![(cc * 100.0).round() as i16; ray_count * gate_count],
            });
        }
        vol.meta.moments.push(Moment::DifferentialReflectivity);
        vol.meta.moments.push(Moment::CorrelationCoefficient);
        vol
    }

    /// One sweep, 1 ray, `gates` gates, with per-gate Z/ZDR/CC/KDP — the
    /// classifier's whole input set.
    fn dual_pol_volume(z: &[f32], zdr: &[f32], cc: &[f32], kdp: &[f32]) -> Volume {
        let gates = z.len();
        let cal = MomentCalibration {
            scale: 1.0,
            offset: 2.0,
            threshold: 0.0,
        };
        let with_cal = |kind, calibration, values: &[f32]| MomentData {
            kind,
            calibration,
            range_start_m: 0.0,
            gate_size_m: 250.0,
            gate_count: gates,
            ray_count: 1,
            data: values
                .iter()
                .map(|v| (v * calibration.scale + calibration.offset).round() as i16)
                .collect(),
        };
        let fine = MomentCalibration {
            scale: 100.0,
            offset: 0.0,
            threshold: 0.0,
        };
        Volume {
            meta: VolumeMeta {
                site_id: "KTWX".into(),
                volume_start: chrono::Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap(),
                vcp: Some(212),
                sequence_number: Some(1),
                sweep_count: 1,
                moments: vec![Moment::Reflectivity, Moment::DifferentialReflectivity],
            },
            sweeps: vec![Sweep {
                elevation_deg: 0.5,
                elevation_measured_deg: 0.5,
                azimuth_width_deg: 1.0,
                azimuths_deg: vec![0.0],
                moments: vec![
                    with_cal(Moment::Reflectivity, cal, z),
                    with_cal(Moment::DifferentialReflectivity, fine, zdr),
                    with_cal(Moment::CorrelationCoefficient, fine, cc),
                    with_cal(Moment::SpecificDifferentialPhase, fine, kdp),
                ],
            }],
        }
    }

    /// The winter classifier over the documented decision tree: each gate is
    /// one class on purpose.
    #[test]
    fn hydrometeor_classes_follow_the_winter_tree() {
        let z = [30.0, 45.0, 15.0, 20.0, 20.0, 5.0, 50.0, 20.0];
        let zdr = [1.5, 1.5, 0.2, 0.6, 0.5, 0.1, 0.5, 2.0];
        let cc = [0.99, 0.99, 0.99, 0.95, 0.90, 0.99, 0.95, 0.60];
        let kdp = [0.2, 2.0, 0.0, 0.1, 0.1, 0.0, 0.1, 0.1];
        let vol = dual_pol_volume(&z, &zdr, &cc, &kdp);
        let hca = hydrometeor_sweep(&vol).expect("HCA sweep");
        let md = &hca.moments[0];
        assert_eq!(md.kind, Moment::Hydrometeor);
        let expect = [
            HydrometeorClass::Rain,
            HydrometeorClass::HeavyRain,
            HydrometeorClass::DrySnow,
            HydrometeorClass::WetSnow,
            HydrometeorClass::Mixed,
            HydrometeorClass::IceCrystals,
            HydrometeorClass::GraupelHail,
            HydrometeorClass::Clutter,
        ];
        for (g, class) in expect.into_iter().enumerate() {
            let value = md.value(0, g).expect("classified");
            assert_eq!(
                crate::winter::HydrometeorClass::from_value(value),
                Some(class),
                "gate {g}: z {} zdr {} cc {}",
                z[g],
                zdr[g],
                cc[g]
            );
            // And the display label resolves, not a bare number.
            assert!(Moment::Hydrometeor.value_label(value).is_some());
        }
        // No dual-pol, no classification.
        let plain = volume_with(20.0, &[0.5], 8, 8);
        assert!(hydrometeor_sweep(&plain).is_none());
    }

    /// Snow rate is anchored where the relation says: 20 dBZ ≈ 0.5 in/h,
    /// 30 dBZ ≈ 1.5 in/h, masked for noise and for rain/hail.
    #[test]
    fn snow_rate_follows_the_winter_z_s_relation() {
        let mut vol = volume_with(20.0, &[0.5], 1, 4);
        let cal = vol.sweeps[0].moments[0].calibration;
        vol.sweeps[0].moments[0].data = [20.0, 30.0, 3.0, 45.0]
            .iter()
            .map(|z| (z * cal.scale + cal.offset).round() as i16)
            .collect();
        let snow = snow_rate_sweep(&vol).expect("snow rate");
        assert_eq!(snow.moments[0].kind, Moment::SnowRate);
        let at = |g| snow.moments[0].value(0, g);
        let (s20, s30) = (at(0).unwrap(), at(1).unwrap());
        assert!((s20 - 0.50).abs() < 0.06, "20 dBZ → {s20}");
        assert!((s30 - 1.50).abs() < 0.12, "30 dBZ → {s30}");
        assert!(at(2).is_none(), "3 dBZ is noise, not snow");
        assert!(at(3).is_none(), "45 dBZ is rain/hail, not snow");
    }

    /// The melting layer is the height of the CC dip: a synthetic column with
    /// clear air below, the dip at the 2.5° cut, clear air above.
    #[test]
    fn melting_layer_finds_the_cc_dip() {
        let mut vol = volume_with(20.0, &[0.5, 2.5, 4.5], 1, 200);
        // CC on each cut: 0.99 / 0.90 / 0.99 at the same grid.
        for (sweep, cc) in vol.sweeps.iter_mut().zip([0.99f32, 0.90, 0.99]) {
            let fine = MomentCalibration {
                scale: 100.0,
                offset: 0.0,
                threshold: 0.0,
            };
            sweep.moments.push(MomentData {
                kind: Moment::CorrelationCoefficient,
                calibration: fine,
                range_start_m: 0.0,
                gate_size_m: 250.0,
                gate_count: 200,
                ray_count: 1,
                data: vec![(cc * 100.0).round() as i16; 200],
            });
        }
        let melt = melting_layer_sweep(&vol).expect("melting layer");
        let slant = 119.0 * 0.25; // gate 119's centre
        let expect = h(slant, 2.5) * KFT_PER_KM;
        let got = melt.moments[0].value(0, 119).expect("a height");
        assert!((got - expect).abs() < 0.05, "melt {got} vs {expect}");
        // No dual-pol, no melting layer.
        let plain = volume_with(20.0, &[0.5, 1.5, 2.5], 8, 8);
        assert!(melting_layer_sweep(&plain).is_none());
        // A column with no CC dip has no melting layer either.
        let mut clear = vol.clone();
        for sweep in &mut clear.sweeps {
            if let Some(md) = sweep
                .moments
                .iter_mut()
                .find(|m| m.kind == Moment::CorrelationCoefficient)
            {
                md.data.iter_mut().for_each(|d| *d = 99);
            }
        }
        let none = melting_layer_sweep(&clear).expect("a sweep, no heights");
        assert!(none.moments[0].value(0, 119).is_none());
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
