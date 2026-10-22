//! Allocator-retention probe: mimics stormscope's decode churn — a ring of 24
//! live ~78 MB "volumes" (every page written, like decoded gate data), one
//! new volume per iteration with the oldest dropped — and reports RSS over
//! time. The steady state should hold ~1.9 GB; growth past that is allocator
//! retention, and `tuned` applies the `main.rs` mallopt settings to compare.
//!
//! ```sh
//! cargo run -p stormscope-app --release --example alloc_probe -- plain 200
//! cargo run -p stormscope-app --release --example alloc_probe -- tuned 200
//! ```

use std::collections::VecDeque;

fn rss_mb() -> u64 {
    #[cfg(target_os = "linux")]
    {
        return std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
            .map(|pages| pages * 4096 / 1_000_000)
            .unwrap_or(0);
    }
    // The probe's RSS readout is Linux-only; other platforms report 0.
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

/// One "decoded volume": 78 MB, every page written (gate data), plus a bunch
/// of small per-message allocations like the decoder makes.
fn decode_volume(i: usize) -> Vec<u8> {
    let mut big = vec![0u8; 78_000_000];
    for page in big.chunks_exact_mut(4096) {
        page[0] = (i & 0xff) as u8;
    }
    // Decoder-ish small-object churn: thousands of short-lived allocations.
    let mut acc = 0usize;
    for j in 0..2000 {
        let v: Vec<u8> = vec![(i ^ j) as u8; 4096];
        acc += v.len();
    }
    std::hint::black_box(acc);
    big
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "plain".into());
    let iters: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);
    if mode == "tuned" {
        // glibc-only: other platforms (and Windows) run the plain probe.
        #[cfg(target_env = "gnu")]
        unsafe {
            libc::mallopt(libc::M_MMAP_THRESHOLD, 1 << 20);
            libc::mallopt(libc::M_TRIM_THRESHOLD, 1 << 20);
            libc::mallopt(libc::M_ARENA_MAX, 4);
        }
        #[cfg(not(target_env = "gnu"))]
        eprintln!("`tuned` is a glibc-only mode; this platform runs the plain probe");
    }
    const RING: usize = 24;
    let mut ring: VecDeque<Vec<u8>> = VecDeque::new();
    println!(
        "mode={mode} iters={iters} ring={RING} (steady state should be ~{:.1} GB RSS)",
        RING as f64 * 0.078
    );
    for i in 0..iters {
        ring.push_back(decode_volume(i));
        if ring.len() > RING {
            ring.pop_front();
        }
        if (i + 1) % 20 == 0 {
            println!("  after {:3} volumes: rss={} MB", i + 1, rss_mb());
        }
    }
    // Drop everything: how much comes back?
    ring.clear();
    #[cfg(target_env = "gnu")]
    unsafe {
        libc::malloc_trim(0);
    }
    println!("after dropping the ring + trim: rss={} MB", rss_mb());
}
