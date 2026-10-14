//! Rasterization timing harness: heavy synthetic storm × the two splat
//! paths (straight-alpha vs premultiplied), at a typical viewport size.
//!
//! ```sh
//! cargo run -p radar-render --example raster_bench --release
//! ```

use radar_core::{Moment, MomentCalibration, MomentData, Sweep};
use radar_render::{rasterize_sweep_into, rasterize_sweep_premultiplied_into, MapView};
use std::time::Instant;

const RADAR_LAT: f64 = 35.3331;
const RADAR_LON: f64 = -97.2778;
const RAYS: usize = 720;
const GATES: usize = 1200;
const RANGE_START_M: f32 = 500.0;
const GATE_SIZE_M: f32 = 250.0;

fn speckle(ray: usize, gate: usize) -> f64 {
    (((ray * 7 + gate * 13) % 11) as f64 - 5.0) * 0.12
}

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

/// A broad, echo-dense storm plus a weak-return haze over the whole range:
/// the Min dBZ 0 / clear-air worst case where nearly every gate splats.
fn build_sweep() -> Sweep {
    let mut refl = vec![0i16; RAYS * GATES];
    for ray in 0..RAYS {
        let az = ray as f64;
        for gate in 0..GATES {
            let r = RANGE_START_M as f64 + (gate as f64 + 0.5) * GATE_SIZE_M as f64;
            let d1 = cell_dist(r, az, 60.0, 40_000.0);
            let v1 = 5.0 + 58.0 * (-(d1 * d1) / (2.0 * 22_000.0 * 22_000.0)).exp();
            let d2 = cell_dist(r, az, 200.0, 35_000.0);
            let v2 = 5.0 + 52.0 * (-(d2 * d2) / (2.0 * 18_000.0 * 18_000.0)).exp();
            // Clutter/haze: weak returns almost everywhere (Min dBZ 0 view).
            let haze = 3.0 + 4.0 * ((ray * 13 + gate * 7) % 23) as f64 / 23.0;
            let v_raw = v1.max(v2).max(haze);
            let v = v_raw + speckle(ray, gate);
            refl[ray * GATES + gate] = (v * 2.0 + 66.0).round().clamp(0.0, i16::MAX as f64) as i16;
        }
    }
    Sweep {
        elevation_deg: 0.5,
        elevation_measured_deg: 0.5,
        azimuth_width_deg: 1.0,
        azimuths_deg: (0..RAYS).map(|i| i as f32).collect(),
        moments: vec![MomentData {
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
        }],
    }
}

