//! 3D volume geometry: turn a decoded volume scan into a rotatable scene.
//!
//! A NEXRAD volume is already a 3D measurement — every cut (0.5°, 0.9°, …,
//! 19.5°) samples the same storm from a different beam elevation. The 2D map
//! shows one cut at a time; this module builds the whole volume as triangle
//! meshes so all cuts render at once, each at its true beam height:
//!
//! * Every cut becomes a draped surface over its (azimuth × slant range)
//!   grid. Azimuth rays are binned into evenly spaced columns (averaging the
//!   rays that land in the same bin, which also smooths super-resolution),
//!   and range is sampled on a fixed step — the grid a GPU can chew through
//!   without a decimation pass.
//! * Vertices sit at true 4/3-earth beam heights (`beam_height_km`): a 0.5°
//!   beam is 1.5 km up at 100 km out, and the curvature term keeps far low
//!   cuts from visually dipping into the ground. Vertical exaggeration is a
//!   shader-side multiply, so the slider costs nothing.
//! * Values are colored with the same [`ColorTable`] the 2D rasters use —
//!   the 3D view and the map never disagree about what a color means — and
//!   the physical value rides along per vertex so the fragment shader can
//!   discard below the display threshold without rebuilding geometry.
//! * Gaps (`DN 0` — below threshold / no echo) become *missing* quads: a
//!   quad is emitted only when all four corners carry data. That is what
//!   makes clear-air slices look like structure instead of soup, and it is
//!   also the biggest triangle-budget saver.
//!
//! The camera is a plain orbit rig (`Camera`) with column-major matrices
//! ready for WGSL's `mat4x4<f32>`; `project` lets the UI place screen-space
//! labels (N/E/S/W, range rings) with the exact same transform.

use std::ops::Range;

use radar_core::{Moment, MomentData, Volume};

use super::raster::angle_delta;

use crate::colormap::ColorTable;

/// One vertex of a volume/grid mesh. `repr(C)` + `Pod` so the app can hand
/// it to wgpu without a repack step.
///
/// `color` is **premultiplied** by its own alpha (matching how egui composites
/// textures), and `value` is the physical measurement (dBZ, m/s, …) for the
/// fragment shader's threshold test. Non-data geometry (the ground grid)
/// carries [`NO_THRESHOLD_VALUE`] so it can never be discarded.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MeshVertex {
    pub pos_km: [f32; 3],
    pub normal: [f32; 3],
    pub color: [u8; 4],
    pub value: f32,
}

/// Value used for ground-grid vertices — far above any real measurement, so
/// `value < threshold` is never true for them.
pub const NO_THRESHOLD_VALUE: f32 = 1.0e30;

/// Geometry density preset. Higher density costs vertex memory one-for-one:
/// Normal is ~580k vertices (~19 MB) for a full 14-cut volume before gaps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MeshDetail {
    /// 3° columns, 3 km range rows — for integrated GPUs.
    Coarse,
    /// 2° columns, 2 km range rows — the default.
    Normal,
    /// 1° columns, 2 km range rows — super-resolution azimuth, same rows.
    Fine,
}

impl MeshDetail {
    pub fn azimuth_step_deg(self) -> f32 {
        match self {
            MeshDetail::Coarse => 3.0,
            MeshDetail::Normal => 2.0,
            MeshDetail::Fine => 1.0,
        }
    }

    pub fn range_step_km(self) -> f32 {
        match self {
            MeshDetail::Coarse => 3.0,
            MeshDetail::Normal => 2.0,
            MeshDetail::Fine => 2.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MeshDetail::Coarse => "Coarse",
            MeshDetail::Normal => "Normal",
            MeshDetail::Fine => "Fine",
        }
    }

    pub const ALL: [MeshDetail; 3] = [MeshDetail::Coarse, MeshDetail::Normal, MeshDetail::Fine];
}

/// Kilometres in a statute mile (the 3D pane speaks miles and feet).
pub const KM_PER_MILE: f32 = 1.609_344;
/// Feet in a kilometre (beam heights on screen are feet, like a skew-T).
pub const FT_PER_KM: f32 = 3280.84;

/// What to build: which moment, how dense, and which cuts.
#[derive(Clone, Copy, Debug)]
pub struct MeshFilter {
    pub moment: Moment,
    pub detail: MeshDetail,
    /// Lowest cut elevation to include, degrees (inclusive). `None` = no
    /// lower bound. The 2D map shows one tilt; the 3D scene shows all of
    /// them unless the user narrows the range.
    pub elev_min_deg: Option<f32>,
    /// Highest cut elevation to include, degrees (inclusive).
    pub elev_max_deg: Option<f32>,
}

impl MeshFilter {
    pub fn new(moment: Moment, detail: MeshDetail) -> Self {
        Self {
            moment,
            detail,
            elev_min_deg: None,
            elev_max_deg: None,
        }
    }
}

/// Draw range of one cut inside [`VolumeMesh::indices`].
#[derive(Clone, Debug)]
pub struct TiltDraw {
    pub elevation_deg: f32,
    /// Range into the shared index buffer. Cuts are appended in ascending
    /// elevation so drawing them in order is also back-to-front enough for
    /// translucent blending seen from above.
    pub indices: Range<u32>,
}

/// A whole volume as one vertex/index pair (shared across cuts).
pub struct VolumeMesh {
    pub vertices: Vec<MeshVertex>,
    pub indices: Vec<u32>,
    /// One entry per included cut, ascending elevation.
    pub tilts: Vec<TiltDraw>,
    /// Longest cut range (km) of the included cuts — the ground-grid radius.
    pub max_range_km: f32,
    pub lowest_elev_deg: f32,
    pub highest_elev_deg: f32,
}

