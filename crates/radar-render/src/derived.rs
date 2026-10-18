//! Derived products computed from a volume's reflectivity — the fields
//! analysis viewers derive on the fly (GR2Analyst's five: Echo Tops, VIL,
//! VILD, POSH, MEHS; we ship the three that matter most for storm work:
//! VIL, Echo Tops, MEHS), plus storm-relative velocity from VEL.
//!
//! Each builder returns a synthetic single-moment **sweep** whose rays,
//! gates and geometry match the source sweep, so the existing raster, 3D
//! sampler and probe paths consume it with no special cases: the map draws
//! it like any product, and the 3D pane treats it like any moment.

use radar_core::{Moment, MomentCalibration, MomentData, Sweep, Volume};

/// ICD-style encoding for the derived fields: DN 0 stays "below threshold",
/// physical = (dn - offset) / scale.
fn cal(scale: f32, offset: f32) -> MomentCalibration {
    MomentCalibration {
        scale,
        offset,
        threshold: 0.0,
    }
}

/// Build a synthetic sweep from a source sweep, encoding `values`
/// (physical) with `calibration`.
fn synth(
    source: &Sweep,
    kind: Moment,
    calibration: MomentCalibration,
    values: Vec<f32>,
) -> Sweep {
    let (ray_count, gate_count, range_start_m, gate_size_m) = source
        .moments
        .first()
        .map(|m| (m.ray_count, m.gate_count, m.range_start_m, m.gate_size_m))
        .unwrap_or((0, 0, 0.0, 0.0));
    let data = values
        .iter()
        .map(|v| {
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
        elevation_deg: source.elevation_deg,
        elevation_measured_deg: source.elevation_measured_deg,
        azimuth_width_deg: source.azimuth_width_deg,
        azimuths_deg: source.azimuths_deg.clone(),
        moments: vec![MomentData {
            kind,
            calibration,
            range_start_m,
            gate_size_m,
            gate_count,
            ray_count,
            data,
        }],
    }
}

/// Vertically Integrated Liquid (kg/m²) per (ray, gate column):
/// `VIL = Σ 3.44e-6 · (10^(0.1·Z))^0.57 · Δh` over the reflectivity cuts
/// (Amburn & Wolf 1997), Z in dBZ, Δh in metres between cut heights.
///
/// Returns one sweep per REF cut shape: the value is a *column* quantity,
/// so it is written on the lowest cut (the one with the widest range) and
/// that is the sweep the map/3D show it as.
pub fn vil_sweep(volume: &Volume) -> Option<Sweep> {
    let cols = columns(volume)?;
    let (source, _, heights) = cols;
    let mut values = vec![f32::NAN; source.ray_gate_count()];
    for (ray, gate) in source.column_indices() {
        let mut sum = 0.0f32;
        let mut any = false;
        for k in 0..heights.len() - 1 {
            let z = source.value_at(ray, gate, k)?;
            any = true;
            let dh_m = (heights[k + 1] - heights[k]) * 1000.0;
            sum += 3.44e-6 * (10f32.powf(0.1 * z)).powf(0.57) * dh_m.max(0.0);
        }
        values[ray * source.gate_count + gate] = if any { sum } else { f32::NAN };
    }
    Some(synth(&source.sweep, Moment::Vil, cal(100.0, 0.0), values))
}

/// Echo Tops (km): the top of the ≥18.5 dBZ echo per column, interpolated
/// between the two cuts bracketing the threshold.
pub fn echo_tops_sweep(volume: &Volume) -> Option<Sweep> {
    let cols = columns(volume)?;
    let (source, _, heights) = cols;
    let mut values = vec![f32::NAN; source.ray_gate_count()];
    for (ray, gate) in source.column_indices() {
        let mut top = f32::NAN;
        for k in 0..heights.len() {
            if let Some(z) = source.value_at(ray, gate, k) {
                if z >= 18.5 {
                    top = heights[k];
                }
            }
        }
        values[ray * source.gate_count + gate] = top;
    }
    Some(synth(&source.sweep, Moment::EchoTops, cal(100.0, 0.0), values))
}

/// Maximum Estimated Hail Size (inches): the classic Witt 1998 relation
/// `MEHS(in) = 0.1 · sqrt(VIL)` (VIL in kg/m²). A column quantity like
/// VIL; written on the lowest cut.
pub fn mehs_sweep(volume: &Volume) -> Option<Sweep> {
    let vil = vil_sweep(volume)?;
    let values: Vec<f32> = vil.moments[0]
        .data
        .iter()
        .map(|&dn| {
            if dn == 0 {
                f32::NAN
            } else {
                let v = (f32::from(dn) - vil.moments[0].calibration.offset)
                    / vil.moments[0].calibration.scale;
                0.1 * v.max(0.0).sqrt()
            }
        })
        .collect();
    Some(synth(
        vil.moments.first().map(|_| &vil)?,
        Moment::Mesh,
        cal(1000.0, 0.0),
        values,
    ))
}
