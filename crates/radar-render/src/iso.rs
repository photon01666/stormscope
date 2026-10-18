//! Isosurfaces (echo shells) built from a volume by marching tetrahedra.
//!
//! A radar is a polar sampling grid, so the isosurface is extracted directly
//! in polar space — azimuth columns (one prepared [`Profile`] each), range
//! rows and height levels — and the vertices are placed in the scene's
//! Cartesian frame afterwards. Cells are cubes in that lattice; each cube is
//! split into six tetrahedra along its main diagonal (Kuhn's decomposition:
//! the same split in every cell, so the surface is crack-free) and each
//! tetrahedron is contoured analytically.
//!
//! Missing data (above the beam coverage, beyond range, no-echo gaps) is
//! `NaN`; any cube touching it is skipped, so a shell is open where the
//! radar genuinely did not measure — the honest shape, not an extrapolated
//! bubble. Vertices are deduplicated per shell by their generating edge, and
//! normals come from the accumulated triangle faces, so shells shade like
//! the rest of the volume.

use std::collections::HashMap;
use std::ops::Range;

use radar_core::sampling::VolumeSampler;
use radar_core::{Moment, Volume};

use crate::colormap::ColorTable;
use crate::volume3d::{MeshDetail, MeshVertex};

/// Grid density of the isosurface extraction, plus the shell ceiling.
#[derive(Clone, Copy, Debug)]
pub struct IsoParams {
    /// Azimuth/range spacing preset (the height step is separate).
    pub detail: MeshDetail,
    /// Top of the sampled grid (km). 21 km ≈ 69 kft: above every real echo.
    pub top_km: f32,
    /// Vertical sample spacing (km).
    pub height_step_km: f32,
}

impl Default for IsoParams {
    fn default() -> Self {
        Self {
            detail: MeshDetail::Normal,
            top_km: 21.0,
            height_step_km: 0.7,
        }
    }
}

/// Shells extracted from one volume: one shared vertex/index pair plus the
/// index range of each requested threshold.
pub struct IsoMesh {
    pub vertices: Vec<MeshVertex>,
    pub indices: Vec<u32>,
    /// `(threshold, index range)` in request order (thresholds nothing
    /// crosses are absent — no shell was extracted).
    pub shells: Vec<(f32, Range<u32>)>,
}