impl VolumeMesh {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

/// Standard radar beam height: 4/3-earth effective-radius model, the same
/// approximation the raster path uses for its gates.
///
/// `h = r·sin(θ) + r² / (2·k·R)` with k = 4/3 and R = 6371 km. At 0.5° and
/// 100 km that is 0.87 + 0.59 ≈ 1.46 km — the classic "height of the 0.5°
/// beam at 100 km" number forecasters quote.
pub fn beam_height_km(slant_range_km: f32, elevation_deg: f32) -> f32 {
    const K_RE: f32 = (4.0 / 3.0) * 6371.0;
    let e = elevation_deg.to_radians();
    slant_range_km * e.sin() + (slant_range_km * slant_range_km) / (2.0 * K_RE)
}

/// Build the full scene mesh described by `filter`.
///
/// Cuts that do not carry the moment are skipped (NEXRAD splits REF/VEL into
/// separate cuts at the same nominal tilt); so are cuts outside the filter's
/// elevation range. If nothing qualifies, the result is an empty mesh with
/// `max_range_km == 0`.
pub fn build_volume_mesh(volume: &Volume, filter: &MeshFilter) -> VolumeMesh {
    let moment = filter.moment;
    let detail = filter.detail;
    let table = ColorTable::for_moment(moment);
    let az_step = detail.azimuth_step_deg();
    let range_step = detail.range_step_km();
    let n_az = (360.0 / az_step).round().max(4.0) as usize;

    let mut mesh = VolumeMesh {
        vertices: Vec::new(),
        indices: Vec::new(),
        tilts: Vec::new(),
        max_range_km: 0.0,
        lowest_elev_deg: 0.0,
        highest_elev_deg: 0.0,
    };

    // Ascending elevation order (draw order = blending order), narrowed by
    // the elevation filter when one is set.
    let mut cuts: Vec<_> = volume
        .sweeps
        .iter()
        .filter(|s| {
            filter
                .elev_min_deg
                .is_none_or(|lo| s.elevation_deg >= lo - 0.01)
                && filter
                    .elev_max_deg
                    .is_none_or(|hi| s.elevation_deg <= hi + 0.01)
        })
        .filter_map(|s| s.moment(moment).map(|m| (s, m)))
        .collect();
    cuts.sort_by(|a, b| {
        a.0.elevation_deg
            .partial_cmp(&b.0.elevation_deg)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for (sweep, md) in cuts {
        if md.ray_count == 0 || md.gate_count == 0 {
            continue;
        }
        let first = mesh.vertices.len() as u32;
        let elev = sweep.elevation_measured_deg;

        // --- bin rays into evenly spaced azimuth columns, averaged ---------
        // Super-resolution rays are 0.25–0.5° apart; a 2° column therefore
        // averages a handful of them, which is both cheaper and less noisy
        // than picking one.
        let mut sums = vec![0.0f32; n_az * md.gate_count];
        let mut counts = vec![0u16; n_az * md.gate_count];
        for ray in 0..md.ray_count {
            let az = sweep
                .azimuths_deg
                .get(ray)
                .copied()
                .unwrap_or(ray as f32 * sweep.azimuth_width_deg);
            let col = ((az.rem_euclid(360.0) / az_step).round() as usize) % n_az;
            let row = &md.data[ray * md.gate_count..(ray + 1) * md.gate_count];
            let base = col * md.gate_count;
            for (g, &dn) in row.iter().enumerate() {
                if dn != 0 {
                    let v = (dn as f32 - md.calibration.offset) / md.calibration.scale;
                    sums[base + g] += v;
                    counts[base + g] = counts[base + g].saturating_add(1);
                }
            }
        }
        // NaN marks "no data" for this (column, gate).
        let grid: Vec<f32> = (0..n_az * md.gate_count)
            .map(|i| {
                if counts[i] == 0 {
                    f32::NAN
                } else {
                    sums[i] / f32::from(counts[i])
                }
            })
            .collect();

        // --- sample the grid onto the render lattice -----------------------
        let range_start_km = md.range_start_m / 1000.0;
        let gate_size_km = (md.gate_size_m / 1000.0).max(1.0e-4);
        let max_gate_range_km = range_start_km + md.gate_count as f32 * gate_size_km;
        let rows = (((max_gate_range_km - range_start_km) / range_step).floor() as usize + 1)
            .clamp(2, 600);

        let mut cols: Vec<Vec<[f32; 3]>> = Vec::with_capacity(n_az);
        for col in 0..n_az {
            let az_rad = (col as f32 * az_step).to_radians();
            let (sin_a, cos_a) = az_rad.sin_cos();
            let mut row_pts = Vec::with_capacity(rows);
            for j in 0..rows {
                let r_km = range_start_km + j as f32 * range_step;
                let value = {
                    let g = ((r_km - range_start_km) / gate_size_km - 0.5)
                        .round()
                        .clamp(0.0, (md.gate_count - 1) as f32)
                        as usize;
                    grid[col * md.gate_count + g]
                };
                let ground_km = r_km * elev.to_radians().cos();
                let z = beam_height_km(r_km, elev);
                row_pts.push([
                    ground_km * sin_a,
                    ground_km * cos_a,
                    if value.is_finite() { z } else { f32::NAN },
                ]);
            }
            cols.push(row_pts);
        }

        // --- emit vertices -------------------------------------------------
        let base_u32 = first;
        for (col, row_pts) in cols.iter().enumerate() {
            for (j, p) in row_pts.iter().enumerate() {
                let r_km = range_start_km + j as f32 * range_step;
                let g = ((r_km - range_start_km) / gate_size_km - 0.5)
                    .round()
                    .clamp(0.0, (md.gate_count - 1) as f32) as usize;
                let value = grid[col * md.gate_count + g];
                let color = if value.is_finite() {
                    let c = table.map(value);
                    // Premultiply by the table alpha; the shader multiplies
                    // the user's opacity on top.
                    let a = u32::from(c[3]);
                    [
                        ((u32::from(c[0]) * a + 127) / 255) as u8,
                        ((u32::from(c[1]) * a + 127) / 255) as u8,
                        ((u32::from(c[2]) * a + 127) / 255) as u8,
                        c[3],
                    ]
                } else {
                    [0, 0, 0, 0]
                };
                mesh.vertices.push(MeshVertex {
                    pos_km: *p,
                    normal: [0.0, 0.0, 1.0], // fixed up in the pass below
                    color,
                    value,
                });
            }
        }

        // --- indices: a quad only where all four corners carry data --------
        let tilt_index_start = mesh.indices.len() as u32;
        let idx = |col: usize, row: usize| (base_u32 + (col * rows + row) as u32) as u32;
        for col in 0..n_az {
            let col2 = (col + 1) % n_az;
            for j in 0..rows - 1 {
                let v00 = base_u32 + (col * rows + j) as u32;
                let v01 = base_u32 + (col * rows + j + 1) as u32;
                let v10 = base_u32 + (col2 * rows + j) as u32;
                let v11 = base_u32 + (col2 * rows + j + 1) as u32;
                let all_present = [
                    mesh.vertices[v00 as usize].value,
                    mesh.vertices[v01 as usize].value,
                    mesh.vertices[v10 as usize].value,
                    mesh.vertices[v11 as usize].value,
                ]
                .iter()
                .all(|v| v.is_finite());
                if !all_present {
                    continue;
                }
                let _ = idx;
                mesh.indices
                    .extend_from_slice(&[v00, v01, v11, v00, v11, v10]);
            }
        }
        let tilt_index_end = mesh.indices.len() as u32;

        // --- normals from grid neighbors -----------------------------------
        for col in 0..n_az {
            let col_r = (col + 1) % n_az;
            let col_l = (col + n_az - 1) % n_az;
            for j in 0..rows {
                let i = base_u32 as usize + col * rows + j;
                if !mesh.vertices[i].value.is_finite() {
                    continue;
                }
                let j_dn = j.saturating_sub(1);
                let j_up = (j + 1).min(rows - 1);
                let p = |c: usize, r: usize| mesh.vertices[base_u32 as usize + c * rows + r].pos_km;
                let t_az = sub(p(col_r, j), p(col_l, j));
                let t_r = sub(p(col, j_up), p(col, j_dn));
                let mut n = cross(t_az, t_r);
                let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                if len > 1.0e-6 {
                    n = [n[0] / len, n[1] / len, n[2] / len];
                    if n[2] < 0.0 {
                        n = [-n[0], -n[1], -n[2]];
                    }
                    mesh.vertices[i].normal = n;
                }
            }
        }

        if tilt_index_end > tilt_index_start {
            mesh.tilts.push(TiltDraw {
                elevation_deg: sweep.elevation_deg,
                indices: tilt_index_start..tilt_index_end,
            });
        }
        mesh.max_range_km = mesh.max_range_km.max(max_gate_range_km);
        if mesh.tilts.len() == 1 {
            mesh.lowest_elev_deg = sweep.elevation_deg;
        }
        mesh.highest_elev_deg = sweep.elevation_deg;
    }

    mesh
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Range rings every 50 statute miles out to the volume's range — the shared
/// source for the ground grid and the pane's distance labels. The 3D pane
/// speaks miles and feet only, so the rings are mile landmarks (50 mi =
/// 80 km, not "100 km"), like a sectional chart.
pub fn ring_radii_mi(max_range_km: f32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut mi = 50u32;
    while (mi as f32) * KM_PER_MILE <= max_range_km + 8.0 && out.len() < 8 {
        out.push(mi);
        mi += 50;
    }
    if out.is_empty() {
        out.push(50); // always at least one ring for scale
    }
    out
}

/// Tile keys covering the ground square around a site out to `radius_km`.
///
/// The zoom is chosen so the square is under ~10 tiles across: sharp enough
/// to read cities and highways on the ground plane, small enough that one
/// tile set is not a memory event. The 2D map's tile layer serves these from
/// the same disk cache (and the same network politeness gate).
pub fn ground_tile_keys(site_lat: f64, site_lon: f64, radius_km: f32) -> Vec<(u8, u32, u32)> {
    if radius_km < 1.0 {
        return Vec::new();
    }
    let base_mpp = 156_543.03 * site_lat.to_radians().cos().abs().max(0.05);
    let mpp_min = 2.2 * f64::from(radius_km) * 1000.0 / (256.0 * 10.0);
    let zoom = (base_mpp / mpp_min.max(1.0))
        .log2()
        .floor()
        .clamp(3.0, 12.0) as u8;
    let n = 2f64.powi(i32::from(zoom));

    let lon_span = f64::from(radius_km) / (111.32 * site_lat.to_radians().cos().abs().max(0.05));
    let lat_span = f64::from(radius_km) / 110.574;
    let lon2x = |lon: f64| (((lon + 180.0) / 360.0) * n).floor() as i64;
    let lat2y = |lat: f64| {
        let lat = lat.clamp(-85.05, 85.05).to_radians();
        (((1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0) * n).floor()
            as i64
    };
    let (x0, x1) = (lon2x(site_lon - lon_span), lon2x(site_lon + lon_span));
    let (y0, y1) = (lat2y(site_lat + lat_span), lat2y(site_lat - lat_span));
    let mut out = Vec::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            if y < 0 || x < 0 || y >= n as i64 || x >= n as i64 {
                continue;
            }
            out.push((zoom, x as u32, y as u32));
        }
    }
    out
}

/// Geographic bounds of one tile in the scene's local frame: kilometres east
/// and north of the site, `[x_min, y_min, x_max, y_max]`.
///
/// Equirectangular around the site — the same approximation the app's other
/// distance math uses; its distortion over a ground plane is far below one
/// pixel.
pub fn ground_tile_bounds_km(key: (u8, u32, u32), site_lat: f64, site_lon: f64) -> [f32; 4] {
    let (z, x, y) = key;
    let n = 2f64.powi(i32::from(z));
    let lon0 = x as f64 / n * 360.0 - 180.0;
    let lon1 = (x + 1) as f64 / n * 360.0 - 180.0;
    let lat_at = |yy: f64| {
        (std::f64::consts::PI * (1.0 - 2.0 * yy / n))
            .sinh()
            .atan()
            .to_degrees()
    };
    let km_per_deg_lon = 111.32 * site_lat.to_radians().cos();
    let km_per_deg_lat = 110.574;
    [
        ((lon0 - site_lon) * km_per_deg_lon) as f32,
        ((lat_at((y + 1) as f64) - site_lat) * km_per_deg_lat) as f32,
        ((lon1 - site_lon) * km_per_deg_lon) as f32,
        ((lat_at(y as f64) - site_lat) * km_per_deg_lat) as f32,
    ]
}

/// Ground reference grid: the mile range rings plus 12 spokes, emitted as
/// *centerlines* (two verts per path point, the ribbon's perpendicular in
/// the normal) so the vertex shader can regenerate the width on screen. Thin
/// baked ribbons alias into flicker once the view compresses them below a
/// pixel — a screen-space minimum width keeps rings and spokes stable at any
/// pitch and distance, which MSAA-style fixes cannot do cheaply here.
pub fn build_ground_grid(max_range_km: f32) -> (Vec<MeshVertex>, Vec<u32>) {
    let mut verts = Vec::new();
    let mut indices = Vec::new();
    // Dim gray, premultiplied by 0.42.
    let color = [52u8, 56, 63, 107];

    for r_mi in ring_radii_mi(max_range_km) {
        let r = r_mi as f32 * KM_PER_MILE;
        let segments = 256;
        let start = verts.len() as u32;
        for s in 0..=segments {
            let a = s as f32 / segments as f32 * std::f32::consts::TAU;
            let (sin_a, cos_a) = a.sin_cos();
            let p = [r * sin_a, r * cos_a, 0.0];
            verts.push(grid_vertex(p, color, [sin_a, cos_a]));
            verts.push(grid_vertex(p, color, [-sin_a, -cos_a]));
        }
        for s in 0..segments {
            let i = start + s as u32 * 2;
            indices.extend_from_slice(&[i, i + 1, i + 3, i, i + 3, i + 2]);
        }
    }

    for k in 0..12 {
        let a = k as f32 * 30.0f32.to_radians();
        let (sin_a, cos_a) = a.sin_cos();
        // Unit perpendicular to the spoke.
        let (px, py) = (-cos_a, sin_a);
        let steps = ((max_range_km / 10.0).ceil() as u32).max(1);
        let start = verts.len() as u32;
        for s in 0..=steps {
            let r = max_range_km * s as f32 / steps as f32;
            let p = [r * sin_a, r * cos_a, 0.0];
            verts.push(grid_vertex(p, color, [px, py]));
            verts.push(grid_vertex(p, color, [-px, -py]));
        }
        for s in 0..steps {
            let i = start + s * 2;
            indices.extend_from_slice(&[i, i + 1, i + 3, i, i + 3, i + 2]);
        }
    }

    (verts, indices)
}

fn grid_vertex(pos_km: [f32; 3], color: [u8; 4], perp: [f32; 2]) -> MeshVertex {
    MeshVertex {
        // 20 m above the ground tiles: the ground writes depth, and this
        // keeps the grid from z-fighting with it at grazing angles.
        pos_km: [pos_km[0], pos_km[1], 0.02],
        // The ribbon perpendicular lives in the normal; the shader widens
        // the line along it and shades the grid unlit.
        normal: [perp[0], perp[1], 0.0],
        color,
        value: NO_THRESHOLD_VALUE,
    }
}

/// Orbit camera looking at the radar site (world origin is the site; X east,
/// Y north, Z up; kilometres).
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    /// Azimuth the camera sits at, degrees clockwise from north. Wider view
    /// uses larger values; 0 puts the camera to the *south* of the site so
    /// north is up on screen.
    pub yaw_deg: f32,
    /// Elevation of the camera above the horizon, degrees (clamped by the UI).
    pub pitch_deg: f32,
    pub distance_km: f32,
    pub target_km: [f32; 3],
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            yaw_deg: 0.0,
            pitch_deg: 38.0,
            distance_km: 500.0,
            target_km: [0.0, 0.0, 0.0],
        }
    }
}

