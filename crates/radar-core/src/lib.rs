//! radar-core — shared data model for the stormscope radar application.
//!
//! This crate is the contract between the decoder (`radar-decode`), the
//! network/data layer (`radar-data`), the renderer (`radar-render`) and the
//! application shell (`stormscope-app`). Keep it dependency-light and
//! semantically stable; extend additively.

pub mod sites;
pub mod vcp;
pub mod warnings;

use chrono::{DateTime, Utc};

/// Radar site selected on first launch (Topeka, KS).
pub const DEFAULT_SITE: &str = "KTWX";

/// A single radar data moment (equivalent to an L2 "data block" or an L3
/// product family).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Moment {
    /// Base reflectivity (dBZ) — REF / CZ blocks, N0Q/N0Z-style products.
    Reflectivity,
    /// Base radial velocity (m/s) — VEL / VR blocks, N0U/N0S.
    Velocity,
    /// Spectrum width (m/s) — SW / SV blocks.
    SpectrumWidth,
    /// Differential reflectivity (dB) — DR/ZDR blocks, N0X.
    DifferentialReflectivity,
    /// Raw differential phase (degrees) — DP/PHI blocks, N0P.
    DifferentialPhase,
    /// Specific differential phase (deg/km) — KD blocks, N0K.
    SpecificDifferentialPhase,
    /// Correlation coefficient (unitless 0..1) — CC/RF blocks, N0C.
    CorrelationCoefficient,
    /// Normalized vertical power / hydrometeor-ish extras are out of scope
    /// for now; extend as needed.
    CompositeReflectivity,
}

impl Moment {
    /// Short human label used in UI selectors ("REF", "VEL", "SW", ...).
    pub fn label(&self) -> &'static str {
        match self {
            Moment::Reflectivity => "REF",
            Moment::Velocity => "VEL",
            Moment::SpectrumWidth => "SW",
            Moment::DifferentialReflectivity => "ZDR",
            Moment::DifferentialPhase => "PHI",
            Moment::SpecificDifferentialPhase => "KDP",
            Moment::CorrelationCoefficient => "CC",
            Moment::CompositeReflectivity => "COMP",
        }
    }

    /// Physical unit for display ("dBZ", "m/s", "", ...).
    pub fn unit(&self) -> &'static str {
        match self {
            Moment::Reflectivity => "dBZ",
            Moment::Velocity => "m/s",
            Moment::SpectrumWidth => "m/s",
            Moment::DifferentialReflectivity => "dB",
            Moment::DifferentialPhase => "deg",
            Moment::SpecificDifferentialPhase => "deg/km",
            Moment::CorrelationCoefficient => "",
            Moment::CompositeReflectivity => "dBZ",
        }
    }
}

/// Identity + timing metadata for one volume scan.
#[derive(Clone, Debug)]
pub struct VolumeMeta {
    /// Radar site ID, e.g. "KTLX".
    pub site_id: String,
    /// Volume scan start time (UTC).
    pub volume_start: DateTime<Utc>,
    /// Volume Coverage Pattern number when known.
    pub vcp: Option<u16>,
    /// Monotonic ordering aid (L2 sequence number) when known.
    pub sequence_number: Option<u32>,
    /// Number of elevation sweeps in the volume.
    pub sweep_count: usize,
    /// Moments detected in this volume (UI can use to filter selectors).
    pub moments: Vec<Moment>,
}

/// Calibration for one moment's digital numbers:
/// `physical = (dn - offset) / scale`. DN of 0 and values at/below the
/// threshold flag are "no data". See WSR-88D ICD message 31 data header.
#[derive(Copy, Clone, Debug)]
pub struct MomentCalibration {
    pub scale: f32,
    pub offset: f32,
    /// Minimum gate SNR threshold (data is valid only above it).
    pub threshold: f32,
}

impl Default for MomentCalibration {
    fn default() -> Self {
        Self { scale: 1.0, offset: 2.0, threshold: 0.0 }
    }
}

/// Moment data for one elevation sweep, stored ray-major
/// (`data[ray * gate_count + gate]`) as raw signed digital numbers.
/// Physical conversion uses `calibration`; DN 0 means "below threshold /
/// range folded / no data".
#[derive(Clone, Debug)]
pub struct MomentData {
    pub kind: Moment,
    pub calibration: MomentCalibration,
    /// Slant range (meters) from radar to the center of the first gate.
    pub range_start_m: f32,
    /// Gate spacing (meters).
    pub gate_size_m: f32,
    /// Number of range gates per ray (all rays share this in L2).
    pub gate_count: usize,
    /// Number of azimuthal rays.
    pub ray_count: usize,
    /// Ray-major raw data.
    pub data: Vec<i16>,
}

impl MomentData {
    /// Physical value in native units, or `None` when the gate has no data.
    pub fn value(&self, ray: usize, gate: usize) -> Option<f32> {
        if ray >= self.ray_count || gate >= self.gate_count {
            return None;
        }
        let dn = self.data[ray * self.gate_count + gate];
        if dn == 0 {
            return None;
        }
        Some((dn as f32 - self.calibration.offset) / self.calibration.scale)
    }

