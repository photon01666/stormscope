//! Vertical cross-section (RHI) geometry and the 3D data probe.
//!
//! Both are thin geometry layers over [`radar_core::sampling`]:
//!
//! * [`build_cross_section`] cuts a vertical curtain through the volume
//!   along one azimuth — the RHI view operators read for storm structure
//!   (overhang, BWER, hail core depth). One prepared [`Profile`] serves the
//!   whole plane, so the grid is cheap enough to rebuild while the user
//!   drags the azimuth slider.
//! * [`probe_ray`] marches a camera ray through the volume and reports the
//!   first echo and the strongest echo along it — the "what is under the
//!   cursor" readout, in the exact units the pane speaks.
//!
//! Vertices are [`MeshVertex`]es in the same scene frame as the volume mesh
//! (km ENU, Z beam height), so the existing wgpu pipeline draws and shades
//! them with no new shader: the curtain inherits the height exaggeration,
//! the echo threshold and the altitude slab from the pane's uniforms.

use std::ops::Range;

use radar_core::sampling::VolumeSampler;
use radar_core::{Moment, Volume};

use crate::colormap::ColorTable;
use crate::volume3d::MeshVertex;

/// Cross-section curtain parameters. Defaults suit a storm-scale RHI: 1 km
/// columns, 0.35 km rows, up to 20 km (65 kft) of height.
#[derive(Clone, Copy, Debug)]
pub struct SectionParams {
    /// Azimuth the plane runs along, degrees clockwise from north.
    pub azimuth_deg: f32,
    /// Outward distance from the radar (km).
    pub max_range_km: f32,
    /// Height of the top of the grid (km).
    pub top_km: f32,
    /// Horizontal sample spacing (km).
    pub range_step_km: f32,
    /// Vertical sample spacing (km).
    pub height_step_km: f32,
}

impl Default for SectionParams {
    fn default() -> Self {
        Self {
            azimuth_deg: 90.0,
            max_range_km: 230.0,
            top_km: 20.0,
            range_step_km: 1.0,
            height_step_km: 0.35,
        }
    }
}

/// Premultiplied table color for a value (transparent when out of table or
/// not finite) — the same convention the volume mesh uses.
fn vertex_color(table: &ColorTable, value: f32) -> [u8; 4] {
    if !value.is_finite() {
        return [0, 0, 0, 0];
    }
    let c = table.map(value);
    let a = u32::from(c[3]);
    [
        ((u32::from(c[0]) * a + 127) / 255) as u8,
        ((u32::from(c[1]) * a + 127) / 255) as u8,
        ((u32::from(c[2]) * a + 127) / 255) as u8,
        c[3],
    ]
}

/// Build the cross-section along `params.azimuth_deg` as a triangle mesh.
///
/// Quads are emitted only where all four corners carry data, so gaps and the
/// unsampled wedges above/below the beam coverage stay transparent — an RHI
/// silhouette rather than a filled rectangle.
pub fn build_cross_section(
    volume: &Volume,
    moment: Moment,
    params: &SectionParams,
) -> (Vec<MeshVertex>, Vec<u32>) {
    let n_r =
        ((params.max_range_km / params.range_step_km.max(0.05)).ceil() as usize + 1).clamp(2, 1024);
    let n_h = ((params.top_km / params.height_step_km.max(0.02)).ceil() as usize + 1).clamp(2, 256);
    let table = ColorTable::for_moment(moment);
    let sampler = VolumeSampler::new(volume, moment);
    let profile = sampler.profile(params.azimuth_deg);
    let az = params.azimuth_deg.to_radians();
    let (sin_a, cos_a) = az.sin_cos();
    // The curtain is a plane through the site: one constant surface normal.
    let normal = [cos_a, -sin_a, 0.0];

    let mut vertices = Vec::with_capacity(n_r * n_h);
    for h in 0..n_h {
        let z = h as f32 * params.height_step_km;
        for r in 0..n_r {
            let ground = r as f32 * params.range_step_km;
            let value = profile.sample(ground, z);
            vertices.push(MeshVertex {
                pos_km: [ground * sin_a, ground * cos_a, z],
                normal,
                color: value.map_or([0, 0, 0, 0], |v| vertex_color(&table, v)),
                value: value.unwrap_or(f32::NAN),
            });
        }
    }

    let mut indices = Vec::new();
    for h in 0..n_h - 1 {
        for r in 0..n_r - 1 {
            let v00 = (h * n_r + r) as u32;
            let v01 = v00 + 1;
            let v10 = v00 + n_r as u32;
            let v11 = v10 + 1;
            let present = [v00, v01, v10, v11]
                .iter()
                .all(|&i| vertices[i as usize].value.is_finite());
            if present {
                indices.extend_from_slice(&[v00, v01, v11, v00, v11, v10]);
            }
        }
    }
    (vertices, indices)
}