/// Vertical field of view of the scene camera, degrees.
pub const FOV_Y_DEG: f32 = 40.0;

impl Camera {
    pub fn eye(&self) -> [f32; 3] {
        let yaw = self.yaw_deg.to_radians();
        let pitch = self.pitch_deg.to_radians();
        let (sy, cy) = yaw.sin_cos();
        let (sp, cp) = pitch.sin_cos();
        [
            self.target_km[0] + self.distance_km * cp * sy,
            self.target_km[1] - self.distance_km * cp * cy,
            self.target_km[2] + self.distance_km * sp,
        ]
    }

    /// Screen-plane pan: a pointer drag (points) as a world-space offset for
    /// `target_km` (km). Positive `dx` (drag right) moves the scene content
    /// right; positive `dy` (drag down in screen coordinates) likewise — the
    /// natural "grab the world" feel. `viewport_h` is the pane height in the
    /// same units as the drag (the math is a ratio, so points are fine).
    pub fn pan_offset_km(&self, drag: [f32; 2], viewport_h: f32) -> [f32; 3] {
        let eye = self.eye();
        let fwd = normalize(sub(self.target_km, eye));
        let right = normalize(cross(fwd, [0.0, 0.0, 1.0]));
        let up = cross(right, fwd);
        // Kilometres per screen unit at the target plane.
        let wpp =
            2.0 * self.distance_km * (FOV_Y_DEG.to_radians() / 2.0).tan() / viewport_h.max(1.0);
        [
            -right[0] * drag[0] * wpp + up[0] * drag[1] * wpp,
            -right[1] * drag[0] * wpp + up[1] * drag[1] * wpp,
            -right[2] * drag[0] * wpp + up[2] * drag[1] * wpp,
        ]
    }

