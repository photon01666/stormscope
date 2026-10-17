//! Sweep rasterization into an RGBA viewport overlay.
//!
//! For every ray/gate of a [`Sweep`] the gate is geolocated with the
//! 4/3-earth model ([`gate_latlon`]), projected into the viewport with the
//! shared [`MapView`] mapping, and splatted as a small disc sized to the
//! gate's screen-space footprint, alpha-composited over whatever is already
//! in the buffer.

use crate::colormap::ColorTable;
use crate::projection::{MapView, MercProjector};
use radar_core::{Moment, Sweep};

// Re-export so `radar_render::raster::gate_latlon` keeps working.
pub use crate::projection::gate_latlon;

/// Minimum radius (viewport px) of a gate splat. Keeps echoes looking
/// continuous even when gates are sub-pixel (low zoom / far ranges).
const MIN_GATE_RADIUS_PX: f64 = 1.0;
/// Upper clamp so a single oversized gate never floods the screen.
const MAX_GATE_RADIUS_PX: f64 = 48.0;

/// RGBA image covering the viewport at (0,0) top-left, row-major.
pub struct RadarImage {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl RadarImage {
    /// A fully transparent image the size of the viewport.
    pub fn transparent(view: &MapView) -> Self {
        let (w, h) = (view.width(), view.height());
        Self {
            width: w,
            height: h,
            rgba: vec![0u8; w * h * 4],
        }
    }

    /// Straight RGBA of one pixel (out of range => fully transparent).
    pub fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0, 0, 0, 0];
        }
        let o = (y * self.width + x) * 4;
        let p = &self.rgba[o..o + 4];
        [p[0], p[1], p[2], p[3]]
    }
}

/// Rasterize one sweep/moment for the current view.
/// `radar_lat`/`radar_lon`: radar site coordinates. `opacity`: 0..=1.
pub fn rasterize_sweep(
    sweep: &Sweep,
    moment: Moment,
    view: &MapView,
    radar_lat: f64,
    radar_lon: f64,
    opacity: f32,
    min_dbz: f32,
) -> RadarImage {
    let mut img = RadarImage::transparent(view);
    rasterize_sweep_into(
        sweep,
        moment,
        view,
        radar_lat,
        radar_lon,
        opacity,
        min_dbz,
        &mut img.rgba,
    );
    img
}

/// Premultiplied-alpha variant of [`rasterize_sweep`], ready for
/// `egui::ColorImage::from_rgba_premultiplied` + texture upload (what the app
/// paints). The blend is division-free (the straight-alpha path divides by
/// 255 and by the destination alpha per channel per pixel), which is what
/// makes echo-dense frames — Min dBZ 0, VEL, clear-air haze — stay smooth.
pub fn rasterize_sweep_premultiplied(
    sweep: &Sweep,
    moment: Moment,
    view: &MapView,
    radar_lat: f64,
    radar_lon: f64,
    opacity: f32,
    min_dbz: f32,
) -> RadarImage {
    let mut img = RadarImage::transparent(view);
    rasterize_sweep_premultiplied_into(
        sweep,
        moment,
        view,
        radar_lat,
        radar_lon,
        opacity,
        min_dbz,
        &mut img.rgba,
    );
    img
}

/// [`rasterize_sweep`] drawn into a caller-provided RGBA buffer
/// (`view.width() * view.height() * 4` bytes, row-major) so the app can
/// composite layers without reallocating every frame. Existing pixels are
/// alpha-composited over (source-over), not overwritten.
pub fn rasterize_sweep_into(
    sweep: &Sweep,
    moment: Moment,
    view: &MapView,
    radar_lat: f64,
    radar_lon: f64,
    opacity: f32,
    min_dbz: f32,
    buf: &mut [u8],
) {
    rasterize_impl::<false>(
        sweep, moment, view, radar_lat, radar_lon, opacity, min_dbz, buf,
    );
}

/// Premultiplied-alpha [`rasterize_sweep_into`]: same geometry, same
/// compositing result, but RGB channels are stored premultiplied by alpha and
/// the per-pixel blend needs no division. Pairs with
/// `egui::ColorImage::from_rgba_premultiplied`.
pub fn rasterize_sweep_premultiplied_into(
    sweep: &Sweep,
    moment: Moment,
    view: &MapView,
    radar_lat: f64,
    radar_lon: f64,
    opacity: f32,
    min_dbz: f32,
    buf: &mut [u8],
) {
    rasterize_impl::<true>(
        sweep, moment, view, radar_lat, radar_lon, opacity, min_dbz, buf,
    );
}

