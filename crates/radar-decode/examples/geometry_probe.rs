//! Scratch probe: compare the sweep geometry fields of two consecutive
//! decoded volumes, to see whether the GateGeometry cache key can rely on
//! exact bits or needs quantization.
//!
//! ```sh
//! cargo run -p radar-decode --example geometry_probe -- <a.gz> <b.gz>
//! ```

fn main() -> anyhow::Result<()> {
    let a = std::fs::read(std::env::args().nth(1).expect("first volume path"))?;
    let b = std::fs::read(std::env::args().nth(2).expect("second volume path"))?;
    let mut vols = Vec::new();
    for (name, bytes) in [("a", a), ("b", b)] {
        let vol = radar_decode::decode_volume_file(&bytes)?;
        println!("== {name}: {} sweeps, start {}", vol.sweeps.len(), vol.meta.volume_start);
        for (i, s) in vol.sweeps.iter().take(3).enumerate() {
            let md = s.moments.first();
            println!(
                "  sweep {i}: elev={:?} measured={:?} az_width={:?} rays={} gates={} az[0]={:?} az[1]={:?}",
                s.elevation_deg,
                s.elevation_measured_deg,
                s.azimuth_width_deg,
                md.map(|m| m.ray_count).unwrap_or(0),
                md.map(|m| m.gate_count).unwrap_or(0),
                s.azimuths_deg.first(),
                s.azimuths_deg.get(1),
            );
        }
        vols.push(vol);
    }

    // Rotation hypothesis: is volume b's azimuth list an exact rotation of
    // volume a's (same set, phase-shifted)? If so, a cached geometry table
    // can be *row-remapped* instead of rebuilt.
    let ang_diff = |x: f32, y: f32| ((x - y + 180.0).rem_euclid(360.0) - 180.0).abs();
    for (si, (sa, sb)) in vols[0]
        .sweeps
        .iter()
        .zip(vols[1].sweeps.iter())
        .enumerate()
    {
        let (aa, ab) = (&sa.azimuths_deg, &sb.azimuths_deg);
        if aa.len() != ab.len() || aa.is_empty() {
            println!("sweep {si}: ray counts differ ({} vs {})", aa.len(), ab.len());
            continue;
        }
        let n = aa.len();
        // Best shift k: residual of matching ab[(k+i)%n] against aa[i].
        let mut best = (usize::MAX, f32::MAX);
        for k in 0..n {
            let mut worst = 0.0f32;
            for i in 0..n {
                worst = worst.max(ang_diff(ab[(k + i) % n], aa[i]));
                if worst > best.1 {
                    break;
                }
            }
            if worst < best.1 {
                best = (k, worst);
            }
        }
        println!(
            "sweep {si}: n={n}, best shift k={}, max residual {:.6} deg{}",
            best.0,
            best.1,
            if best.1 < 0.01 { "  <= rotation!" } else { "" }
        );
    }
    Ok(())
}
