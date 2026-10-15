//! Synthetic radar fixture — builds a plausible storm (reflectivity blob +
//! velocity couplet) as a `radar_core::Sweep`, rasterizes it over a
//! `MapView` centered on an invented radar site, composites over a solid
//! basemap color, and writes `target/example-render.png`.
//!
//! No network access is required. Run from the workspace root:
//!
//! ```sh
//! cargo run -p radar-render --example render_fixture
//! ```

use radar_core::{Moment, MomentCalibration, MomentData, Sweep};
use radar_render::{MapView, rasterize_sweep_into};

const RADAR_LAT: f64 = 35.3331;
const RADAR_LON: f64 = -97.2778;

const RAYS: usize = 360;
const GATES: usize = 400;
const RANGE_START_M: f32 = 500.0;
const GATE_SIZE_M: f32 = 250.0;

/// Deterministic speckle so the echo edge isn't perfectly smooth.
fn speckle(ray: usize, gate: usize) -> f64 {
    (((ray * 7 + gate * 13) % 11) as f64 - 5.0) * 0.12 // -0.6 ..= 0.6 dBZ
}

/// Planar distance (m) from a polar point (r, azimuth) to a cell centered at
/// (rc, azc). Good enough for a fixture well inside one hemisphere.
fn cell_dist(r: f64, az_deg: f64, azc_deg: f64, rc: f64) -> f64 {
    let mut daz = az_deg - azc_deg;
    while daz > 180.0 {
        daz -= 360.0;
    }
    while daz < -180.0 {
        daz += 360.0;
    }
    let d = daz.to_radians();
    (r * r + rc * rc - 2.0 * r * rc * d.cos()).max(0.0).sqrt()
}

fn build_sweep() -> Sweep {
    let mut refl = vec![0i16; RAYS * GATES];
    let mut vel = vec![0i16; RAYS * GATES];

    for ray in 0..RAYS {
        let az = ray as f64; // 0..359 deg, 1 deg rays
        for gate in 0..GATES {
            let r = RANGE_START_M as f64 + (gate as f64 + 0.5) * GATE_SIZE_M as f64;

            // ---- Reflectivity: two gaussian storm cells + speckle ----
            let d1 = cell_dist(r, az, 60.0, 45_000.0); // main cell, NE
            let v1 = 5.0 + 58.0 * (-(d1 * d1) / (2.0 * 14_000.0 * 14_000.0)).exp();
            let d2 = cell_dist(r, az, 95.0, 30_000.0); // second cell
            let v2 = 5.0 + 50.0 * (-(d2 * d2) / (2.0 * 9_000.0 * 9_000.0)).exp();
            let v_raw = v1.max(v2);
            // calibration: physical = (dn - 66) / 2  =>  dn = 2v + 66
            let v = v_raw + speckle(ray, gate);
            refl[ray * GATES + gate] = if v_raw < 6.0 {
                0 // below the useful floor: no data
            } else {
                (v * 2.0 + 66.0).round().clamp(0.0, i16::MAX as f64) as i16
            };

            // ---- Velocity: rotational couplet at az 250 deg, 32 km ----
            let mut daz = az - 250.0;
            while daz > 180.0 {
                daz -= 360.0;
            }
            while daz < -180.0 {
                daz += 360.0;
            }
            let cross_m = r * daz.to_radians(); // + = east of couplet center
            let along = r - 32_000.0;
            let shape = (-(cross_m * cross_m) / (2.0 * 3_500.0 * 3_500.0)).exp()
                * (-(along * along) / (2.0 * 7_000.0 * 7_000.0)).exp();
            let v = 34.0 * shape * cross_m.signum(); // inbound (green) west, outbound (red) east
            vel[ray * GATES + gate] = if v.abs() < 1.0 {
                0 // weak/no data: keeps the zero-motion line clean
            } else {
                (v + 128.0).round() as i16 // physical = dn - 128
            };
        }
    }

    Sweep {
        elevation_deg: 0.5,
        elevation_measured_deg: 0.5,
        azimuth_width_deg: 1.0,
        azimuths_deg: (0..RAYS).map(|i| i as f32).collect(),
        moments: vec![
            MomentData {
                kind: Moment::Reflectivity,
                calibration: MomentCalibration {
                    scale: 2.0,
                    offset: 66.0,
                    threshold: 0.0,
                },
                range_start_m: RANGE_START_M,
                gate_size_m: GATE_SIZE_M,
                gate_count: GATES,
                ray_count: RAYS,
                data: refl,
            },
            MomentData {
                kind: Moment::Velocity,
                calibration: MomentCalibration {
                    scale: 1.0,
                    offset: 128.0,
                    threshold: 0.0,
                },
                range_start_m: RANGE_START_M,
                gate_size_m: GATE_SIZE_M,
                gate_count: GATES,
                ray_count: RAYS,
                data: vel,
            },
        ],
    }
}

fn main() -> anyhow::Result<()> {
    let sweep = build_sweep();
    let view = MapView {
        center_lat: RADAR_LAT,
        center_lon: RADAR_LON,
        zoom: 9.0,
        width_px: 1280.0,
        height_px: 800.0,
    };
    let (w, h) = (view.width(), view.height());

    // Solid "Carto dark"-ish basemap background.
    let mut buf = vec![0u8; w * h * 4];
    for px in buf.chunks_exact_mut(4) {
        px.copy_from_slice(&[16, 18, 24, 255]);
    }

    // Layers: reflectivity first, then the velocity couplet on top, both
    // alpha-composited into the same buffer (no reallocation).
    rasterize_sweep_into(
        &sweep,
        Moment::Reflectivity,
        &view,
        RADAR_LAT,
        RADAR_LON,
        0.9,
        f32::NEG_INFINITY,
        &mut buf,
    );
    rasterize_sweep_into(
        &sweep,
        Moment::Velocity,
        &view,
        RADAR_LAT,
        RADAR_LON,
        0.8,
        f32::NEG_INFINITY,
        &mut buf,
    );

    // White cross marking the radar site (the view center).
    let (cx, cy) = (w / 2, h / 2);
    for d in -7i32..=7 {
        for (x, y) in [(cx as i32 + d, cy as i32), (cx as i32, cy as i32 + d)] {
            if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
                let o = ((y as usize) * w + x as usize) * 4;
                buf[o..o + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }

    // Count echo pixels (anything that isn't the solid basemap color).
    let echo_px = buf
        .chunks_exact(4)
        .filter(|p| [p[0], p[1], p[2]] != [16, 18, 24])
        .count();
    let out = std::path::Path::new("target/example-render.png");
    std::fs::create_dir_all("target")?;
    image::save_buffer(out, &buf, w as u32, h as u32, image::ColorType::Rgba8)?;

    let meta = std::fs::metadata(out)?;
    println!(
        "wrote {} ({} bytes), {w}x{h}, {echo_px} echo/marker px of {}",
        out.display(),
        meta.len(),
        w * h
    );
    anyhow::ensure!(meta.len() > 0, "output PNG is empty");
    anyhow::ensure!(echo_px > 10_000, "suspiciously few echo pixels: {echo_px}");
    Ok(())
}
