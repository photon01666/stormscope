//! Headless smoke test for the 3D volume renderer: builds a synthetic
//! supercell-like volume, pushes it through the real [`Volume3d`] pipeline
//! on whatever GPU adapter is available (even lavapipe/llvmpipe on a bare
//! runner), and writes the frame to a PNG.
//!
//! Ignored by default: it needs a GPU adapter, which CI runners do not
//! reliably have. Run it locally with:
//!
//! ```text
//! cargo test -p stormscope-app --test render3d_smoke -- --ignored --nocapture
//! ```
//!
//! The PNG lands in `target/tmp/render3d-smoke.png` — the point of the test
//! is as much "look at it" as the pixel assertions.

use chrono::TimeZone;
use radar_core::{Moment, MomentCalibration, MomentData, Sweep, Volume, VolumeMeta};
use radar_render::volume3d::{Camera, MeshDetail, MeshFilter};
use stormscope_app::render3d::Volume3d;

/// A plausible supercell: a low-level core with a hook, a bright-band
/// stratiform shield, a top anvil, and the whole storm leaning with height
/// (which is what makes a 3D view interesting — each tilt samples a
/// different part of the storm).
fn supercell_dbz(x: f32, y: f32, h: f32) -> f32 {
    // Updraft core, leaning to the NE with height: at 10 km it sits ~5.5 km
    // downshear of its low-level position — a visible overhang.
    let cx = 2.0 + 0.55 * h;
    let cy = 5.0 + 0.35 * h;
    let r2 = (x - cx).powi(2) + (y - cy).powi(2);
    let core = (70.0 - 2.2 * h) * (-r2 / (2.0 * 9.0 * 9.0)).exp();
    // Hook echo, low levels only.
    let hook_r2 = (x + 3.0).powi(2) + (y - 1.0).powi(2);
    let hook = 52.0 * (-hook_r2 / (2.0 * 5.0 * 5.0)).exp() * (-((h - 1.0) / 2.5).powi(2)).exp();
    // Stratiform shield with a bright band at ~2.5 km.
    let shield =
        33.0 * (-(x * x + y * y) / (2.0 * 45.0 * 45.0)).exp() * (-((h - 2.5) / 3.0).powi(2)).exp();
    // Anvil streaming off the top.
    let anvil =
        22.0 * (-((h - 9.5) / 2.0).powi(2)).exp() * (-(x * x + y * y) / (2.0 * 70.0 * 70.0)).exp();
    let all = [core, hook, shield, anvil];
    all.iter().cloned().fold(f32::MIN, f32::max)
}

fn synthetic_volume() -> Volume {
    let elevations = [0.5f32, 0.9, 1.3, 1.8, 2.4, 3.1, 4.0, 5.1, 6.4, 8.0];
    let cal = MomentCalibration {
        scale: 1.0,
        offset: 2.0,
        threshold: 0.0,
    };
    let rays = 360usize;
    let gates = 1200usize; // 300 km at 250 m gates
    let gate_size = 250.0f32;
    let range_start = 2125.0f32;

    let sweeps = elevations
        .iter()
        .map(|&elev| {
            let mut data = vec![0i16; rays * gates];
            for ray in 0..rays {
                let az = ray as f32; // 1° apart
                for g in 0..gates {
                    let r_km = (range_start + (g as f32 + 0.5) * gate_size) / 1000.0;
                    let ground = r_km * elev.to_radians().cos();
                    let x = ground * az.to_radians().sin();
                    let y = ground * az.to_radians().cos();
                    let h = radar_render::volume3d::beam_height_km(r_km, elev);
                    let dbz = supercell_dbz(x, y, h);
                    if dbz >= 5.0 {
                        let dn = ((dbz - cal.offset) / cal.scale).round() as i16;
                        data[ray * gates + g] = dn.max(1);
                    }
                }
            }
            Sweep {
                elevation_deg: elev,
                elevation_measured_deg: elev + 0.02,
                azimuth_width_deg: 1.0,
                azimuths_deg: (0..rays).map(|i| i as f32).collect(),
                moments: vec![MomentData {
                    kind: Moment::Reflectivity,
                    calibration: cal,
                    range_start_m: range_start,
                    gate_size_m: gate_size,
                    gate_count: gates,
                    ray_count: rays,
                    data,
                }],
            }
        })
        .collect();

    Volume {
        meta: VolumeMeta {
            site_id: "KTWX".into(),
            volume_start: chrono::Utc.with_ymd_and_hms(2026, 10, 16, 6, 0, 0).unwrap(),
            vcp: Some(212),
            sequence_number: Some(1),
            sweep_count: elevations.len(),
            moments: vec![Moment::Reflectivity],
        },
        sweeps,
    }
}