/// One hit along the probe ray: the physical value and where it sits.
#[derive(Clone, Copy, Debug)]
pub struct ProbeHit {
    pub value: f32,
    /// Scene position (km ENU; Z = beam height).
    pub pos_km: [f32; 3],
    /// Ground distance from the radar (km).
    pub range_km: f32,
    /// Bearing from the radar, degrees clockwise from north.
    pub bearing_deg: f32,
}

impl ProbeHit {
    fn at(value: f32, pos_km: [f32; 3]) -> Self {
        let (dx, dy) = (pos_km[0], pos_km[1]);
        Self {
            value,
            pos_km,
            range_km: dx.hypot(dy),
            bearing_deg: dx.atan2(dy).to_degrees().rem_euclid(360.0),
        }
    }
}

/// What the probe found along one ray.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProbeReport {
    /// First point along the ray whose value is at or above `threshold`.
    pub first: Option<ProbeHit>,
    /// Strongest sample along the ray (any echo, threshold or not).
    pub max: Option<ProbeHit>,
}

/// March `origin + t·dir` through the volume and report what is there.
///
/// The step grows with distance (0.25 km near the radar, 4 km at long range)
/// so a ray across the whole display costs a few hundred samples; the first
/// threshold crossing is then bisected to the beam edge.
pub fn probe_ray(
    sampler: &VolumeSampler<'_>,
    origin: [f32; 3],
    dir: [f32; 3],
    t_max_km: f32,
    threshold: f32,
) -> ProbeReport {
    let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
    if !len.is_finite() || len <= 1.0e-6 || !t_max_km.is_finite() || t_max_km <= 0.0 {
        return ProbeReport::default();
    }
    let d = [dir[0] / len, dir[1] / len, dir[2] / len];
    let at = |t: f32| {
        [
            origin[0] + d[0] * t,
            origin[1] + d[1] * t,
            origin[2] + d[2] * t,
        ]
    };

    let mut report = ProbeReport::default();
    let mut t = 0.0f32;
    let mut prev_t = 0.0f32;
    while t <= t_max_km {
        let p = at(t);
        if let Some(v) = sampler.sample(p[0], p[1], p[2]) {
            if report.first.is_none() && v >= threshold {
                // Refine the entry edge between the last empty sample
                // and here (8 bisections ≈ 3% of the step).
                let (mut lo, mut hi) = (prev_t, t);
                for _ in 0..8 {
                    let mid = 0.5 * (lo + hi);
                    let pm = at(mid);
                    if sampler.sample(pm[0], pm[1], pm[2]).is_some() {
                        hi = mid;
                    } else {
                        lo = mid;
                    }
                }
                let pe = at(hi);
                let ve = sampler.sample(pe[0], pe[1], pe[2]).unwrap_or(v);
                report.first = Some(ProbeHit::at(ve, pe));
            }
            if report.max.is_none_or(|m| v > m.value) {
                report.max = Some(ProbeHit::at(v, p));
            }
        }
        prev_t = t;
        let step = (t * 0.01).clamp(0.25, 4.0);
        t += step;
    }
    report
}