    pub fn raw(&self, ray: usize, gate: usize) -> Option<i16> {
        if ray >= self.ray_count || gate >= self.gate_count {
            return None;
        }
        let dn = self.data[ray * self.gate_count + gate];
        if dn == 0 { None } else { Some(dn) }
    }
}

/// One elevation cut with its moment data blocks.
#[derive(Clone, Debug)]
pub struct Sweep {
    /// Requested/rounded tilt angle in degrees (e.g. 0.5).
    pub elevation_deg: f32,
    /// Measured mean elevation of the cut in degrees.
    pub elevation_measured_deg: f32,
    /// Azimuthal width of one ray in degrees (e.g. 0.5 or 1.0).
    pub azimuth_width_deg: f32,
    /// Per-ray azimuth angles in degrees (0 = north, clockwise). Length may
    /// differ per moment; use each MomentData's ray_count and index here.
    pub azimuths_deg: Vec<f32>,
    /// Available moments for this sweep.
    pub moments: Vec<MomentData>,
}

impl Sweep {
    pub fn moment(&self, kind: Moment) -> Option<&MomentData> {
        self.moments.iter().find(|m| m.kind == kind)
    }
}

/// A full volume scan: metadata + all elevation sweeps.
#[derive(Clone, Debug)]
pub struct Volume {
    pub meta: VolumeMeta,
    pub sweeps: Vec<Sweep>,
}

impl Volume {
    /// Nearest sweep index to the given tilt angle.
    pub fn sweep_index_for_tilt(&self, tilt_deg: f32) -> Option<usize> {
        self.sweeps
            .iter()
            .enumerate()
            .min_by(|a, b| {
                (a.1.elevation_deg - tilt_deg)
                    .abs()
                    .partial_cmp(&(b.1.elevation_deg - tilt_deg))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
    }

    /// Index of the sweep closest to `tilt_deg` **that also contains
    /// `kind`**. NEXRAD splits cuts: REF+ZDR+PHI+CC and REF+VEL+SW live on
    /// *separate sweeps at the same nominal tilt*, so a plain tilt lookup can
    /// land on a sweep without the requested moment (selecting VEL on the
    /// tilt-nearest sweep rendered nothing). Falls back to the plain tilt
    /// match only when no sweep at all carries the moment.
    pub fn sweep_index_for_tilt_moment(&self, tilt_deg: f32, kind: Moment) -> Option<usize> {
        let with_moment = self
            .sweeps
            .iter()
            .enumerate()
            .filter(|(_, s)| s.moment(kind).is_some())
            .min_by(|a, b| {
                (a.1.elevation_deg - tilt_deg)
                    .abs()
                    .partial_cmp(&(b.1.elevation_deg - tilt_deg))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i);
        with_moment.or_else(|| self.sweep_index_for_tilt(tilt_deg))
    }

    /// Distinct sorted tilt angles present in the volume.
    pub fn tilts(&self) -> Vec<f32> {
        let mut v: Vec<f32> = self.sweeps.iter().map(|s| s.elevation_deg).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v.dedup_by(|a, b| (*a - *b).abs() < 0.05);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sweep(elev: f32, moments: &[Moment]) -> Sweep {
        Sweep {
            elevation_deg: elev,
            elevation_measured_deg: elev,
            azimuth_width_deg: 1.0,
            azimuths_deg: (0..360).map(|i| i as f32).collect(),
            moments: moments
                .iter()
                .map(|&k| MomentData {
                    kind: k,
                    calibration: MomentCalibration::default(),
                    range_start_m: 500.0,
                    gate_size_m: 250.0,
                    gate_count: 4,
                    ray_count: 360,
                    data: vec![0; 360 * 4],
                })
                .collect(),
        }
    }

    fn vol(sweeps: Vec<Sweep>) -> Volume {
        let n = sweeps.len();
        Volume {
            meta: VolumeMeta {
                site_id: "KTEST".into(),
                volume_start: Utc::now(),
                vcp: None,
                sequence_number: None,
                sweep_count: n,
                moments: vec![],
            },
            sweeps,
        }
    }

    /// NEXRAD splits cuts: REF+pol and REF+VEL live on separate sweeps at the
    /// same nominal tilt. Selecting VEL must find the second 0.5° cut, not
    /// the tilt-nearest cut that lacks velocity (that bug rendered nothing).
    #[test]
    fn split_cut_moment_lookup() {
        let v = vol(vec![
            sweep(0.5, &[Moment::Reflectivity, Moment::CorrelationCoefficient]),
            sweep(
                0.5,
                &[Moment::Reflectivity, Moment::Velocity, Moment::SpectrumWidth],
            ),
            sweep(0.9, &[Moment::Reflectivity]),
        ]);
        // VEL lives on the second 0.5° cut, though sweep 0 is tilt-nearest.
        assert_eq!(v.sweep_index_for_tilt_moment(0.5, Moment::Velocity), Some(1));
        // REF prefers the first 0.5° cut.
        assert_eq!(
            v.sweep_index_for_tilt_moment(0.5, Moment::Reflectivity),
            Some(0)
        );
        // No 0.9 VEL cut in this synthetic volume: fall back to the nearest
        // sweep that does carry velocity instead of rendering nothing.
        assert_eq!(v.sweep_index_for_tilt_moment(0.9, Moment::Velocity), Some(1));
    }
}
