//! Integration tests against a real downloaded KTLX Level II volume.
//!
//! The fixture lives in `tests/fixtures/` (gitignored); if it is missing it
//! is fetched once via `tests/fixtures/download.sh` (anonymous HTTPS from the
//! public `unidata-nexrad-level2-chunks` bucket).

use std::path::PathBuf;
use std::process::Command;

use chrono::Datelike;
use radar_core::Moment;
use radar_decode::{decode_realtime_volume, decode_volume_file};

/// The fixture currently used: KTLX volume scan 984, started
/// 2026-10-12 00:57:16 UTC (V06 super-resolution, dual-pol).
const FIXTURE: &str = "KTLX20261012_005716_V06.ar2v.gz";

fn fixture_path() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(FIXTURE);
    if !path.exists() {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/download.sh");
        let status = Command::new("sh")
            .arg(&script)
            .status()
            .expect("failed to run tests/fixtures/download.sh");
        assert!(status.success(), "fixture download failed: {status}");
    }
    path
}

fn print_volume_stats(label: &str, volume: &radar_core::Volume) {
    let meta = &volume.meta;
    println!(
        "[{label}] site={} start={} vcp={:?} seq={:?} sweeps={} moments={:?}",
        meta.site_id,
        meta.volume_start,
        meta.vcp,
        meta.sequence_number,
        meta.sweep_count,
        meta.moments.iter().map(|m| m.label()).collect::<Vec<_>>(),
    );
    println!("[{label}] tilts: {:?}", volume.tilts());
    if let Some(idx) = volume.sweep_index_for_tilt(0.5) {
        let sweep = &volume.sweeps[idx];
        println!(
            "[{label}] lowest tilt: requested={:.2} measured={:.2} az_width={} rays={}",
            sweep.elevation_deg,
            sweep.elevation_measured_deg,
            sweep.azimuth_width_deg,
            sweep.azimuths_deg.len(),
        );
        for m in &sweep.moments {
            let mut nonzero = 0usize;
            let mut min = f32::MAX;
            let mut max = f32::MIN;
            for ray in 0..m.ray_count {
                for gate in 0..m.gate_count {
                    if let Some(v) = m.value(ray, gate) {
                        nonzero += 1;
                        min = min.min(v);
                        max = max.max(v);
                    }
                }
            }
            println!(
                "[{label}]   {}: gates={} rays={} range_start={:.0}m gate={:.0}m cal=({:.1},{:.1},{:.1}) nonzero={} min={:.1} max={:.1} {}",
                m.kind.label(),
                m.gate_count,
                m.ray_count,
                m.range_start_m,
                m.gate_size_m,
                m.calibration.scale,
                m.calibration.offset,
                m.calibration.threshold,
                nonzero,
                min,
                max,
                m.kind.unit(),
            );
        }
    }
}

/// Sanity checks shared by both decode paths.
fn assert_sane_volume(volume: &radar_core::Volume) {
    assert_eq!(volume.meta.site_id, "KTLX");
    assert!(volume.meta.sweep_count > 0, "no sweeps decoded");
    assert!(volume.meta.vcp.is_some(), "VCP number missing");
    assert!(volume.meta.moments.contains(&Moment::Reflectivity));
    assert!(!volume.sweeps.is_empty());

    // Volume start is the day of the fixture.
    let start = volume.meta.volume_start;
    assert_eq!(
        (
            start.date_naive().year(),
            start.date_naive().month(),
            start.date_naive().day()
        ),
        (2026, 10, 12)
    );

    // Lowest tilt is ~0.5 deg.
    let tilts = volume.tilts();
    assert!(
        tilts.iter().any(|t| (t - 0.5).abs() <= 0.2),
        "no ~0.5 deg tilt; tilts: {tilts:?}"
    );

    // The lowest tilt carries reflectivity with plausible geometry and data.
    let idx = volume.sweep_index_for_tilt(0.5).expect("tilt lookup");
    let sweep = &volume.sweeps[idx];
    let refl = sweep
        .moment(Moment::Reflectivity)
        .expect("REF on lowest tilt");
    assert!(
        refl.gate_count >= 200,
        "REF gate count too small: {}",
        refl.gate_count
    );
    assert!(
        refl.ray_count >= 300,
        "REF ray count too small: {}",
        refl.ray_count
    );
    assert_eq!(refl.data.len(), refl.ray_count * refl.gate_count);
    assert!(refl.calibration.scale > 0.0);

    let mut dbz = Vec::new();
    for r in 0..refl.ray_count {
        for g in 0..refl.gate_count {
            if let Some(v) = refl.value(r, g) {
                dbz.push(v);
            }
        }
    }
    assert!(!dbz.is_empty(), "lowest-tilt REF has no nonzero gates");
    let max_dbz = dbz.iter().cloned().fold(f32::MIN, f32::max);
    let min_dbz = dbz.iter().cloned().fold(f32::MAX, f32::min);
    assert!(
        (-10.0..=85.0).contains(&max_dbz),
        "implausible max reflectivity: {max_dbz} dBZ"
    );
    assert!(
        min_dbz >= -40.0,
        "implausible min reflectivity: {min_dbz} dBZ"
    );

    // Every sweep's moment matrices are consistently sized.
    for (i, sweep) in volume.sweeps.iter().enumerate() {
        assert!(!sweep.azimuths_deg.is_empty(), "sweep {i} has no azimuths");
        for m in &sweep.moments {
            assert_eq!(
                m.data.len(),
                m.ray_count * m.gate_count,
                "sweep {i} {:?}",
                m.kind
            );
        }
    }
}

#[test]
fn decode_archive_volume_file() {
    let bytes = std::fs::read(fixture_path()).expect("read fixture");
    let volume = decode_volume_file(&bytes).expect("decode_volume_file");
    print_volume_stats("archive", &volume);
    assert_eq!(volume.meta.sequence_number, Some(984));
    assert_sane_volume(&volume);
}

#[test]
fn decode_realtime_volume_from_assembled_stream() {
    let bytes = std::fs::read(fixture_path()).expect("read fixture");
    let volume = decode_realtime_volume(&bytes).expect("decode_realtime_volume (with header)");
    print_volume_stats("realtime", &volume);
    assert_sane_volume(&volume);

    // The same stream without the 24-byte Archive II header (pure LDM chunk
    // concatenation, as produced by some assembly pipelines).
    let raw = {
        use std::io::Read;
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(bytes.as_slice())
            .read_to_end(&mut raw)
            .expect("gunzip fixture");
        raw
    };
    assert!(
        raw.starts_with(b"AR2V"),
        "fixture stream lacks archive header"
    );
    let headerless =
        decode_realtime_volume(&raw[24..]).expect("decode_realtime_volume (headerless)");
    print_volume_stats("realtime-headerless", &headerless);
    assert_sane_volume(&headerless);

    // Both paths agree on structure.
    assert_eq!(headerless.meta.site_id, volume.meta.site_id);
    assert_eq!(headerless.meta.sweep_count, volume.meta.sweep_count);
    assert_eq!(headerless.meta.vcp, volume.meta.vcp);
}