    /// Column-major view-projection for WGSL. `max_range_km` sizes the
    /// depth range so a close zoom doesn't z-fight and a far view keeps the
    /// whole grid inside the frustum.
    pub fn view_proj(&self, aspect: f32, max_range_km: f32) -> [[f32; 4]; 4] {
        let near = (self.distance_km * 0.02).max(0.1);
        let far = self.distance_km + max_range_km * 4.0 + 50.0;
        let view = look_at_rh(self.eye(), self.target_km);
        let proj = perspective_rh(FOV_Y_DEG.to_radians(), aspect.max(0.05), near, far);
        mul(&proj, &view)
    }

    /// Project a world point (km) to NDC, or `None` when it falls outside
    /// the depth range / behind the camera. NDC maps to screen with
    /// `x = cx + ndc.x·w/2`, `y = cy − ndc.y·h/2`.
    pub fn project(&self, aspect: f32, max_range_km: f32, world: [f32; 3]) -> Option<[f32; 2]> {
        let m = self.view_proj(aspect, max_range_km);
        let (x, y, z) = (world[0], world[1], world[2]);
        let w = m[0][3] * x + m[1][3] * y + m[2][3] * z + m[3][3];
        if w <= 1.0e-6 {
            return None;
        }
        let ndc_x = (m[0][0] * x + m[1][0] * y + m[2][0] * z + m[3][0]) / w;
        let ndc_y = (m[0][1] * x + m[1][1] * y + m[2][1] * z + m[3][1]) / w;
        let ndc_z = (m[0][2] * x + m[1][2] * y + m[2][2] * z + m[3][2]) / w;
        if !(0.0..=1.0).contains(&ndc_z) {
            return None;
        }
        Some([ndc_x, ndc_y])
    }
}

