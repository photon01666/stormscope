//! stormscope binary entry point.

use stormscope_app::StormApp;

/// glibc tuning for a long-running app that churns ~80 MB buffers (decoded
/// radar volumes) every few minutes.
///
/// Without this, glibc keeps freed decode buffers in its heap/arenas and the
/// process RSS creeps from ~2 GB to ~5 GB over a long session. That is not
/// just the app's problem: on GPUs that share system memory (integrated
/// graphics, small-VRAM cards) RAM pressure makes the driver fail to evict
/// GPU buffers (`[TTM] Buffer eviction failed`) and the whole desktop
/// stutters until the app is restarted. Best-effort; a no-op on non-glibc
/// targets.
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

/** Log a panic to a file before the process dies.
 *
 * A GUI app started from a desktop launcher has nowhere to print a panic, and
 * a window that simply vanishes tells the user (and whoever they report to)
 * nothing. Writing the message and backtrace to `~/.cache/stormscope/` leaves
 * evidence; `coredumpctl info stormscope` covers the harder crashes.
 */
fn install_panic_log() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let path = radar_data::DiskCache::default_root().join("crash.log");
        let mut text = format!(
            "stormscope {} panicked\n{info}\n",
            env!("CARGO_PKG_VERSION")
        );
        let bt = std::backtrace::Backtrace::force_capture();
        text.push_str(&format!("backtrace:\n{bt}\n"));
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, text);
        default_hook(info);
    }));
}

fn main() -> eframe::Result {
    tune_allocator();
    install_panic_log();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    eframe::run_native(
        "stormscope",
        native_options(),
        Box::new(|cc| Ok(Box::new(StormApp::new(cc)))),
    )
}

/// Window and renderer options.
///
/// Presentation is pinned to vsync (`Fifo`) instead of eframe's default
/// `AutoVsync`, because the auto modes renegotiate whenever the output is
/// reconfigured: a display that has been asleep can come back from the
/// monitor sleep/wake cycle with a different capability list, and an uncapped
/// fullscreen map then presents at hundreds of frames a second. The GPU is
/// shared with the compositor (whose DisplayPort audio sits on the same
/// device), so the whole session feels like it dropped to half the refresh
/// rate — dragging, zooming, everything — and the audio hiccups. That is
/// exactly what a night with the monitors off ended in until the app was
/// restarted, so the mode is no longer left to negotiation.
fn native_options() -> eframe::NativeOptions {
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 500.0])
            .with_title("stormscope"),
        ..Default::default()
    };
    options.wgpu_options.surface.present_mode = egui_wgpu::wgpu::PresentMode::Fifo;
    options
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Presentation must stay vsync-bound: an uncapped present after a
    /// display reconfigure is what dragged the whole session down to "half
    /// the refresh rate" (see `native_options`).
    #[test]
    fn presentation_is_pinned_to_vsync() {
        assert_eq!(
            native_options().wgpu_options.surface.present_mode,
            egui_wgpu::wgpu::PresentMode::Fifo
        );
    }
}
