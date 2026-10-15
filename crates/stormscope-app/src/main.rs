//! stormscope binary entry point.

use stormscope_app::StormApp;

/// glibc tuning for a long-running app that churns ~80 MB buffers (decoded
/// radar volumes) every few minutes.
///
/// Without this, glibc keeps freed decode buffers in its heap/arenas and the
/// process RSS creeps from ~2 GB to ~5 GB over a night. That is not just the
/// app's problem: on this box the GPU is a shared-memory Intel Arc, so system
/// RAM pressure makes the driver fail to evict GPU buffers (`[TTM] Buffer
/// eviction failed`) and *the whole desktop* stutters until the app is
/// restarted. Best-effort; a no-op on non-glibc targets.
fn tune_allocator() {
    #[cfg(target_env = "gnu")]
    unsafe {
        // Large allocations (the volume buffers) go straight to mmap, so
        // free() hands the pages back to the OS immediately. Also keeps the
        // threshold fixed instead of glibc's dynamic growth.
        libc::mallopt(libc::M_MMAP_THRESHOLD, 1 << 20);
        // Trim the heap aggressively when the top of it is free (>1 MB).
        libc::mallopt(libc::M_TRIM_THRESHOLD, 1 << 20);
        // A handful of arenas is plenty for our threads; fewer arenas = less
        // fragmentation.
        libc::mallopt(libc::M_ARENA_MAX, 4);
    }
}

fn main() -> eframe::Result {
    tune_allocator();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 500.0])
            .with_title("stormscope"),
        ..Default::default()
    };

    eframe::run_native(
        "stormscope",
        options,
        Box::new(|cc| Ok(Box::new(StormApp::new(cc)))),
    )
}
