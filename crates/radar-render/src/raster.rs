//! Sweep rasterization into an RGBA viewport overlay. (W3)
//!
//! For every ray/gate of a [`Sweep`] the gate is geolocated with the
//! 4/3-earth model ([`gate_latlon`]), projected into the viewport with the
//! shared [`MapView`] mapping, and splatted as a small disc sized to the
//! gate's screen-space footprint, alpha-composited over whatever is already
//! in the buffer.

use crate::colormap::ColorTable;
use crate::projection::{MercProjector, MapView};
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
        Self { width: w, height: h, rgba: vec![0u8; w * h * 4] }
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
    rasterize_sweep_into(sweep, moment, view, radar_lat, radar_lon, opacity, min_dbz, &mut img.rgba);
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
    let elevation_deg = if sweep.elevation_measured_deg.is_finite() && sweep.elevation_measured_deg != 0.0 {
        sweep.elevation_measured_deg
    } else {
        sweep.elevation_deg
    };
    let az_width_rad = (sweep.azimuth_width_deg as f64).clamp(0.05, 45.0).to_radians();
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
            let (glat, glon) = gate_latlon(radar_lat, radar_lon, azimuth_deg, elevation_deg, slant_m);
            let (px, py) = proj.view_px(glon, glat);

            // Screen-space footprint of this gate: radial (gate size) vs
            // tangential (azimuth width at this range), whichever is larger.
            let extent_m = (md.gate_size_m as f64).max(az_width_rad * slant_m as f64);
            let radius = (0.5 * extent_m / m_per_px).clamp(MIN_GATE_RADIUS_PX, MAX_GATE_RADIUS_PX);

            splat_disc(buf, w, h, px, py, radius, color, op);
        }
    }
}

/// Paint a filled disc of straight-alpha `color` (scaled by `op`/255) centered
/// at (cx, cy) in viewport pixels, source-over composited into `buf`.
fn splat_disc(buf: &mut [u8], w: usize, h: usize, cx: f64, cy: f64, r: f64, color: [u8; 4], op: u32) {
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
                calibration: MomentCalibration { scale: 1.0, offset: 0.0, threshold: 0.0 },
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
        let img = rasterize_sweep(&sweep, Moment::Reflectivity, &view, 35.0, -97.0, 1.0);

        assert_eq!(img.width, 800);
        assert_eq!(img.height, 600);
        assert_eq!(img.rgba.len(), 800 * 600 * 4);

        // Independent expectation: geolocate the gate, then project.
        let (glat, glon) = gate_latlon(35.0, -97.0, 90.0, 0.5, 30_250.0);
        let (px, py) = view.lonlat_to_view_px(glon, glat);

        // The echo must be painted at that pixel...
        let (gx, gy) = (px as usize, py as usize);
        let painted = img.pixel(gx, gy);
        assert!(painted[3] > 0, "expected paint at ({gx},{gy}), got {painted:?}");
        // ...which is east of center (x grows east) and just south of center:
        // a due-east great circle starts at its vertex, so it bows slightly
        // equatorward (south) — sub-pixel-ish at 30 km.
        assert!(px > 400.0, "gate should be east of center: {px}");
        assert!(py > 300.0 && py < 310.0, "gate should sit just south of center: {py}");

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
        let img = rasterize_sweep(&sweep, Moment::Reflectivity, &view, 35.0, -97.0, 1.0);
        assert!(img.rgba.iter().all(|&b| b == 0), "DN 0 must leave the buffer transparent");

        // Below the REF floor (5 dBZ) => no paint either.
        sweep.moments[0].data[0] = 4;
        let img = rasterize_sweep(&sweep, Moment::Reflectivity, &view, 35.0, -97.0, 1.0);
        assert!(img.rgba.iter().all(|&b| b == 0), "below-floor gates must not paint");

        // Missing moment entirely => transparent, no panic.
        let empty = Sweep { moments: vec![], ..sweep };
        let img = rasterize_sweep(&empty, Moment::Velocity, &view, 35.0, -97.0, 1.0);
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

        rasterize_sweep_into(&sweep, Moment::Reflectivity, &view, 35.0, -97.0, 0.5, &mut buf);

        let after = [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]];
        assert_eq!(after[3], 255, "opaque background stays opaque");
        assert_ne!(after, before, "color must have shifted toward the echo");
        // Source-over with 50% opacity: toward REF red-ish at 50 dBZ.
        assert!(after[0] > before[0], "red should increase: {before:?} -> {after:?}");

        // Opacity 0 must be a no-op.
        let mut untouched = vec![10u8; w * h * 4];
        let snapshot = untouched.clone();
        rasterize_sweep_into(&sweep, Moment::Reflectivity, &view, 35.0, -97.0, 0.0, &mut untouched);
        assert_eq!(untouched, snapshot);
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
                calibration: MomentCalibration { scale: 1.0, offset: 0.0, threshold: 0.0 },
                range_start_m: 10_000.0,
                gate_size_m: 10_000.0,
                gate_count: 4,
                ray_count: 1,
                data: vec![30, 30, 30, 30],
            }],
        };
        let view = view();
        let img = rasterize_sweep(&sweep, Moment::Reflectivity, &view, 35.0, -97.0, 1.0);
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
}