/// Shared gate loop; `PM` selects the premultiplied blend (see the wrappers).
fn rasterize_impl<const PM: bool>(
    sweep: &Sweep,
    moment: Moment,
    view: &MapView,
    radar_lat: f64,
    radar_lon: f64,
    opacity: f32,
    min_dbz: f32,
    buf: &mut [u8],
) {
    let (w, h) = (view.width(), view.height());
    assert_eq!(
        buf.len(),
        w * h * 4,
        "rasterize_sweep_into: buffer is {} bytes, expected {w}x{h} RGBA ({} bytes)",
        buf.len(),
        w * h * 4
    );
    if w == 0 || h == 0 {
        return;
    }
    let Some(md) = sweep.moment(moment) else {
        log::debug!("rasterize_sweep: sweep has no {moment:?} data");
        return;
    };
    if md.ray_count == 0 || md.gate_count == 0 {
        return;
    }
    let opacity = opacity.clamp(0.0, 1.0);
    let op = (opacity * 255.0).round() as u32;
    if op == 0 {
        return;
    }

    let table = ColorTable::for_moment(moment);
    let (floor, _) = table.range();
    // User dBZ filter (the "Min dBZ" slider): raises the effective floor so
    // clear-air clutter/noise can be suppressed. Ignored for non-REF moments
    // (callers pass -inf there).
    let thresh = floor.max(min_dbz);

    // Prefer the measured tilt when present; fall back to the requested one.
    let elevation_deg =
        if sweep.elevation_measured_deg.is_finite() && sweep.elevation_measured_deg != 0.0 {
            sweep.elevation_measured_deg
        } else {
            sweep.elevation_deg
        };
    let az_width_rad = (sweep.azimuth_width_deg as f64)
        .clamp(0.05, 45.0)
        .to_radians();
    // Ground meters per viewport pixel at the view center — used only to size
    // splats (positions always come from the exact projection).
    let m_per_px = crate::projection::meters_per_pixel(view.center_lat, view.zoom).max(1e-6);
    let proj = MercProjector::new(view);

    for ray in 0..md.ray_count {
        let Some(&azimuth_deg) = sweep.azimuths_deg.get(ray) else {
            break; // no azimuth for this ray (moment/azimuth count mismatch)
        };
        if !azimuth_deg.is_finite() {
            continue;
        }
        let row_base = ray * md.gate_count;
        if row_base >= md.data.len() {
            break; // truncated data block
        }
        for gate in 0..md.gate_count {
            let dn = md.data[row_base + gate];
            if dn == 0 {
                continue; // no data / range folded / below threshold
            }
            let value = (dn as f32 - md.calibration.offset) / md.calibration.scale;
            if !value.is_finite() || value < thresh {
                continue; // below the moment's floor or the user's Min dBZ
            }
            let color = table.map(value);
            if color[3] == 0 {
                continue;
            }
            let slant_m = md.range_start_m + (gate as f32 + 0.5) * md.gate_size_m;
            if !(slant_m > 0.0) {
                continue;
            }
            let (glat, glon) =
                gate_latlon(radar_lat, radar_lon, azimuth_deg, elevation_deg, slant_m);
            let (px, py) = proj.view_px(glon, glat);

            // Screen-space footprint of this gate: radial (gate size) vs
            // tangential (azimuth width at this range), whichever is larger.
            let extent_m = (md.gate_size_m as f64).max(az_width_rad * slant_m as f64);
            let radius = (0.5 * extent_m / m_per_px).clamp(MIN_GATE_RADIUS_PX, MAX_GATE_RADIUS_PX);

            if PM {
                splat_disc_pm(buf, w, h, px, py, radius, color, op);
            } else {
                splat_disc(buf, w, h, px, py, radius, color, op);
            }
        }
    }
}

/// Paint a filled disc of straight-alpha `color` (scaled by `op`/255) centered
/// at (cx, cy) in viewport pixels, source-over composited into `buf`.
fn splat_disc(
    buf: &mut [u8],
    w: usize,
    h: usize,
    cx: f64,
    cy: f64,
    r: f64,
    color: [u8; 4],
    op: u32,
) {
    if !(cx.is_finite() && cy.is_finite() && r.is_finite()) || r <= 0.0 {
        return;
    }
    let sa = (color[3] as u32 * op) / 255;
    if sa == 0 {
        return;
    }
    let (cr, cg, cb) = (color[0] as u32, color[1] as u32, color[2] as u32);

    // Rows/cols whose pixel *center* (x+0.5, y+0.5) can fall inside the disc.
    let mut ya = (cy - r - 0.5).ceil() as i64;
    let mut yb = (cy + r - 0.5).floor() as i64;
    if ya < 0 {
        ya = 0;
    }
    if yb > h as i64 - 1 {
        yb = h as i64 - 1;
    }
    if ya > yb {
        return;
    }
    let mut xa0 = (cx - r - 0.5).ceil() as i64;
    let mut xb0 = (cx + r - 0.5).floor() as i64;
    if xa0 < 0 {
        xa0 = 0;
    }
    if xb0 > w as i64 - 1 {
        xb0 = w as i64 - 1;
    }
    if xa0 > xb0 {
        return;
    }

    let r2 = r * r;
    for y in ya..=yb {
        let dy = y as f64 + 0.5 - cy;
        let t = r2 - dy * dy;
        if t <= 0.0 {
            continue;
        }
        let half = t.sqrt();
        // Per-row chord, then clamp into the viewport.
        let mut xa = (cx - half - 0.5).ceil() as i64;
        let mut xb = (cx + half - 0.5).floor() as i64;
        if xa < xa0 {
            xa = xa0;
        }
        if xb > xb0 {
            xb = xb0;
        }
        if xa > xb {
            continue;
        }
        let row = y as usize * w;
        for x in xa..=xb {
            blend(buf, (row + x as usize) * 4, cr, cg, cb, sa);
        }
    }
}

/// Source-over blend of straight-alpha color with `sa` (0..=255) into one
/// RGBA pixel. Integer math; result stays within 0..=255.
#[inline(always)]
fn blend(dst: &mut [u8], o: usize, sr: u32, sg: u32, sb: u32, sa: u32) {
    let da = dst[o + 3] as u32;
    let inv = 255 - sa;
    let out_a = sa + (da * inv) / 255;
    if out_a == 0 {
        return;
    }
    dst[o] = ((sr * sa + dst[o] as u32 * da * inv / 255) / out_a) as u8;
    dst[o + 1] = ((sg * sa + dst[o + 1] as u32 * da * inv / 255) / out_a) as u8;
    dst[o + 2] = ((sb * sa + dst[o + 2] as u32 * da * inv / 255) / out_a) as u8;
    dst[o + 3] = out_a as u8;
}

/// `round(a * b / 255)` for 8-bit operands, with no division (the classic
/// multiply-shift form; exact for `a * b <= 65535`).
#[inline(always)]
fn mul255(a: u32, b: u32) -> u32 {
    let t = a * b + 128;
    (t + (t >> 8)) >> 8
}

/// Angle bucket for [`GateGeometry::key`]: degrees snapped to `step`, as i32
/// hundredths (NaN-safe — NaN folds to 0, which only risks a stale table for
/// unusable data).
fn quantize(deg: f32, step: f32) -> i32 {
    if !deg.is_finite() {
        return 0;
    }
    (deg / step).round() as i32
}

/// How a sweep's rays map onto a [`GateGeometry`]'s table rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowMap {
    /// Same azimuth per ray as the build volume.
    Identity,
    /// Rotated layout: `rows[ray]` is the table row for that ray.
    Remapped(Vec<u32>),
}

/// Max azimuth mismatch (deg) tolerated when reusing a table for a rotated
/// scan layout. Echo displacement in *screen pixels* is
/// `tol_rad * range / meters_per_px`, and range grows in lockstep with
/// meters_per_px as you zoom out — so this stays sub-pixel at every zoom
/// (measured: real consecutive volumes are exact rotations within 0.03-0.11°).
const REMAP_TOL_DEG: f32 = 0.26;