/// Standard right-handed look-at, row-major math converted to WGSL's
/// column-major array layout on return.
fn look_at_rh(eye: [f32; 3], target: [f32; 3]) -> [[f32; 4]; 4] {
    let f = normalize(sub(target, eye));
    let up = if f[2].abs() > 0.9999 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 0.0, 1.0]
    };
    let s = normalize(cross(f, up));
    let u = cross(s, f);
    [
        [s[0], u[0], -f[0], 0.0],
        [s[1], u[1], -f[1], 0.0],
        [s[2], u[2], -f[2], 0.0],
        [-dot(s, eye), -dot(u, eye), dot(f, eye), 1.0],
    ]
}

/// Right-handed perspective with wgpu's 0..1 depth range.
fn perspective_rh(fovy: f32, aspect: f32, near: f32, far: f32) -> [[f32; 4]; 4] {
    let f = 1.0 / (fovy / 2.0).tan();
    [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, far / (near - far), -1.0],
        [0.0, 0.0, near * far / (near - far), 0.0],
    ]
}

/// `a * b` for column-major matrices (`m[col][row]`).
fn mul(a: &[[f32; 4]; 4], b: &[[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0f32; 4]; 4];
    for col in 0..4 {
        for row in 0..4 {
            out[col][row] = (0..4).map(|k| a[k][row] * b[col][k]).sum();
        }
    }
    out
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = dot(v, v).sqrt();
    if len > 1.0e-9 {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 0.0, 1.0]
    }
}