/// Triangle index count of a mesh, for telemetry lines.
pub fn triangle_ranges(ranges: &[Range<u32>]) -> usize {
    ranges.iter().map(|r| (r.end - r.start) as usize / 3).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use radar_core::{MomentCalibration, MomentData, Sweep, VolumeMeta};

    fn test_volume() -> Volume {
        let cal = MomentCalibration {
            scale: 1.0,
            offset: 2.0,
            threshold: 0.0,
        };
        let mk = |elev: f32, dbz: f32| {
            let rays: Vec<f32> = (0..360).map(|i| i as f32).collect();
            let mut data = vec![0i16; 360 * 400];
            for r in 0..360 {
                for g in 0..400 {
                    data[r * 400 + g] = (dbz + 2.0) as i16;
                }
            }
            Sweep {
                elevation_deg: elev,
                elevation_measured_deg: elev,
                azimuth_width_deg: 1.0,
                azimuths_deg: rays,
                moments: vec![MomentData {
                    kind: Moment::Reflectivity,
                    calibration: cal,
                    range_start_m: 0.0,
                    gate_size_m: 250.0,
                    gate_count: 400,
                    ray_count: 360,
                    data,
                }],
            }
        };
        Volume {
            meta: VolumeMeta {
                site_id: "KTWX".into(),
                volume_start: chrono::Utc.with_ymd_and_hms(2026, 10, 18, 0, 0, 0).unwrap(),
                vcp: None,
                sequence_number: None,
                sweep_count: 3,
                moments: vec![Moment::Reflectivity],
            },
            sweeps: vec![mk(0.5, 30.0), mk(1.5, 40.0), mk(2.5, 50.0)],
        }
    }

    /// The curtain spans its grid, carries the moment's values, and skips
    /// quads that straddle no-data (the wedge below the lowest beam).
    #[test]
    fn cross_section_covers_the_plane_and_honors_gaps() {
        let vol = test_volume();
        let p = SectionParams {
            azimuth_deg: 90.0,
            max_range_km: 60.0,
            top_km: 3.0,
            range_step_km: 1.0,
            height_step_km: 0.5,
        };
        let (verts, idx) = build_cross_section(&vol, Moment::Reflectivity, &p);
        assert_eq!(verts.len(), 61 * 7);
        assert!(!idx.is_empty());
        // East plane: positions go +x, y stays 0, z is the row.
        let east = verts.iter().find(|v| v.value.is_finite()).unwrap();
        assert!(east.pos_km[0] > 0.0 && east.pos_km[1].abs() < 1.0e-4);
        // Down-range corners at low height are below the 0.5 deg beam: no
        // data, transparent, never referenced.
        let low_far = verts
            .iter()
            .find(|v| v.pos_km[0] > 40.0 && v.pos_km[2] < 0.2);
        if let Some(v) = low_far {
            assert!(!v.value.is_finite(), "below-beam corner should be empty");
            assert_eq!(v.color, [0, 0, 0, 0]);
        }
        for &i in &idx {
            assert!(verts[i as usize].value.is_finite());
        }
        // The 50 dBZ top cut shows up somewhere on the curtain.
        let table = ColorTable::for_moment(Moment::Reflectivity);
        let want = table.map(50.0);
        assert!(
            verts
                .iter()
                .any(|v| (v.value - 50.0).abs() < 1.0e-3 && v.color[3] == want[3]),
            "no 50 dBZ vertex on the curtain"
        );
    }

    /// The probe finds the first threshold crossing and the strongest echo
    /// along a ray, with bearings and ranges that make sense.
    #[test]
    fn probe_reports_first_and_max_echo() {
        let vol = test_volume();
        let sampler = VolumeSampler::new(&vol, Moment::Reflectivity);
        // A ray due east climbing gently (from 1 km at the radar to ~2.6 km
        // at 60 km): it threads the 0.5-2.5 deg beam stack out past 55 km.
        let origin = [-20.0f32, 0.0, 1.0];
        let dir = [1.0f32, 0.0, 0.02];
        let report = probe_ray(&sampler, origin, dir, 400.0, 20.0);
        let first = report.first.expect("first echo");
        assert!(
            first.range_km > 40.0 && first.range_km < 80.0,
            "first echo at {first:?}"
        );
        assert!((first.bearing_deg - 90.0).abs() < 1.0, "{first:?}");
        let max = report.max.expect("max echo");
        assert!(max.value >= first.value);
        assert!(max.value >= 49.0, "expected the 50 dBZ cut: {max:?}");
        // A ray that never touches the volume reports nothing.
        let none = probe_ray(&sampler, [0.0, 0.0, -50.0], [0.0, 1.0, 0.0], 400.0, 20.0);
        assert!(none.first.is_none() && none.max.is_none());
    }
}