/// Cached per-gate geometry for one sweep *shape* at one site.
///
/// `gate_latlon` + Mercator projection are ~half the raster cost and they do
/// not depend on the moment values — only on the sweep's ray/gate layout and
/// the site. Positions are stored as *world* Mercator pixels at zoom 0
/// (f64): the projection is `f(lat|lon) * scale`, so scaling by `2^zoom` is
/// exact and the table serves every zoom. So: build once per shape+site,
/// then reuse across pans, zooms, moment switches, Min dBZ changes, and
/// playback frames. Consecutive volumes of one VCP report the same azimuth
/// *set* rotated by a different phase, which [`Self::reuse_for`] handles by
/// remapping rows instead of rebuilding.
pub struct GateGeometry {
    shape: u64,
    ray_count: usize,
    gate_count: usize,
    /// World Mercator px (zoom 0) per (ray, gate), row-major; NaN = not
    /// drawable.
    world_x: Vec<f64>,
    world_y: Vec<f64>,
    /// Ground footprint extent (m) per gate: radius source, zoom-independent.
    extent_m: Vec<f32>,
    /// Build-time azimuths (deg) per row, for [`Self::reuse_for`].
    azimuths: Vec<f32>,
}

/// Shortest signed angular difference `a - b`, degrees.
fn angle_delta(a: f32, b: f32) -> f32 {
    (a - b + 180.0).rem_euclid(360.0) - 180.0
}

impl GateGeometry {
    /// Hash of everything the geometry depends on *except* the azimuth
    /// phase (and *not* the view: the table is zoom-independent). Angles are
    /// quantized (0.01°) so last-bit jitter can't miss the cache.
    pub fn shape_key(sweep: &Sweep, radar_lat: f64, radar_lon: f64) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        radar_lat.to_bits().hash(&mut h);
        radar_lon.to_bits().hash(&mut h);
        quantize(sweep.elevation_deg, 0.01).hash(&mut h);
        quantize(sweep.azimuth_width_deg, 0.01).hash(&mut h);
        // Deliberately *not* in the key: the measured elevation (jitters by
        // scan — the table's ground placement moves less than a metre per
        // 0.01°), the ray count, and the azimuth-list length (L2 volumes
        // drop or gain a ray at the scan start). Those are absorbed by
        // `reuse_for`'s tolerant row mapping; a scan-to-scan difference that
        // big would otherwise rebuild this table (~90 ms!) on every playback
        // frame. Gate layout is index-strided, so it stays in the key.
        if let Some(md) = sweep.moments.first() {
            md.range_start_m.to_bits().hash(&mut h);
            md.gate_size_m.to_bits().hash(&mut h);
            md.gate_count.hash(&mut h);
        }
        h.finish()
    }

    /// Build the table for one sweep shape at one site.
    pub fn build(sweep: &Sweep, radar_lat: f64, radar_lon: f64) -> Self {
        let shape = Self::shape_key(sweep, radar_lat, radar_lon);
        let (ray_count, gate_count, range_start_m, gate_size_m) = sweep
            .moments
            .first()
            .map(|m| (m.ray_count, m.gate_count, m.range_start_m, m.gate_size_m))
            .unwrap_or((0, 0, 0.0, 0.0));
        let elevation_deg =
            if sweep.elevation_measured_deg.is_finite() && sweep.elevation_measured_deg != 0.0 {
                sweep.elevation_measured_deg
            } else {
                sweep.elevation_deg
            };
        let az_width_rad = (sweep.azimuth_width_deg as f64)
            .clamp(0.05, 45.0)
            .to_radians();

        let n = ray_count * gate_count;
        let mut world_x = vec![f64::NAN; n];
        let mut world_y = vec![f64::NAN; n];
        let mut extent_m = vec![0.0f32; n];
        for ray in 0..ray_count {
            let Some(&azimuth_deg) = sweep.azimuths_deg.get(ray) else {
                break; // rows past the azimuth table are not drawable
            };
            if !azimuth_deg.is_finite() {
                continue;
            }
            let row = ray * gate_count;
            for gate in 0..gate_count {
                let slant_m = range_start_m + (gate as f32 + 0.5) * gate_size_m;
                if !(slant_m > 0.0) {
                    continue;
                }
                let (glat, glon) =
                    gate_latlon(radar_lat, radar_lon, azimuth_deg, elevation_deg, slant_m);
                // World Mercator px at zoom 0: the projection scales by an
                // exact power of two per zoom level.
                let (gx, gy) = crate::projection::lonlat_to_merc_px(glon, glat, 0.0);
                world_x[row + gate] = gx;
                world_y[row + gate] = gy;
                extent_m[row + gate] =
                    (gate_size_m as f64).max(az_width_rad * slant_m as f64) as f32;
            }
        }
        Self {
            shape,
            ray_count,
            gate_count,
            world_x,
            world_y,
            extent_m,
            azimuths: sweep.azimuths_deg.iter().take(ray_count).copied().collect(),
        }
    }

    /// Can this table serve `sweep`? Returns how its rows map (`None` =
    /// rebuild needed). The table is zoom-independent, so pans and zooms
    /// always reuse it.
    pub fn reuse_for(&self, sweep: &Sweep, radar_lat: f64, radar_lon: f64) -> Option<RowMap> {
        if self.shape != Self::shape_key(sweep, radar_lat, radar_lon) {
            return None;
        }
        let az = &sweep.azimuths_deg;
        let n = self.ray_count;
        if az.is_empty() || n == 0 {
            return None;
        }
        // Same phase (up to last-bit jitter): identity mapping.
        if az.len() == n
            && self
                .azimuths
                .iter()
                .zip(az.iter())
                .all(|(a, b)| angle_delta(*a, *b).abs() <= 0.001)
        {
            return Some(RowMap::Identity);
        }
        // Otherwise walk both rings together: each axis ray takes the
        // *nearest* table row at or just after the previous pick. This
        // absorbs the three scan-to-scan realities — azimuth jitter, a
        // rotated scan start, and a ray dropped/added at the start (looking
        // a few rows ahead) — while a genuinely different layout blows past
        // the tolerance. Repeat picks are fine (a table row drawn twice is
        // invisible); skipping ahead is what a dropped axis ray looks like.
        let nearest_to = |target: f32| {
            let mut best = 0usize;
            let mut best_d = angle_delta(self.azimuths[0], target).abs();
            for (i, a) in self.azimuths.iter().enumerate().skip(1) {
                let d = angle_delta(*a, target).abs();
                if d < best_d {
                    best = i;
                    best_d = d;
                }
            }
            (best, best_d)
        };
        let (mut cursor, first_d) = nearest_to(az[0]);
        if first_d > REMAP_TOL_DEG {
            return None;
        }
        let mut rows = vec![0u32; az.len()];
        for (j, a) in az.iter().enumerate() {
            let mut best_d = angle_delta(self.azimuths[cursor], *a).abs();
            for step in 1..=3 {
                let i = (cursor + step) % n;
                let d = angle_delta(self.azimuths[i], *a).abs();
                if d < best_d {
                    cursor = i;
                    best_d = d;
                }
            }
            if best_d > REMAP_TOL_DEG {
                return None; // not this layout
            }
            rows[j] = cursor as u32;
        }
        if rows.iter().enumerate().all(|(j, &i)| i as usize == j) {
            return Some(RowMap::Identity);
        }
        Some(RowMap::Remapped(rows))
    }
}