#[cfg(test)]
mod tests {
    /// Sampler: value interpolation between cuts, gaps and bounds as None.
    #[test]
    fn sampler_interpolates_between_cuts_and_honors_gaps() {
        use chrono::TimeZone;
        use radar_core::{MomentCalibration, MomentData, Sweep, VolumeMeta};
        let cal = MomentCalibration {
            scale: 1.0,
            offset: 2.0,
            threshold: 0.0,
        };
        // Two cuts at 0.5 and 1.5 deg, constant 20 and 40 dBZ, every ray.
        let mk = |elev: f32, dbz: f32, drop_ray: Option<usize>| {
            let rays: Vec<f32> = (0..360).map(|i| i as f32).collect();
            let mut data = vec![0i16; 360 * 200];
            for r in 0..360 {
                if Some(r) == drop_ray {
                    continue;
                }
                for g in 0..200 {
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
        let vol = Volume {
            meta: VolumeMeta {
                site_id: "KTWX".into(),
                volume_start: chrono::Utc.with_ymd_and_hms(2026, 10, 18, 0, 0, 0).unwrap(),
                vcp: None,
                sequence_number: None,
                sweep_count: 2,
                moments: vec![Moment::Reflectivity],
            },
            sweeps: vec![mk(0.5, 20.0, None), mk(1.5, 40.0, None)],
        };
        let s = VolumeSampler::new(&vol, Moment::Reflectivity);
        assert!((s.max_range_km() - 50.0).abs() < 0.01);

        // 30 km north (inside the 50 km sweep), elevation 1.0 deg (halfway between the cuts) -> 30.
        let h = 30.0 * 1.0f32.to_radians().tan();
        let v = s.sample(0.0, 30.0, h).expect("inside the data");
        assert!((v - 30.0).abs() < 0.5, "expected ~30, got {v}");

        // Exactly on a cut -> that cut's value.
        let h0 = 30.0 * 0.5f32.to_radians().tan();
        let v = s.sample(0.0, 30.0, h0).expect("on the low cut");
        assert!((v - 20.0).abs() < 0.2, "expected ~20, got {v}");

        // Above the highest cut / beyond range / in a dropped ray: None.
        assert!(
            s.sample(0.0, 30.0, 30.0 * 3.0f32.to_radians().tan())
                .is_none()
        );
        assert!(s.sample(0.0, 400.0, 2.0).is_none());
    }
    use super::*;
    use chrono::TimeZone;
    use radar_core::{MomentCalibration, MomentData, Sweep, VolumeMeta};

    fn sweep(elev: f32, rays: usize, gates: usize, f: impl Fn(usize, usize) -> f32) -> Sweep {
        let cal = MomentCalibration {
            scale: 1.0,
            offset: 2.0,
            threshold: 0.0,
        };
        let mut data = vec![0i16; rays * gates];
        for ray in 0..rays {
            for gate in 0..gates {
                let v = f(ray, gate);
                if v.is_finite() {
                    data[ray * gates + gate] = (v * cal.scale + cal.offset).round() as i16;
                }
            }
        }
        Sweep {
            elevation_deg: elev,
            elevation_measured_deg: elev,
            azimuth_width_deg: 360.0 / rays as f32,
            azimuths_deg: (0..rays).map(|i| i as f32 * 360.0 / rays as f32).collect(),
            moments: vec![MomentData {
                kind: Moment::Reflectivity,
                calibration: cal,
                range_start_m: 2125.0,
                gate_size_m: 250.0,
                gate_count: gates,
                ray_count: rays,
                data,
            }],
        }
    }

    fn volume(sweeps: Vec<Sweep>) -> Volume {
        let sweep_count = sweeps.len();
        Volume {
            meta: VolumeMeta {
                site_id: "KTWX".into(),
                volume_start: chrono::Utc.with_ymd_and_hms(2026, 10, 16, 6, 0, 0).unwrap(),
                vcp: Some(212),
                sequence_number: Some(1),
                sweep_count,
                moments: vec![Moment::Reflectivity],
            },
            sweeps,
        }
    }

    /// The 4/3-earth beam height: 0.5° at 100 km is ~1.46 km (0.87 rise +
    /// 0.59 curvature) — the number forecasters use for beam placement.
    #[test]
    fn beam_height_matches_the_4_3_earth_model() {
        let h = beam_height_km(100.0, 0.5);
        assert!((h - 1.4612).abs() < 0.002, "0.5° @ 100 km = {h} km");
        assert_eq!(beam_height_km(0.0, 0.5), 0.0);
        // 0.9° at 230 km: 3.61 + 3.14 ≈ 6.75 km — the far 0.9° beam.
        let h = beam_height_km(230.0, 0.9);
        assert!((h - 6.75).abs() < 0.05, "0.9° @ 230 km = {h} km");
        // Steeper angles rise faster: 19.5° at 50 km ≈ 18.1 km.
        let h = beam_height_km(50.0, 19.5);
        assert!(h > 16.0 && h < 19.0, "19.5° @ 50 km = {h} km");
    }

    #[test]
    fn mesh_only_references_vertices_that_carry_data() {
        // Dense rays (2° apart — every Normal column carries data) with
        // periodic no-echo rows.
        let s = sweep(0.5, 180, 40, |ray, gate| {
            if gate % 7 == 3 {
                f32::NAN
            } else {
                20.0 + (ray % 3) as f32
            }
        });
        let v = volume(vec![s]);
        let mesh = build_volume_mesh(
            &v,
            &MeshFilter::new(Moment::Reflectivity, MeshDetail::Normal),
        );
        assert!(mesh.triangle_count() > 0);
        assert_eq!(mesh.indices.len() % 3, 0);
        for &i in &mesh.indices {
            assert!((i as usize) < mesh.vertices.len());
            assert!(
                mesh.vertices[i as usize].value.is_finite(),
                "index {i} points at a no-data vertex"
            );
        }
        // No-data vertices exist (so the skip actually skipped something).
        assert!(mesh.vertices.iter().any(|v| !v.value.is_finite()));
    }

    #[test]
    fn a_volume_with_no_echo_produces_no_triangles() {
        let s = sweep(0.5, 8, 40, |_, _| f32::NAN);
        let v = volume(vec![s]);
        let mesh = build_volume_mesh(
            &v,
            &MeshFilter::new(Moment::Reflectivity, MeshDetail::Normal),
        );
        assert_eq!(mesh.triangle_count(), 0);
        assert!(mesh.tilts.is_empty());
    }

    #[test]
    fn tilts_come_out_in_ascending_elevation_with_contiguous_ranges() {
        let s1 = sweep(2.4, 180, 40, |_, _| 30.0);
        let s2 = sweep(0.5, 180, 40, |_, _| 20.0);
        let s3 = sweep(1.3, 180, 40, |_, _| 25.0);
        let v = volume(vec![s1, s2, s3]);
        let mesh = build_volume_mesh(
            &v,
            &MeshFilter::new(Moment::Reflectivity, MeshDetail::Coarse),
        );
        let elevs: Vec<f32> = mesh.tilts.iter().map(|t| t.elevation_deg).collect();
        assert_eq!(elevs, vec![0.5, 1.3, 2.4]);
        let mut cursor = 0;
        for t in &mesh.tilts {
            assert_eq!(t.indices.start, cursor);
            cursor = t.indices.end;
        }
        assert_eq!(cursor as usize, mesh.indices.len());
        assert_eq!(mesh.lowest_elev_deg, 0.5);
        assert_eq!(mesh.highest_elev_deg, 2.4);
    }

    #[test]
    fn fine_detail_has_more_geometry_than_coarse() {
        let s = sweep(0.5, 360, 400, |ray, gate| {
            if ray % 9 == 0 {
                f32::NAN
            } else {
                30.0 + (gate % 5) as f32
            }
        });
        let v = volume(vec![s]);
        let coarse = build_volume_mesh(
            &v,
            &MeshFilter::new(Moment::Reflectivity, MeshDetail::Coarse),
        );
        let fine = build_volume_mesh(&v, &MeshFilter::new(Moment::Reflectivity, MeshDetail::Fine));
        assert!(fine.vertices.len() > coarse.vertices.len());
        assert!(fine.triangle_count() > coarse.triangle_count());
    }

    #[test]
    fn vertex_colors_are_premultiplied_table_colors() {
        let s = sweep(0.5, 8, 40, |_, _| 30.0);
        let v = volume(vec![s]);
        let mesh = build_volume_mesh(
            &v,
            &MeshFilter::new(Moment::Reflectivity, MeshDetail::Coarse),
        );
        let table = ColorTable::for_moment(Moment::Reflectivity);
        let expect = table.map(30.0);
        let a = expect[3] as u32;
        let want = [
            ((expect[0] as u32 * a + 127) / 255) as u8,
            ((expect[1] as u32 * a + 127) / 255) as u8,
            ((expect[2] as u32 * a + 127) / 255) as u8,
            expect[3],
        ];
        let vtx = mesh
            .vertices
            .iter()
            .find(|v| v.value.is_finite())
            .expect("data vertices exist");
        assert_eq!(vtx.color, want);
        assert_eq!(vtx.value, 30.0);
    }

    #[test]
    fn ground_grid_scales_with_range() {
        let (verts, idx) = build_ground_grid(180.0);
        // 180 km covers the 50 and 100 mile rings (80 / 161 km).
        assert!(verts.len() > 2 * 256, "mile rings expected");
        assert_eq!(idx.len() % 3, 0);
        for &i in &idx {
            assert!((i as usize) < verts.len());
        }
        assert!(verts.iter().all(|v| v.value == NO_THRESHOLD_VALUE));
        assert!(
            verts.iter().all(|v| v.normal[0].hypot(v.normal[1]) > 0.9),
            "grid normals must carry the ribbon perpendicular"
        );
        let (verts_small, _) = build_ground_grid(60.0);
        assert!(verts_small.len() < verts.len());
    }

    /// The 3D pane speaks miles: rings are 50/100/150 mi landmarks, not
    /// round kilometres, and there is always at least one for scale.
    #[test]
    fn ring_radii_are_mile_landmarks() {
        assert_eq!(ring_radii_mi(300.0), vec![50, 100, 150]);
        assert_eq!(ring_radii_mi(460.0), vec![50, 100, 150, 200, 250]);
        assert_eq!(ring_radii_mi(60.0), vec![50]);
        // 150 mi = 241 km barely over a 230 km range: dropped.
        assert_eq!(ring_radii_mi(230.0), vec![50, 100]);
    }

    #[test]
    fn tilt_filter_keeps_only_selected_cuts() {
        let s1 = sweep(0.5, 180, 40, |_, _| 20.0);
        let s2 = sweep(1.3, 180, 40, |_, _| 25.0);
        let s3 = sweep(3.1, 180, 40, |_, _| 30.0);
        let v = volume(vec![s1, s2, s3]);
        let mut filter = MeshFilter::new(Moment::Reflectivity, MeshDetail::Coarse);
        filter.elev_min_deg = Some(1.3);
        filter.elev_max_deg = Some(3.1);
        let mesh = build_volume_mesh(&v, &filter);
        let elevs: Vec<f32> = mesh.tilts.iter().map(|t| t.elevation_deg).collect();
        assert_eq!(elevs, vec![1.3, 3.1]);
        // One cut alone.
        let mut filter = MeshFilter::new(Moment::Reflectivity, MeshDetail::Coarse);
        filter.elev_min_deg = Some(1.3);
        filter.elev_max_deg = Some(1.3);
        let mesh = build_volume_mesh(&v, &filter);
        assert_eq!(mesh.tilts.len(), 1);
        assert_eq!(mesh.tilts[0].elevation_deg, 1.3);
    }

    /// The ground tiles must actually contain the site, be a sane count, and
    /// have ordered bounds — the invariants the GPU quads rely on.
    #[test]
    fn ground_tiles_cover_the_site() {
        let (lat, lon) = (38.996, -96.232); // KTWX
        let keys = ground_tile_keys(lat, lon, 300.0);
        assert!(!keys.is_empty());
        assert!(keys.len() <= 130, "too many tiles: {}", keys.len());
        let z = keys[0].0;
        assert!(keys.iter().all(|k| k.0 == z));
        let covers_site = keys.iter().any(|k| {
            let b = ground_tile_bounds_km(*k, lat, lon);
            b[0] <= 0.0 && 0.0 <= b[2] && b[1] <= 0.0 && 0.0 <= b[3]
        });
        assert!(covers_site, "no tile contains the site");
        for k in &keys {
            let b = ground_tile_bounds_km(*k, lat, lon);
            assert!(b[0] < b[2] && b[1] < b[3], "bad bounds {b:?} for {k:?}");
        }
        // A tighter range asks for a sharper zoom, so fewer km per tile.
        let wide = ground_tile_bounds_km(keys[0], lat, lon);
        let fine_keys = ground_tile_keys(lat, lon, 120.0);
        let fine = ground_tile_bounds_km(fine_keys[0], lat, lon);
        assert!(fine[2] - fine[0] <= wide[2] - wide[0]);
    }

    #[test]
    fn camera_centers_its_target_and_keeps_north_up() {
        let cam = Camera {
            pitch_deg: 45.0,
            ..Camera::default()
        };
        let aspect = 16.0 / 9.0;
        let ndc = cam.project(aspect, 230.0, [0.0, 0.0, 0.0]).unwrap();
        assert!(ndc[0].abs() < 1.0e-4 && ndc[1].abs() < 1.0e-4, "{ndc:?}");
        let north = cam.project(aspect, 230.0, [0.0, 100.0, 0.0]).unwrap();
        assert!(north[1] > 0.0, "north should be up: {north:?}");
        let east = cam.project(aspect, 230.0, [100.0, 0.0, 0.0]).unwrap();
        assert!(east[0] > 0.0, "east should be right: {east:?}");
        // A point well behind the camera does not project.
        let behind = cam.project(aspect, 230.0, [0.0, -1500.0, 0.0]);
        assert!(behind.is_none());
    }

    #[test]
    fn pan_moves_the_content_with_the_pointer() {
        let cam = Camera {
            pitch_deg: 38.0,
            distance_km: 500.0,
            ..Camera::default()
        };
        // Default view faces north; dragging right must carry the scene east
        // (i.e. move the camera/target west).
        let right = cam.pan_offset_km([100.0, 0.0], 800.0);
        assert!(right[0] < 0.0, "drag right: {right:?}");
        assert!(right[0].abs() > 1.0, "too little pan per 100 px: {right:?}");
        // Dragging down raises the camera (content follows down).
        let down = cam.pan_offset_km([0.0, 100.0], 800.0);
        assert!(down[2] > 0.0, "drag down should rise: {down:?}");
        // Panning is symmetric.
        let back = cam.pan_offset_km([-100.0, 0.0], 800.0);
        assert!((back[0] + right[0]).abs() < 1.0e-4);
    }

    #[test]
    fn yaw_rotates_the_scene() {
        let cam = Camera {
            yaw_deg: 90.0,
            pitch_deg: 45.0,
            ..Camera::default()
        };
        // Camera swung to the east looking west: the observer's right hand
        // (screen right) now points north, and south ends up on the left.
        let north = cam.project(1.6, 230.0, [0.0, 100.0, 0.0]).unwrap();
        let south = cam.project(1.6, 230.0, [0.0, -100.0, 0.0]).unwrap();
        assert!(north[0] > 0.2, "north should be right: {north:?}");
        assert!(south[0] < -0.2, "south should be left: {south:?}");
    }
}

/// Interpolating sampler over one moment of a volume: the echo at an
/// arbitrary scene point (km ENU, X east / Y north / Z beam height).
///
/// This is the shared foundation of the data probe, the vertical
/// cross-section and the isosurface grid — all three ask "what is here?".
/// Sampling is nearest-ray + linear between gates, then linear between the
/// two cuts bracketing the point's elevation; the (r, z) ↔ (slant, elev)
/// mapping ignores earth curvature (it is ~2 km of error at 200 km — far
/// below the grid the callers sample on).
pub struct VolumeSampler<'a> {
    cuts: Vec<CutSampler<'a>>,
}

struct CutSampler<'a> {
    elevation_deg: f32,
    data: &'a MomentData,
    azimuths: &'a [f32],
}

impl<'a> VolumeSampler<'a> {
    pub fn new(volume: &'a Volume, moment: Moment) -> Self {
        let mut cuts: Vec<CutSampler<'a>> = volume
            .sweeps
            .iter()
            .filter_map(|s| {
                s.moment(moment).map(|m| CutSampler {
                    elevation_deg: if s.elevation_measured_deg.is_finite()
                        && s.elevation_measured_deg != 0.0
                    {
                        s.elevation_measured_deg
                    } else {
                        s.elevation_deg
                    },
                    data: m,
                    azimuths: &s.azimuths_deg,
                })
            })
            .collect();
        cuts.sort_by(|a, b| {
            a.elevation_deg
                .partial_cmp(&b.elevation_deg)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Self { cuts }
    }

    /// Longest slant range covered by any cut (km).
    pub fn max_range_km(&self) -> f32 {
        self.cuts
            .iter()
            .map(|c| {
                (c.data.range_start_m + c.data.gate_count as f32 * c.data.gate_size_m) / 1000.0
            })
            .fold(0.0, f32::max)
    }

    /// Highest cut elevation (deg).
    pub fn top_elevation_deg(&self) -> f32 {
        self.cuts.last().map(|c| c.elevation_deg).unwrap_or(0.0)
    }

    /// The value at a scene point, or `None` outside the data (below the
    /// lowest cut, above the highest, beyond range, or in a no-echo gap).
    pub fn sample(&self, x_km: f32, y_km: f32, z_km: f32) -> Option<f32> {
        if self.cuts.is_empty() {
            return None;
        }
        let ground = x_km.hypot(y_km);
        let slant = ground.hypot(z_km);
        let elev = z_km.atan2(ground).to_degrees();
        let az = x_km.atan2(y_km).to_degrees().rem_euclid(360.0);

        // Strictly within the sampled elevation range (a hair of tolerance
        // for floating point on "exactly the top cut"): above the highest
        // cut there is no data to sample, so the answer is None — the
        // cross-section and isosurface callers rely on that to stop.
        let lo = self.cuts.first().map(|c| c.elevation_deg).unwrap_or(0.0);
        let hi = self.cuts.last().map(|c| c.elevation_deg).unwrap_or(0.0);
        if elev < lo - 0.05 || elev > hi + 0.05 {
            return None;
        }

        // Bracket the point's elevation between the two nearest cuts.
        let mut above = None;
        let mut below = None;
        for (i, c) in self.cuts.iter().enumerate() {
            if c.elevation_deg <= elev {
                below = Some(i);
            } else {
                above = Some(i);
                break;
            }
        }
        match (below, above) {
            (Some(b), Some(a)) => {
                let cb = self.sample_cut(b, az, slant)?;
                let ca = self.sample_cut(a, az, slant)?;
                let t = (elev - self.cuts[b].elevation_deg)
                    / (self.cuts[a].elevation_deg - self.cuts[b].elevation_deg);
                Some(cb + (ca - cb) * t)
            }
            (Some(b), None) => self.sample_cut(b, az, slant),
            (None, Some(a)) => self.sample_cut(a, az, slant),
            (None, None) => None,
        }
    }

    /// One cut: nearest ray, linear between the two nearest gates.
    fn sample_cut(&self, cut: usize, az_deg: f32, slant_km: f32) -> Option<f32> {
        let c = &self.cuts[cut];
        if c.azimuths.is_empty() {
            return None;
        }
        // Nearest ray (the azimuth ring is short; a scan is fine).
        let mut best = 0usize;
        let mut best_d = angle_delta(c.azimuths[0], az_deg).abs();
        for (i, a) in c.azimuths.iter().enumerate().skip(1) {
            let d = angle_delta(*a, az_deg).abs();
            if d < best_d {
                best = i;
                best_d = d;
            }
        }
        if best_d > 180.0 {
            return None;
        }
        let m = c.data;
        let slant_m = slant_km * 1000.0;
        let gf = (slant_m - m.range_start_m) / m.gate_size_m;
        if gf < -0.5 || gf > m.gate_count as f32 - 0.5 {
            return None;
        }
        let g0 = gf
            .floor()
            .clamp(0.0, (m.gate_count.saturating_sub(2)) as f32) as usize;
        let t = (gf - g0 as f32).clamp(0.0, 1.0);
        let v0 = m.value(best, g0)?;
        let v1 = m.value(best, g0 + 1).unwrap_or(v0);
        Some(v0 + (v1 - v0) * t)
    }
}