impl IsoMesh {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

/// Hard ceiling on triangles per shell: a broken threshold setting must not
/// be able to eat the GPU memory budget (a real shell is well under 200k).
const MAX_SHELL_TRIS: usize = 400_000;

/// Kuhn's decomposition of a cube (corner index = x + 2y + 4z) into six
/// tetrahedra along the 0–7 diagonal. The same split in every cell ⇒
/// adjacent cube faces are divided identically, so the surface has no cracks.
const TETS: [[usize; 4]; 6] = [
    [0, 1, 3, 7],
    [0, 1, 5, 7],
    [0, 2, 3, 7],
    [0, 2, 6, 7],
    [0, 4, 5, 7],
    [0, 4, 6, 7],
];

fn premultiplied(table: &ColorTable, value: f32) -> [u8; 4] {
    let c = table.map(value);
    let a = u32::from(c[3]);
    [
        ((u32::from(c[0]) * a + 127) / 255) as u8,
        ((u32::from(c[1]) * a + 127) / 255) as u8,
        ((u32::from(c[2]) * a + 127) / 255) as u8,
        c[3],
    ]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Extract one threshold's shell into the shared mesh.
#[allow(clippy::too_many_arguments)]
fn extract_shell(
    values: &[f32],
    dims: (usize, usize, usize),
    az_step_deg: f32,
    range_step_km: f32,
    height_step_km: f32,
    threshold: f32,
    table: &ColorTable,
    mesh: &mut IsoMesh,
) {
    let (n_az, n_r, n_h) = dims;
    let node = |i: usize, j: usize, k: usize| ((i % n_az) * n_r + j) * n_h + k;
    let node_pos = |i: usize, j: usize, k: usize| -> [f32; 3] {
        let az = (i % n_az) as f32 * az_step_deg;
        let (sin_a, cos_a) = az.to_radians().sin_cos();
        let ground = j as f32 * range_step_km;
        [ground * sin_a, ground * cos_a, k as f32 * height_step_km]
    };

    let start_index = mesh.indices.len() as u32;
    let start_vertex = mesh.vertices.len();
    let color = premultiplied(table, threshold);
    // Edge (node pair) → vertex index, so neighboring cells share vertices.
    let mut edge_verts: HashMap<(u32, u32), u32> = HashMap::new();
    let mut tris = 0usize;
    // Cube corners in bit order (index = x + 2y + 4z): lattice coordinates,
    // node index and value.
    let mut corners = [(0usize, 0usize, 0usize); 8];
    let mut corner_nodes = [0usize; 8];
    let mut corner_values = [0.0f32; 8];

    'az: for i in 0..n_az {
        for j in 0..n_r - 1 {
            for k in 0..n_h - 1 {
                let mut min = f32::INFINITY;
                let mut max = f32::NEG_INFINITY;
                let mut gap = false;
                for c in 0..8 {
                    let (di, dj, dk) = (c & 1, (c >> 1) & 1, (c >> 2) & 1);
                    let coords = (i + di, j + dj, k + dk);
                    let n = node(coords.0, coords.1, coords.2);
                    corners[c] = coords;
                    corner_nodes[c] = n;
                    let v = values[n];
                    corner_values[c] = v;
                    if !v.is_finite() {
                        gap = true; // no measurement in this cube: no surface
                        break;
                    }
                    min = min.min(v);
                    max = max.max(v);
                }
                if gap || !(min < threshold && threshold <= max) {
                    continue;
                }
                for tet in TETS {
                    let mut inside = [usize::MAX; 4];
                    let mut outside = [usize::MAX; 4];
                    let (mut n_in, mut n_out) = (0usize, 0usize);
                    for (idx, &c) in tet.iter().enumerate() {
                        if corner_values[c] >= threshold {
                            inside[n_in] = idx;
                            n_in += 1;
                        } else {
                            outside[n_out] = idx;
                            n_out += 1;
                        }
                    }
                    if n_in == 0 || n_in == 4 {
                        continue;
                    }

                    // Vertex on the edge between tet corners `a` and `b`,
                    // deduplicated by the lattice nodes it spans.
                    let mut vertex_on = |a: usize, b: usize| -> u32 {
                        let (na, nb) = (corner_nodes[tet[a]], corner_nodes[tet[b]]);
                        let (lo, hi) = if na <= nb { (na, nb) } else { (nb, na) };
                        *edge_verts.entry((lo as u32, hi as u32)).or_insert_with(|| {
                            let va = corner_values[tet[a]];
                            let vb = corner_values[tet[b]];
                            let t = ((threshold - va) / (vb - va)).clamp(0.0, 1.0);
                            let ca = corners[tet[a]];
                            let cb = corners[tet[b]];
                            let pa = node_pos(ca.0, ca.1, ca.2);
                            let pb = node_pos(cb.0, cb.1, cb.2);
                            let idx = mesh.vertices.len() as u32;
                            mesh.vertices.push(MeshVertex {
                                pos_km: [
                                    pa[0] + (pb[0] - pa[0]) * t,
                                    pa[1] + (pb[1] - pa[1]) * t,
                                    pa[2] + (pb[2] - pa[2]) * t,
                                ],
                                normal: [0.0, 0.0, 1.0],
                                color,
                                value: threshold,
                            });
                            idx
                        })
                    };

                    // Emit the cut polygon: one triangle for a 1/3 case, a
                    // quad for the 2/2 case (edges in the tet's local order).
                    if n_in == 1 || n_in == 3 {
                        let (p, q, r) = if n_in == 1 {
                            (inside[0], outside[0], outside[1])
                        } else {
                            (outside[0], inside[0], inside[1])
                        };
                        let s = if n_in == 1 { outside[2] } else { inside[2] };
                        let (v0, v1, v2) = (vertex_on(p, q), vertex_on(p, r), vertex_on(p, s));
                        mesh.indices.extend_from_slice(&[v0, v1, v2]);
                        tris += 1;
                    } else {
                        let (i0, i1) = (inside[0], inside[1]);
                        let (o0, o1) = (outside[0], outside[1]);
                        let (e00, e01) = (vertex_on(i0, o0), vertex_on(i0, o1));
                        let (e10, e11) = (vertex_on(i1, o0), vertex_on(i1, o1));
                        // Quad (e00, e01, e11, e10) in two triangles sharing
                        // the diagonal e01–e10.
                        mesh.indices
                            .extend_from_slice(&[e00, e01, e10, e01, e11, e10]);
                        tris += 2;
                    }
                    if tris > MAX_SHELL_TRIS {
                        break 'az;
                    }
                }
            }
        }
    }

    // Normals from the faces (orientation-independent: the shader shades
    // with |dot| and the shells are seen from both sides).
    accumulate_normals(
        &mut mesh.vertices,
        start_vertex,
        &mesh.indices[start_index as usize..],
    );
    if mesh.indices.len() as u32 > start_index {
        mesh.shells
            .push((threshold, start_index..mesh.indices.len() as u32));
    }
}

/// Accumulate face normals onto every vertex created since `start_vertex`,
/// then normalize.
fn accumulate_normals(vertices: &mut [MeshVertex], start_vertex: usize, indices: &[u32]) {
    for i in indices.chunks(3) {
        if i.len() < 3 {
            break;
        }
        let (a, b, c) = (i[0] as usize, i[1] as usize, i[2] as usize);
        let (pa, pb, pc) = (vertices[a].pos_km, vertices[b].pos_km, vertices[c].pos_km);
        let n = cross(
            [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]],
            [pc[0] - pa[0], pc[1] - pa[1], pc[2] - pa[2]],
        );
        for idx in [a, b, c] {
            let v = &mut vertices[idx];
            v.normal[0] += n[0];
            v.normal[1] += n[1];
            v.normal[2] += n[2];
        }
    }
    for v in vertices[start_vertex..].iter_mut() {
        let len =
            (v.normal[0] * v.normal[0] + v.normal[1] * v.normal[1] + v.normal[2] * v.normal[2])
                .sqrt();
        if len > 1.0e-9 {
            v.normal = [v.normal[0] / len, v.normal[1] / len, v.normal[2] / len];
        } else {
            v.normal = [0.0, 0.0, 1.0];
        }
    }
}