fn main() -> anyhow::Result<()> {
    let sweep = build_sweep();
    let view = MapView {
        center_lat: RADAR_LAT,
        center_lon: RADAR_LON,
        zoom: 8.5, // whole storm on screen: many gates per pixel
        width_px: 1920.0,
        height_px: 1080.0,
    };
    let (w, h) = (view.width(), view.height());
    const N: usize = 10;

    // Warm up + time the straight-alpha path.
    let mut buf = vec![0u8; w * h * 4];
    rasterize_sweep_into(
        &sweep, Moment::Reflectivity, &view, RADAR_LAT, RADAR_LON, 0.5, 0.0, &mut buf,
    );
    let t0 = Instant::now();
    for _ in 0..N {
        buf.fill(0);
        rasterize_sweep_into(
            &sweep, Moment::Reflectivity, &view, RADAR_LAT, RADAR_LON, 0.5, 0.0, &mut buf,
        );
    }
    let straight = t0.elapsed().as_secs_f64() / N as f64;

    // Premultiplied path.
    let mut buf = vec![0u8; w * h * 4];
    rasterize_sweep_premultiplied_into(
        &sweep, Moment::Reflectivity, &view, RADAR_LAT, RADAR_LON, 0.5, 0.0, &mut buf,
    );
    let t1 = Instant::now();
    for _ in 0..N {
        buf.fill(0);
        rasterize_sweep_premultiplied_into(
            &sweep, Moment::Reflectivity, &view, RADAR_LAT, RADAR_LON, 0.5, 0.0, &mut buf,
        );
    }
    let premul = t1.elapsed().as_secs_f64() / N as f64;

    // Isolate the per-gate geolocation + projection cost (no splat writes).
    let t2 = Instant::now();
    let mut acc = 0.0f64;
    for _ in 0..N {
        for ray in 0..RAYS {
            let az = ray as f32;
            for gate in 0..GATES {
                let slant = RANGE_START_M + (gate as f32 + 0.5) * GATE_SIZE_M;
                let (glat, glon) = radar_render::gate_latlon(RADAR_LAT, RADAR_LON, az, 0.5, slant);
                let (px, py) = view.lonlat_to_view_px(glon, glat);
                acc += px + py;
            }
        }
    }
    let geom_time = t2.elapsed().as_secs_f64() / N as f64;
    std::hint::black_box(acc);

    // Cached-geometry path: build once, then rasterize repeatedly (the
    // playback / pan / moment-switch case).
    let t3 = Instant::now();
    let geom = radar_render::GateGeometry::build(&sweep, RADAR_LAT, RADAR_LON);
    let build = t3.elapsed().as_secs_f64();
    let rows = geom
        .reuse_for(&sweep, RADAR_LAT, RADAR_LON)
        .expect("identity reuse");

    let mut buf = vec![0u8; w * h * 4];
    let t4 = Instant::now();
    for _ in 0..N {
        buf.fill(0);
        radar_render::rasterize_sweep_from_geometry_pm(
            &sweep, Moment::Reflectivity, &geom, &rows, &view, 0.5, 0.0, 1.0, &mut buf,
        );
    }
    let cached = t4.elapsed().as_secs_f64() / N as f64;

    // Pan: same zoom, translated center (rebuild-free).
    let panned = MapView { center_lon: view.center_lon + 0.02, ..view };
    let t5 = Instant::now();
    for _ in 0..N {
        buf.fill(0);
        radar_render::rasterize_sweep_from_geometry_pm(
            &sweep, Moment::Reflectivity, &geom, &rows, &panned, 0.5, 0.0, 1.0, &mut buf,
        );
    }
    let pan = t5.elapsed().as_secs_f64() / N as f64;

    // Split the cached cost: rasterize into a 1x1 viewport (same zoom, so the
    // geometry table is reused) — every gate still runs the value/color
    // mapping and the splat *call*, but all splats clip, so the difference
    // against the full viewport isolates the actual pixel work.
    let tiny = MapView { width_px: 1.0, height_px: 1.0, ..view };
    let mut tiny_buf = vec![0u8; 4];
    let t6 = Instant::now();
    for _ in 0..N {
        tiny_buf.fill(0);
        radar_render::rasterize_sweep_from_geometry_pm(
            &sweep, Moment::Reflectivity, &geom, &rows, &tiny, 0.5, 0.0, 1.0, &mut tiny_buf,
        );
    }
    let gate_only = t6.elapsed().as_secs_f64() / N as f64;

    // Motion-resolution path (what the app uses while dragging/playing):
    // half-size viewport, display_scale 0.5.
    let mview = radar_render::scaled_view(&view, 0.5);
    let (mw, mh) = (mview.width(), mview.height());
    let mut mbuf = vec![0u8; mw * mh * 4];
    let t7 = Instant::now();
    for _ in 0..N {
        mbuf.fill(0);
        radar_render::rasterize_sweep_from_geometry_pm(
            &sweep, Moment::Reflectivity, &geom, &rows, &view, 0.5, 0.0, 0.5, &mut mbuf,
        );
    }
    let motion = t7.elapsed().as_secs_f64() / N as f64;

    println!(
        "{w}x{h} heavy storm, {} rays x {} gates:\n  straight-alpha:   {:.1} ms\n  premultiplied:    {:.1} ms\n  geometry only:    {:.1} ms\n  cached geometry:  {:.1} ms (build once: {:.1} ms)\n  cached + panned:  {:.1} ms\n  gate loop only:   {:.1} ms\n  => pixel work:    {:.1} ms\n  motion (half-res):{:.1} ms",
        RAYS,
        GATES,
        straight * 1000.0,
        premul * 1000.0,
        geom_time * 1000.0,
        cached * 1000.0,
        build * 1000.0,
        pan * 1000.0,
        gate_only * 1000.0,
        (cached - gate_only) * 1000.0,
        motion * 1000.0,
    );
    Ok(())
}