#[test]
#[ignore = "needs a GPU adapter; run locally with --ignored --nocapture"]
fn renders_a_synthetic_supercell_to_a_png() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .expect("no GPU adapter available");
    eprintln!("adapter: {:?}", adapter.get_info());
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("device request failed");

    let volume = synthetic_volume();
    let mut v3 = Volume3d::new(&device, &queue);
    let filter = MeshFilter::new(Moment::Reflectivity, MeshDetail::Normal);
    let rebuilt = v3.update_scene(&device, &queue, &volume, &filter, 1);
    assert!(rebuilt);
    eprintln!(
        "scene: {} tilts, {:.0} km range, {} tris, {} verts",
        v3.tilt_count(),
        v3.max_range_km(),
        v3.triangles(),
        v3.vertex_count()
    );
    assert!(v3.triangles() > 50_000, "expected a substantial mesh");
    assert!(v3.tilt_count() == 10);

    let size = [1280u32, 720u32];
    // Mirror the pane's default auto-fit so the PNG represents the app's
    // opening view (1.35 × range, looking a touch above the ground).
    let camera = Camera {
        yaw_deg: 25.0,
        pitch_deg: 30.0,
        distance_km: v3.max_range_km() * 1.35,
        target_km: [0.0, 0.0, v3.max_range_km() * 0.02],
    };
    // 6× — the app's default height scale — and the full altitude slab.
    v3.render(&device, &queue, size, &camera, 6.0, 0.6, 5.0, 0.0, 20.0);

    // Read the rendered frame back.
    let texture = v3.color_texture().expect("render created the target");
    let bytes_per_row = size[0] * 4;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("smoke-readback"),
        size: u64::from(bytes_per_row) * u64::from(size[1]),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("smoke-readback-encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(size[1]),
            },
        },
        wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    let (tx, rx) = std::sync::mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll");
    rx.recv().expect("map callback").expect("map_async failed");
    let data = buffer
        .slice(..)
        .get_mapped_range()
        .expect("mapped range")
        .to_vec();
    buffer.unmap();

    // Background is the clear color (≈ 11, 14, 19). The volume plus the
    // ground grid must light up a large share of the frame.
    let bg = [11u8, 14, 19];
    let mut lit = 0usize;
    let mut greenish = 0usize;
    for px in data.chunks_exact(4) {
        let diff = u32::from(px[0].abs_diff(bg[0]))
            + u32::from(px[1].abs_diff(bg[1]))
            + u32::from(px[2].abs_diff(bg[2]));
        if diff > 12 {
            lit += 1;
        }
        if px[1] > 110 && px[1] > px[0] && px[1] > px[2] {
            greenish += 1;
        }
    }
    eprintln!(
        "pixels: {lit} lit, {greenish} green-ish of {}",
        data.len() / 4
    );

    // Dump a PNG for eyeballing — before the assertions so a failing
    // expectation still leaves the frame to look at.
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("render3d-smoke.png");
    let file = std::fs::File::create(&path).expect("create png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), size[0], size[1]);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .expect("png header")
        .write_image_data(&data)
        .expect("png data");
    eprintln!("wrote {}", path.display());

    assert!(lit > 40_000, "scene drew almost nothing ({lit} lit pixels)");
    assert!(greenish > 8_000, "no echo-colored pixels ({greenish})");
}