/// Rasterize a moment using a prebuilt [`GateGeometry`]: per gate this is
/// one array lookup and a splat — no trig, no projection.
///
/// `rows` comes from [`GateGeometry::reuse_for`] and maps each ray onto the
/// table's rows. `view` is the *display* viewport (any size);
/// `display_scale` says the buffer is rendered at that fraction of the
/// view's size (the app's motion-quality raster). The buffer must be
/// [`scaled_view`]`(view, display_scale)`-sized, and the texture is expected
/// to be stretched across the full viewport rect — so positions scale with
/// the buffer and radii scale to match the ground footprint.
pub fn rasterize_sweep_from_geometry_pm(
    sweep: &Sweep,
    moment: Moment,
    geom: &GateGeometry,
    rows: &RowMap,
    view: &MapView,
    opacity: f32,
    min_dbz: f32,
    display_scale: f32,
    buf: &mut [u8],
) {
    let s = display_scale.clamp(0.1, 4.0) as f64;
    let bview = scaled_view(view, s as f32);
    let (w, h) = (bview.width(), bview.height());
    assert_eq!(
        buf.len(),
        w * h * 4,
        "rasterize_sweep_from_geometry_pm: buffer is {} bytes, expected {w}x{h} RGBA ({} bytes)",
        buf.len(),
        w * h * 4
    );
    if w == 0 || h == 0 || geom.ray_count == 0 || geom.gate_count == 0 {
        return;
    }
    let Some(md) = sweep.moment(moment) else {
        return;
    };
    if md.ray_count != geom.ray_count || md.gate_count != geom.gate_count {
        log::warn!(
            "rasterize_sweep_from_geometry_pm: geometry {}x{} does not fit moment {}x{}",
            geom.ray_count,
            geom.gate_count,
            md.ray_count,
            md.gate_count
        );
        return;
    }
    let opacity = opacity.clamp(0.0, 1.0);
    let op = (opacity * 255.0).round() as u32;
    if op == 0 {
        return;
    }
    let table = ColorTable::for_moment(moment);
    let (floor, _) = table.range();
    let thresh = floor.max(min_dbz);

    // World px -> buffer px: scale the zoom-0 world coordinates to this
    // zoom, subtract the view origin, then scale to the buffer grid. (All
    // power-of-two steps, so a full-resolution raster from the table is
    // bit-identical to the direct projection path.)
    let z_scale = 2f64.powf(view.zoom);
    let (ox, oy) = view.origin_px();
    // Radii: clamp in full-res display px (so the clamps mean what they say
    // at every zoom), then scale into the buffer grid.
    let m_per_px = crate::projection::meters_per_pixel(view.center_lat, view.zoom).max(1e-6);
    let rmin = MIN_GATE_RADIUS_PX * s;
    let rmax = MAX_GATE_RADIUS_PX * s;

    for ray in 0..geom.ray_count {
        // Table row for this ray's azimuth phase.
        let table_row = match rows {
            RowMap::Identity => ray,
            RowMap::Remapped(map) => match map.get(ray) {
                Some(&r) => r as usize,
                None => break, // row map shorter than the sweep: unusable
            },
        };
        let row = table_row * geom.gate_count;
        let row_base = ray * md.gate_count;
        if row_base >= md.data.len() {
            break;
        }
        for gate in 0..geom.gate_count {
            let dn = md.data[row_base + gate];
            if dn == 0 {
                continue;
            }
            let (wx, wy, ext) = (
                geom.world_x[row + gate],
                geom.world_y[row + gate],
                geom.extent_m[row + gate],
            );
            if !(wx.is_finite() && wy.is_finite()) {
                continue;
            }
            let value = (dn as f32 - md.calibration.offset) / md.calibration.scale;
            if !value.is_finite() || value < thresh {
                continue;
            }
            let color = table.map(value);
            if color[3] == 0 {
                continue;
            }
            let gx = ((wx * z_scale - ox) * s) as f64;
            let gy = ((wy * z_scale - oy) * s) as f64;
            let r = (ext as f64 * 0.5 * s / m_per_px).clamp(rmin, rmax);
            splat_disc_pm(buf, w, h, gx, gy, r, color, op);
        }
    }
}

/// The buffer-sized view for a fractional-resolution raster: same center and
/// zoom, size scaled by `scale`. The app sizes its RGBA buffer with this and
/// [`rasterize_sweep_from_geometry_pm`] rasterizes into exactly that grid.
pub fn scaled_view(view: &MapView, scale: f32) -> MapView {
    let scale = scale.clamp(0.1, 4.0);
    MapView {
        width_px: (view.width_px * scale).max(1.0),
        height_px: (view.height_px * scale).max(1.0),
        ..*view
    }
}

/// Premultiplied source-over: `dst = src + dst * (1 - src_a)`. No division
/// and no un-premultiply step, because both operands already carry
/// premultiplied RGB. With `src_pm <= src_a` and `dst_pm <= dst_a` the
/// result cannot exceed 255, so the clamps only ever fire on a rounding
/// edge.
#[inline(always)]
fn blend_pm(dst: &mut [u8], o: usize, sr: u32, sg: u32, sb: u32, sa: u32) {
    let inv = 255 - sa;
    // One bounds check for the whole pixel instead of one per channel.
    let px = &mut dst[o..o + 4];
    px[0] = (sr + mul255(px[0] as u32, inv)).min(255) as u8;
    px[1] = (sg + mul255(px[1] as u32, inv)).min(255) as u8;
    px[2] = (sb + mul255(px[2] as u32, inv)).min(255) as u8;
    px[3] = (sa + mul255(px[3] as u32, inv)).min(255) as u8;
}