/// Build every requested shell of `moment` for `volume`.
///
/// Thresholds are in the moment's units (dBZ for reflectivity). Shells that
/// no data crosses are absent from the result.
pub fn build_isosurfaces(
    volume: &Volume,
    moment: Moment,
    thresholds: &[f32],
    params: &IsoParams,
) -> IsoMesh {
    let mut mesh = IsoMesh {
        vertices: Vec::new(),
        indices: Vec::new(),
        shells: Vec::new(),
    };
    if thresholds.is_empty() {
        return mesh;
    }
    let sampler = VolumeSampler::new(volume, moment);
    let max_range = sampler.max_range_km();
    if !max_range.is_finite() || max_range <= 0.5 {
        return mesh;
    }
    let az_step = params.detail.azimuth_step_deg();
    let range_step = params.detail.range_step_km();
    let z_step = params.height_step_km.max(0.1);
    let n_az = (360.0 / az_step).round().max(8.0) as usize;
    let n_r = ((max_range / range_step).ceil() as usize + 1).clamp(2, 512);
    let n_h = ((params.top_km / z_step).ceil() as usize + 1).clamp(2, 128);

    // Sample the polar lattice once; all thresholds read the same field.
    let mut values = vec![f32::NAN; n_az * n_r * n_h];
    for i in 0..n_az {
        let profile = sampler.profile(i as f32 * az_step);
        for j in 0..n_r {
            let ground = j as f32 * range_step;
            let base = (i * n_r + j) * n_h;
            for (k, slot) in values[base..base + n_h].iter_mut().enumerate() {
                if let Some(v) = profile.sample(ground, k as f32 * z_step) {
                    *slot = v;
                }
            }
        }
    }

    let table = ColorTable::for_moment(moment);
    for &threshold in thresholds {
        extract_shell(
            &values,
            (n_az, n_r, n_h),
            az_step,
            range_step,
            z_step,
            threshold,
            &table,
            &mut mesh,
        );
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use radar_core::{MomentCalibration, MomentData, Sweep, VolumeMeta};

    /// A constant 60 dBZ cylinder inside 30 km, 10 dBZ outside — the 50 dBZ
    /// shell is a vertical wall at 30 km wherever the beams sample.
    fn cylinder_volume() -> Volume {
        let cal = MomentCalibration {
            scale: 1.0,
            offset: 2.0,
            threshold: 0.0,
        };
        let mk = |elev: f32| {
            let rays: Vec<f32> = (0..360).map(|i| i as f32).collect();
            let mut data = vec![0i16; 360 * 200]; // 50 km of 250 m gates
            for r in 0..360 {
                for g in 0..200 {
                    let range_km = (g as f32 + 0.5) * 0.25;
                    let dbz = if range_km < 30.0 { 60.0 } else { 10.0 };
                    data[r * 200 + g] = (dbz + 2.0) as i16;
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
                    gate_count: 200,
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
            sweeps: vec![mk(0.5), mk(1.5), mk(2.5)],
        }
    }

    /// The 50 dBZ shell sits where the field crosses 50: a vertical wall at
    /// 30 km, spanning the sampled heights, with unit normals.
    #[test]
    fn isosurface_sits_on_the_threshold_contour() {
        let vol = cylinder_volume();
        let params = IsoParams {
            detail: MeshDetail::Normal,
            top_km: 4.0,
            height_step_km: 0.25,
        };
        let mesh = build_isosurfaces(&vol, Moment::Reflectivity, &[50.0], &params);
        assert_eq!(mesh.shells.len(), 1, "one shell expected");
        assert_eq!(mesh.shells[0].0, 50.0);
        assert!(
            mesh.triangle_count() > 200,
            "got {} tris",
            mesh.triangle_count()
        );
        assert_eq!(mesh.indices.len() % 3, 0);
        for &i in &mesh.indices {
            let v = &mesh.vertices[i as usize];
            let r = v.pos_km[0].hypot(v.pos_km[1]);
            // The contour is the field crossing between the last 60 dBZ gate
            // and the first 10 dBZ one: within one range step of 30 km.
            assert!((r - 30.0).abs() <= 3.1, "off-contour vertex at r = {r}");
            assert!(v.pos_km[2] >= -0.01 && v.pos_km[2] <= 4.01, "{v:?}");
            assert_eq!(v.value, 50.0);
            let n = v.normal[0].hypot(v.normal[1]).hypot(v.normal[2]);
            assert!((n - 1.0).abs() < 1.0e-3, "normal not unit: {n}");
        }
        // The shell spans the sampled heights (it is a wall, not a lid).
        let zs: Vec<f32> = mesh.vertices.iter().map(|v| v.pos_km[2]).collect();
        let (lo, hi) = (
            zs.iter().cloned().fold(f32::INFINITY, f32::min),
            zs.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
        );
        // The wall spans every beam-sampled height at the contour's range.
        assert!(hi - lo > 0.6, "shell too flat: {lo}..{hi}");

        // A threshold the field never reaches produces no shell.
        let empty = build_isosurfaces(&vol, Moment::Reflectivity, &[90.0], &params);
        assert!(empty.shells.is_empty());
        assert_eq!(empty.triangle_count(), 0);

        // Two thresholds: two shells, non-overlapping index ranges.
        let two = build_isosurfaces(&vol, Moment::Reflectivity, &[20.0, 50.0], &params);
        assert_eq!(two.shells.len(), 2);
        assert_eq!(two.shells[0].1.end, two.shells[1].1.start);
    }
}