/// [`splat_disc`] for a premultiplied-alpha buffer: premultiplies the color
/// once per gate, then blends division-free per pixel.
fn splat_disc_pm(
    buf: &mut [u8],
    w: usize,
    h: usize,
    cx: f64,
    cy: f64,
    r: f64,
    color: [u8; 4],
    op: u32,
) {
    if !(cx.is_finite() && cy.is_finite() && r.is_finite()) || r <= 0.0 {
        return;
    }
    let sa = (color[3] as u32 * op) / 255;
    if sa == 0 {
        return;
    }
    let (sr, sg, sb) = (
        mul255(color[0] as u32, sa),
        mul255(color[1] as u32, sa),
        mul255(color[2] as u32, sa),
    );

    // Tiny-radius fast path (the common case: min-radius splats on dense
    // data). The disc covers at most a 3x3 pixel block, where the general
    // row/chord machinery costs more than the handful of blends it saves.
    // (3x3 is the *safe* superset: with a center near a pixel edge, a point
    // 0.7 px away can land on the pixel past the nearest 2x2.)
    if r <= 1.0 {
        let x0 = cx.floor() as i64 - 1;
        let y0 = cy.floor() as i64 - 1;
        let r2 = r * r;
        for y in y0..y0 + 3 {
            if y < 0 || y >= h as i64 {
                continue;
            }
            let dy = y as f64 + 0.5 - cy;
            let row = y as usize * w;
            for x in x0..x0 + 3 {
                if x < 0 || x >= w as i64 {
                    continue;
                }
                let dx = x as f64 + 0.5 - cx;
                if dx * dx + dy * dy > r2 {
                    continue;
                }
                blend_pm(buf, (row + x as usize) * 4, sr, sg, sb, sa);
            }
        }
        return;
    }

    let mut ya = (cy - r - 0.5).ceil() as i64;
    let mut yb = (cy + r - 0.5).floor() as i64;
    if ya < 0 {
        ya = 0;
    }
    if yb > h as i64 - 1 {
        yb = h as i64 - 1;
    }
    if ya > yb {
        return;
    }
    let mut xa0 = (cx - r - 0.5).ceil() as i64;
    let mut xb0 = (cx + r - 0.5).floor() as i64;
    if xa0 < 0 {
        xa0 = 0;
    }
    if xb0 > w as i64 - 1 {
        xb0 = w as i64 - 1;
    }
    if xa0 > xb0 {
        return;
    }

    let r2 = r * r;
    for y in ya..=yb {
        let dy = y as f64 + 0.5 - cy;
        let t = r2 - dy * dy;
        if t <= 0.0 {
            continue;
        }
        let half = t.sqrt();
        let mut xa = (cx - half - 0.5).ceil() as i64;
        let mut xb = (cx + half - 0.5).floor() as i64;
        if xa < xa0 {
            xa = xa0;
        }
        if xb > xb0 {
            xb = xb0;
        }
        if xa > xb {
            continue;
        }
        let row = y as usize * w;
        for x in xa..=xb {
            blend_pm(buf, (row + x as usize) * 4, sr, sg, sb, sa);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use radar_core::{MomentCalibration, MomentData};

    /// One ray, one gate, 50 dBZ, due east of the site.
    fn single_gate_sweep() -> Sweep {
        Sweep {
            elevation_deg: 0.5,
            elevation_measured_deg: 0.5,
            azimuth_width_deg: 1.0,
            azimuths_deg: vec![90.0],
            moments: vec![MomentData {
                kind: Moment::Reflectivity,
                calibration: MomentCalibration {
                    scale: 1.0,
                    offset: 0.0,
                    threshold: 0.0,
                },
                range_start_m: 30_000.0,
                gate_size_m: 500.0,
                gate_count: 1,
                ray_count: 1,
                data: vec![50],
            }],
        }
    }

    fn view() -> MapView {
        MapView {
            center_lat: 35.0,
            center_lon: -97.0,
            zoom: 10.0,
            width_px: 800.0,
            height_px: 600.0,
        }
    }

    #[test]
    fn single_gate_projects_to_expected_pixel() {
        let sweep = single_gate_sweep();
        let view = view();
        let img = rasterize_sweep(
            &sweep,
            Moment::Reflectivity,
            &view,
            35.0,
            -97.0,
            1.0,
            f32::NEG_INFINITY,
        );

        assert_eq!(img.width, 800);
        assert_eq!(img.height, 600);
        assert_eq!(img.rgba.len(), 800 * 600 * 4);

        // Independent expectation: geolocate the gate, then project.
        let (glat, glon) = gate_latlon(35.0, -97.0, 90.0, 0.5, 30_250.0);
        let (px, py) = view.lonlat_to_view_px(glon, glat);

        // The echo must be painted at that pixel...
        let (gx, gy) = (px as usize, py as usize);
        let painted = img.pixel(gx, gy);
        assert!(
            painted[3] > 0,
            "expected paint at ({gx},{gy}), got {painted:?}"
        );
        // ...which is east of center (x grows east) and just south of center:
        // a due-east great circle starts at its vertex, so it bows slightly
        // equatorward (south) — sub-pixel-ish at 30 km.
        assert!(px > 400.0, "gate should be east of center: {px}");
        assert!(
            py > 300.0 && py < 310.0,
            "gate should sit just south of center: {py}"
        );

        // Far corner untouched.
        assert_eq!(img.pixel(0, 0), [0, 0, 0, 0]);
        assert_eq!(img.pixel(799, 599), [0, 0, 0, 0]);
    }

    #[test]
    fn no_data_and_below_floor_gates_paint_nothing() {
        let mut sweep = single_gate_sweep();
        // DN 0 => no data.
        sweep.moments[0].data[0] = 0;
        let view = view();
        let img = rasterize_sweep(
            &sweep,
            Moment::Reflectivity,
            &view,
            35.0,
            -97.0,
            1.0,
            f32::NEG_INFINITY,
        );
        assert!(
            img.rgba.iter().all(|&b| b == 0),
            "DN 0 must leave the buffer transparent"
        );
        // Premultiplied path: same promise.
        let img = rasterize_sweep_premultiplied(
            &sweep,
            Moment::Reflectivity,
            &view,
            35.0,
            -97.0,
            1.0,
            f32::NEG_INFINITY,
        );
        assert!(
            img.rgba.iter().all(|&b| b == 0),
            "DN 0 must leave the pm buffer transparent"
        );

        // Below the REF floor (5 dBZ) => no paint either.
        sweep.moments[0].data[0] = 4;
        let img = rasterize_sweep(
            &sweep,
            Moment::Reflectivity,
            &view,
            35.0,
            -97.0,
            1.0,
            f32::NEG_INFINITY,
        );
        assert!(
            img.rgba.iter().all(|&b| b == 0),
            "below-floor gates must not paint"
        );

        // Missing moment entirely => transparent, no panic.
        let empty = Sweep {
            moments: vec![],
            ..sweep
        };
        let img = rasterize_sweep(
            &empty,
            Moment::Velocity,
            &view,
            35.0,
            -97.0,
            1.0,
            f32::NEG_INFINITY,
        );
        assert!(img.rgba.iter().all(|&b| b == 0));
    }

    #[test]
    fn into_buffer_composites_over_background() {
        let sweep = single_gate_sweep();
        let view = view();
        let (w, h) = (view.width(), view.height());
        let mut buf = vec![0u8; w * h * 4];
        for px in buf.chunks_exact_mut(4) {
            px.copy_from_slice(&[10, 20, 30, 255]); // opaque basemap
        }

        let (glat, glon) = gate_latlon(35.0, -97.0, 90.0, 0.5, 30_250.0);
        let (px, py) = view.lonlat_to_view_px(glon, glat);
        let o = ((py as usize * w) + px as usize) * 4;
        let before = [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]];

        rasterize_sweep_into(
            &sweep,
            Moment::Reflectivity,
            &view,
            35.0,
            -97.0,
            0.5,
            f32::NEG_INFINITY,
            &mut buf,
        );

        let after = [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]];
        assert_eq!(after[3], 255, "opaque background stays opaque");
        assert_ne!(after, before, "color must have shifted toward the echo");
        // Source-over with 50% opacity: toward REF red-ish at 50 dBZ.
        assert!(
            after[0] > before[0],
            "red should increase: {before:?} -> {after:?}"
        );

        // Opacity 0 must be a no-op.
        let mut untouched = vec![10u8; w * h * 4];
        let snapshot = untouched.clone();
        rasterize_sweep_into(
            &sweep,
            Moment::Reflectivity,
            &view,
            35.0,
            -97.0,
            0.0,
            f32::NEG_INFINITY,
            &mut untouched,
        );
        assert_eq!(untouched, snapshot);
    }

    #[test]
    fn premultiplied_matches_straight_alpha() {
        // A multi-gate sweep so splats overlap (the interesting blend case).
        let mut sweep = single_gate_sweep();
        let md = &mut sweep.moments[0];
        md.ray_count = 4;
        md.gate_count = 40;
        md.range_start_m = 20_000.0;
        md.gate_size_m = 1000.0;
        sweep.azimuths_deg = vec![88.0, 89.0, 90.0, 91.0];
        md.data = vec![45; 4 * 40];

        let view = view();
        let straight = rasterize_sweep(
            &sweep,
            Moment::Reflectivity,
            &view,
            35.0,
            -97.0,
            0.7,
            f32::NEG_INFINITY,
        );
        let pm = rasterize_sweep_premultiplied(
            &sweep,
            Moment::Reflectivity,
            &view,
            35.0,
            -97.0,
            0.7,
            f32::NEG_INFINITY,
        );
        assert_eq!(straight.rgba.len(), pm.rgba.len());
        let mut painted = 0usize;
        for (s, p) in straight.rgba.chunks_exact(4).zip(pm.rgba.chunks_exact(4)) {
            // Alpha must agree exactly.
            assert_eq!(s[3], p[3], "alpha mismatch");
            if s[3] == 0 {
                continue;
            }
            painted += 1;
            // Premultiplied RGB == straight RGB * alpha / 255 (rounding slack).
            for c in 0..3 {
                let expect = (s[c] as u32 * s[3] as u32) / 255;
                assert!(
                    (p[c] as i32 - expect as i32).abs() <= 2,
                    "channel {c}: pm {} vs straight*alpha/255 {expect}",
                    p[c]
                );
            }
        }
        assert!(
            painted > 100,
            "fixture should paint a fair number of pixels, got {painted}"
        );
    }

    #[test]
    fn geometry_cache_matches_direct_raster() {
        let mut sweep = single_gate_sweep();
        let md = &mut sweep.moments[0];
        md.ray_count = 4;
        md.gate_count = 40;
        md.range_start_m = 20_000.0;
        md.gate_size_m = 1000.0;
        sweep.azimuths_deg = vec![88.0, 89.0, 90.0, 91.0];
        md.data = vec![45; 4 * 40];

        let view = view();
        let geom = GateGeometry::build(&sweep, 35.0, -97.0);
        let rows = geom.reuse_for(&sweep, 35.0, -97.0).expect("identity reuse");
        assert_eq!(rows, RowMap::Identity);

        let (w, h) = (view.width(), view.height());
        let mut via_geom = vec![0u8; w * h * 4];
        rasterize_sweep_from_geometry_pm(
            &sweep,
            Moment::Reflectivity,
            &geom,
            &rows,
            &view,
            0.7,
            f32::NEG_INFINITY,
            1.0,
            &mut via_geom,
        );
        let direct = rasterize_sweep_premultiplied(
            &sweep,
            Moment::Reflectivity,
            &view,
            35.0,
            -97.0,
            0.7,
            f32::NEG_INFINITY,
        );
        assert_eq!(
            via_geom, direct.rgba,
            "geometry-table raster must equal the direct path"
        );
    }

    #[test]
    fn resolution_change_keeps_echoes_georeferenced() {
        // Regression guard: at half resolution the buffer must
        // show the SAME ground area as the full-res view (just coarser) —
        // a missing position scale turned it into a 2x center crop.
        let mut sweep = single_gate_sweep();
        let md = &mut sweep.moments[0];
        md.ray_count = 4;
        md.gate_count = 40;
        md.range_start_m = 20_000.0;
        md.gate_size_m = 1000.0;
        sweep.azimuths_deg = vec![88.0, 89.0, 90.0, 91.0];
        md.data = vec![45; 4 * 40];

        let view = view();
        let geom = GateGeometry::build(&sweep, 35.0, -97.0);
        let rows = geom.reuse_for(&sweep, 35.0, -97.0).expect("identity reuse");

        // Painted-alpha centroid in normalized (0..1) viewport coordinates.
        let centroid = |buf: &[u8], w: usize, h: usize| -> (f64, f64) {
            let (mut sx, mut sy, mut n) = (0.0f64, 0.0f64, 0.0f64);
            for (i, px) in buf.chunks_exact(4).enumerate() {
                let a = px[3] as f64;
                if a > 0.0 {
                    sx += (i % w) as f64 * a;
                    sy += (i / w) as f64 * a;
                    n += a;
                }
            }
            (sx / n / w as f64, sy / n / h as f64)
        };

        let full = rasterize_sweep_premultiplied(
            &sweep,
            Moment::Reflectivity,
            &view,
            35.0,
            -97.0,
            0.7,
            f32::NEG_INFINITY,
        );
        let (fx, fy) = centroid(&full.rgba, full.width, full.height);

        let half = scaled_view(&view, 0.5);
        let (hw, hh) = (half.width(), half.height());
        let mut hbuf = vec![0u8; hw * hh * 4];
        rasterize_sweep_from_geometry_pm(
            &sweep,
            Moment::Reflectivity,
            &geom,
            &rows,
            &view,
            0.7,
            f32::NEG_INFINITY,
            0.5,
            &mut hbuf,
        );
        let (hx, hy) = centroid(&hbuf, hw, hh);

        assert!(
            (fx - hx).abs() < 0.01 && (fy - hy).abs() < 0.01,
            "half-res echo centroid ({hx:.3}, {hy:.3}) must match full-res ({fx:.3}, {fy:.3})"
        );
    }

    #[test]
    fn geometry_cache_survives_pans_and_zooms() {
        let mut sweep = single_gate_sweep();
        let md = &mut sweep.moments[0];
        md.ray_count = 4;
        md.gate_count = 40;
        md.range_start_m = 20_000.0;
        md.gate_size_m = 1000.0;
        sweep.azimuths_deg = vec![88.0, 89.0, 90.0, 91.0];
        md.data = vec![45; 4 * 40];

        let view = view();
        let geom = GateGeometry::build(&sweep, 35.0, -97.0);

        // Pan: same zoom, shifted center -> reusable, raster matches direct.
        let panned = MapView {
            center_lon: view.center_lon + 0.05,
            ..view
        };
        let rows = geom
            .reuse_for(&sweep, 35.0, -97.0)
            .expect("pan must not invalidate");
        assert_eq!(rows, RowMap::Identity);
        let (w, h) = (view.width(), view.height());
        let mut via_geom = vec![0u8; w * h * 4];
        rasterize_sweep_from_geometry_pm(
            &sweep,
            Moment::Reflectivity,
            &geom,
            &rows,
            &panned,
            0.7,
            f32::NEG_INFINITY,
            1.0,
            &mut via_geom,
        );
        let direct = rasterize_sweep_premultiplied(
            &sweep,
            Moment::Reflectivity,
            &panned,
            35.0,
            -97.0,
            0.7,
            f32::NEG_INFINITY,
        );
        assert_eq!(
            via_geom, direct.rgba,
            "translated geometry must equal a direct raster"
        );

        // Zoom: the table is zoom-independent, so it still serves — and a
        // raster at the new zoom must match a direct raster there.
        let zoomed = MapView {
            zoom: view.zoom + 0.5,
            ..view
        };
        let rows = geom
            .reuse_for(&sweep, 35.0, -97.0)
            .expect("zoom must not invalidate");
        let mut via_geom = vec![0u8; w * h * 4];
        rasterize_sweep_from_geometry_pm(
            &sweep,
            Moment::Reflectivity,
            &geom,
            &rows,
            &zoomed,
            0.7,
            f32::NEG_INFINITY,
            1.0,
            &mut via_geom,
        );
        let direct = rasterize_sweep_premultiplied(
            &sweep,
            Moment::Reflectivity,
            &zoomed,
            35.0,
            -97.0,
            0.7,
            f32::NEG_INFINITY,
        );
        assert_eq!(
            via_geom, direct.rgba,
            "zoomed raster from the cached table must match direct"
        );
    }

    #[test]
    fn rotated_layout_reuses_table_by_row_remap() {
        // Consecutive volumes start at a different azimuth, so the same
        // geometry arrives as a rotated ray list. The table must be reused
        // with remapped rows — and the output must match a direct raster
        // (a wrong row mapping would shift echoes to the wrong azimuth).
        let mk = |azimuths: Vec<f32>, ray_vals: &[i16]| {
            let rays = azimuths.len();
            let gates = 40usize;
            let mut data = vec![0i16; rays * gates];
            for (r, v) in ray_vals.iter().enumerate() {
                for g in 0..gates {
                    data[r * gates + g] = *v;
                }
            }
            Sweep {
                elevation_deg: 0.5,
                elevation_measured_deg: 0.5,
                azimuth_width_deg: 1.0,
                azimuths_deg: azimuths,
                moments: vec![radar_core::MomentData {
                    kind: Moment::Reflectivity,
                    calibration: radar_core::MomentCalibration {
                        scale: 2.0,
                        offset: 66.0,
                        threshold: 0.0,
                    },
                    range_start_m: 20_000.0,
                    gate_size_m: 1000.0,
                    gate_count: gates,
                    ray_count: rays,
                    data,
                }],
            }
        };
        // Ray-unique dBZ so a mis-mapped row is visible as a color change.
        let a = mk(vec![88.0, 89.0, 90.0, 91.0], &[66, 86, 106, 126]);
        let b = mk(vec![90.0, 91.0, 88.0, 89.0], &[106, 126, 66, 86]);

        let view = view();
        let geom = GateGeometry::build(&a, 35.0, -97.0);
        let rows = geom
            .reuse_for(&b, 35.0, -97.0)
            .expect("rotated layout must reuse the table");
        match &rows {
            RowMap::Remapped(m) => assert_eq!(m, &vec![2, 3, 0, 1], "ray -> table row map"),
            RowMap::Identity => panic!("rotated layout must remap, not claim identity"),
        }

        let (w, h) = (view.width(), view.height());
        let mut via_geom = vec![0u8; w * h * 4];
        rasterize_sweep_from_geometry_pm(
            &b,
            Moment::Reflectivity,
            &geom,
            &rows,
            &view,
            1.0,
            f32::NEG_INFINITY,
            1.0,
            &mut via_geom,
        );
        let direct = rasterize_sweep_premultiplied(
            &b,
            Moment::Reflectivity,
            &view,
            35.0,
            -97.0,
            1.0,
            f32::NEG_INFINITY,
        );
        assert_eq!(
            via_geom, direct.rgba,
            "row-remapped raster must equal the direct path"
        );
    }

    #[test]
    fn real_world_rotation_jitter_still_reuses() {
        // Measured on consecutive KTWX volumes: the layout repeats rotated
        // by an arbitrary phase with up to ~0.11 deg of jitter per ray.
        let n = 720usize;
        let a_az: Vec<f32> = (0..n)
            .map(|i| (i as f32 * 0.5 + 14.2).rem_euclid(360.0))
            .collect();
        let shift = 62usize;
        let jitter = |i: usize| (((i * 37) % 11) as f32 - 5.0) * 0.02; // ±0.1 deg
        let b_az: Vec<f32> = (0..n)
            .map(|j| (a_az[(j + shift) % n] + jitter(j)).rem_euclid(360.0))
            .collect();

        let mk = |az: Vec<f32>| Sweep {
            elevation_deg: 0.5,
            elevation_measured_deg: 0.5,
            azimuth_width_deg: 0.5,
            azimuths_deg: az,
            moments: vec![radar_core::MomentData {
                kind: Moment::Reflectivity,
                calibration: radar_core::MomentCalibration {
                    scale: 2.0,
                    offset: 66.0,
                    threshold: 0.0,
                },
                range_start_m: 500.0,
                gate_size_m: 250.0,
                gate_count: 1832,
                ray_count: n,
                data: vec![0; n * 1832],
            }],
        };
        let a = mk(a_az);
        let b = mk(b_az);
        let geom = GateGeometry::build(&a, 35.0, -97.0);
        match geom.reuse_for(&b, 35.0, -97.0) {
            Some(RowMap::Remapped(m)) => {
                assert_eq!(m.len(), n);
                // Table row 0 holds azimuth a_az[0]; the candidate ray that
                // carries it is (n - shift), so that ray maps to row 0.
                assert_eq!(
                    m[(n - shift) % n] as usize,
                    0,
                    "phase-shifted ray must map to row 0"
                );
                assert_eq!(m[(n - shift + 1) % n] as usize, 1);
            }
            other => panic!("expected a row remap for the real-world rotation, got {other:?}"),
        }
    }

    #[test]
    fn gates_lay_out_along_the_ray() {
        // Several gates along one ray: farther gates land farther east.
        let sweep = Sweep {
            elevation_deg: 0.5,
            elevation_measured_deg: 0.5,
            azimuth_width_deg: 1.0,
            azimuths_deg: vec![90.0],
            moments: vec![MomentData {
                kind: Moment::Reflectivity,
                calibration: MomentCalibration {
                    scale: 1.0,
                    offset: 0.0,
                    threshold: 0.0,
                },
                range_start_m: 10_000.0,
                gate_size_m: 10_000.0,
                gate_count: 4,
                ray_count: 1,
                data: vec![30, 30, 30, 30],
            }],
        };
        let view = view();
        let img = rasterize_sweep(
            &sweep,
            Moment::Reflectivity,
            &view,
            35.0,
            -97.0,
            1.0,
            f32::NEG_INFINITY,
        );
        let mut last_px = -1.0;
        for gate in 0..4 {
            let slant = 10_000.0 + (gate as f32 + 0.5) * 10_000.0;
            let (glat, glon) = gate_latlon(35.0, -97.0, 90.0, 0.5, slant);
            let (px, _py) = view.lonlat_to_view_px(glon, glat);
            assert!(px > last_px, "gates must march eastward");
            last_px = px;
            assert!(img.pixel(px as usize, view.lonlat_to_view_px(glon, glat).1 as usize)[3] > 0);
        }
    }
    /// The playback case this tolerant mapping exists for: consecutive
    /// volumes differ in measured elevation and drop a ray at the scan
    /// start. Both must reuse the table instead of rebuilding it (~90 ms).
    #[test]
    fn reuse_absorbs_measured_elevation_jitter_and_a_missing_start_ray() {
        use radar_core::{MomentCalibration, MomentData, Sweep};
        let cal = MomentCalibration {
            scale: 1.0,
            offset: 2.0,
            threshold: 0.0,
        };
        let mk = |azimuths: Vec<f32>, measured: f32| Sweep {
            elevation_deg: 0.5,
            elevation_measured_deg: measured,
            azimuth_width_deg: 0.5,
            azimuths_deg: azimuths.clone(),
            moments: vec![MomentData {
                kind: radar_core::Moment::Reflectivity,
                calibration: cal,
                range_start_m: 2125.0,
                gate_size_m: 250.0,
                gate_count: 64,
                ray_count: azimuths.len(),
                data: vec![7; azimuths.len() * 64],
            }],
        };
        // 8 rays, 45° apart.
        let a: Vec<f32> = (0..8).map(|i| i as f32 * 45.0).collect();
        let geom = GateGeometry::build(&mk(a.clone(), 0.51), 35.0, -97.0);

        // Measured elevation jitter alone: still the identity.
        let jittered = mk(a.clone(), 0.53);
        assert_eq!(
            geom.reuse_for(&jittered, 35.0, -97.0),
            Some(RowMap::Identity),
            "0.02° of measured-elevation jitter must not rebuild"
        );

        // A dropped first ray: the rest must map onto rows 1..=7.
        let missing: Vec<f32> = (1..8).map(|i| i as f32 * 45.0).collect();
        match geom.reuse_for(&mk(missing, 0.52), 35.0, -97.0) {
            Some(RowMap::Remapped(rows)) => {
                assert_eq!(rows, vec![1, 2, 3, 4, 5, 6, 7], "ray -> table row");
            }
            other => panic!("expected a remapped reuse, got {other:?}"),
        }
    }
}
