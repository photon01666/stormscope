//! Bottom timeline scrubber: play/pause, per-frame ticks with scan times,
//! click-to-seek, and an fps control.

use crate::app::{DataCmd, StormApp};
use crate::state::{AppState, LoopState, Mode};

pub fn timeline(ui: &mut egui::Ui, app: &mut StormApp) {
    ui.horizontal_wrapped(|ui| {
        // Play/pause.
        let playing = app.state.loop_state == LoopState::Playing;
        let label = if playing { "Pause" } else { "Play" };
        if ui.button(label).clicked() {
            if playing {
                app.state.loop_state = LoopState::Paused;
                // Stopping means "show me now": jump to the newest scan.
                // Looping history is what Play is for; leaving the playhead
                // parked in the past after a stop read as the app being
                // stuck on old data.
                app.state.playhead = app.state.ring.len().saturating_sub(1);
            } else {
                app.state.loop_state = LoopState::Playing;
            }
        }
        if ui.button("⏮").clicked() {
            app.state.playhead = 0;
        }
        if ui.button("⏭").clicked() {
            app.state.playhead = app.state.ring.len().saturating_sub(1);
        }

        ui.separator();
        if !cfg!(target_os = "android") {
            ui.label("Loop speed");
            ui.add(
                egui::Slider::new(&mut app.state.fps, 0.25..=30.0)
                    .logarithmic(true)
                    .fixed_decimals(2),
            );
            ui.label(
                egui::RichText::new(crate::timeline::frame_time_label(app.state.fps))
                    .weak()
                    .small(),
            );
            ui.separator();
        }
        if let Some(f) = app.state.current() {
            ui.label(crate::timefmt::fmt_central(&f.volume_start));
        } else {
            ui.label("no frames");
        }
        if let Some(f) = app.state.current() {
            ui.label(crate::timefmt::fmt_central(&f.volume_start));
        } else {
            ui.label("no frames");
        }
        ui.separator();
        if ui
            .button("Downloaded…")
            .on_hover_text("Browse every cached day for this station")
            .clicked()
        {
            app.state.downloads_open = true;
        }
    });

    // Scrub strip: one tick per retained frame.
    let n = app.state.ring.len();
    if n == 0 {
        return;
    }
    // A taller strip is easier to drag with a finger than with a mouse.
    let height = if cfg!(target_os = "android") {
        26.0
    } else {
        18.0
    };
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, egui::Color32::from_rgb(30, 32, 40));

    let slot = rect.width() / n as f32;
    for (i, _frame) in app.state.ring.iter().enumerate() {
        let x0 = rect.left() + i as f32 * slot;
        let cx = x0 + slot / 2.0;
        let selected = i == app.state.playhead;
        let color = if selected {
            egui::Color32::from_rgb(90, 180, 255)
        } else {
            egui::Color32::from_rgb(70, 74, 86)
        };
        let bar = egui::Rect::from_center_size(
            egui::pos2(cx, rect.center().y),
            egui::vec2(
                (slot * 0.5).max(2.0),
                if selected {
                    height - 4.0
                } else {
                    height - 10.0
                },
            ),
        );
        painter.rect_filled(bar, 1.0, color);
    }

    // Tap or drag a finger/pointer along the strip to scrub the ring.
    if response.clicked() || response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            let idx = ((pos.x - rect.left()) / slot).floor();
            app.state.playhead = (idx as usize).min(n - 1);
        }
    }
    if let Some(pos) = response.hover_pos() {
        let idx =
            (((pos.x - rect.left()) / slot).floor() as isize).clamp(0, n as isize - 1) as usize;
        if let Some(frame) = app.state.ring.get(idx) {
            response.on_hover_text(format!(
                "{} · frame {}/{}",
                stamp_full(&frame.volume_start),
                idx + 1,
                n
            ));
        }
    }
}
/// The full downloaded-history browser: every cached Central day for the
/// selected site, with counts/sizes and Start/Mid/End loads. Archive still
/// uses S3; this window is the on-disk history.
pub fn downloads_window(ctx: &egui::Context, app: &mut StormApp) {
    if !app.state.downloads_open {
        return;
    }
    if app.state.cached_days_site != app.state.site_id
        || app
            .state
            .cached_days_at
            .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(30))
    {
        app.refresh_cached_days();
        app.refresh_cached_scans();
    }

    let mut open = app.state.downloads_open;
    let site = app.state.site_id.clone();
    let days = app.state.cached_days.clone();
    let total_scans: usize = days.iter().map(|d| d.scans).sum();
    let total_bytes: u64 = days.iter().map(|d| d.bytes).sum();
    let central = crate::timefmt::central();
    let ring_first = app.state.ring.front().map(|f| f.volume_start);
    let ring_last = app.state.ring.back().map(|f| f.volume_start);
    let scans = app.state.cached_scans.clone();
    let mut load: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut delete: Option<chrono::NaiveDate> = None;

    // Fixed size: this is a history list, and egui's own vertical resize
    // subtracts the title bar on the first drag frame. No handle, no jump.
    let screen = ctx.content_rect().size();
    let size = egui::vec2(
        (screen.x * 0.42).clamp(440.0, 620.0),
        (screen.y * 0.42).clamp(300.0, 500.0),
    );
    egui::Window::new(format!("Downloaded radar — {site}"))
        .id(egui::Id::new("stormscope.downloads"))
        .open(&mut open)
        .fixed_size(size)
        .default_pos(egui::pos2(24.0, 110.0))
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{} scans · {:.1} GB in {} cached days — loaded from disk, no S3",
                    total_scans,
                    total_bytes as f64 / 1e9,
                    days.len()
                ))
                .weak(),
            );
            ui.label(
                egui::RichText::new(
                    "Each load is a 24-scan window (the ring's memory budget). Use \
                     earlier/later to walk the rest.",
                )
                .small()
                .weak(),
            );
            ui.separator();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .max_height((ui.available_height() - 40.0).max(120.0))
                .show(ui, |ui| {
                    for day in &days {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(day.date.format("%a %b %-d").to_string())
                                    .strong(),
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} scans · {:.1} MB · {}–{}",
                                    day.scans,
                                    day.bytes as f64 / 1e6,
                                    day.first.with_timezone(central).format("%H:%M"),
                                    day.last.with_timezone(central).format("%H:%M"),
                                ))
                                .small()
                                .weak(),
                            );
                            if ui.small_button("Start").clicked() {
                                load = Some(day.first);
                            }
                            if ui.small_button("Mid").clicked() {
                                load = Some(day.first + (day.last - day.first) / 2);
                            }
                            if ui.small_button("End").clicked() {
                                load = Some(day.last);
                            }
                            if app.state.downloads_confirm == Some(day.date) {
                                ui.label(
                                    egui::RichText::new(format!("Delete {} scans?", day.scans))
                                        .small()
                                        .strong(),
                                );
                                if ui.small_button("Yes").clicked() {
                                    delete = Some(day.date);
                                    app.state.downloads_confirm = None;
                                }
                                if ui.small_button("No").clicked() {
                                    app.state.downloads_confirm = None;
                                }
                            } else if ui
                                .small_button("Delete")
                                .on_hover_text("Remove every cached scan for this day from disk")
                                .clicked()
                            {
                                app.state.downloads_confirm = Some(day.date);
                            }
                        });
                    }
                    if days.is_empty() {
                        ui.label(
                            egui::RichText::new("No cached scans for this station yet.").weak(),
                        );
                    }
                });
            ui.separator();
            ui.horizontal(|ui| {
                let before = ring_first.and_then(|first| {
                    scans
                        .iter()
                        .rev()
                        .find(|s| s.start < first)
                        .map(|s| s.start)
                });
                let after = ring_last
                    .and_then(|last| scans.iter().find(|s| s.start > last).map(|s| s.start));
                if ui
                    .add_enabled(before.is_some(), egui::Button::new("◀ earlier 24"))
                    .clicked()
                {
                    load = before;
                }
                if ui
                    .add_enabled(after.is_some(), egui::Button::new("later 24 ▶"))
                    .clicked()
                {
                    load = after;
                }
                ui.label(
                    egui::RichText::new("window of 24 cached scans")
                        .small()
                        .weak(),
                );
            });
        });
    app.state.downloads_open = open;

    if let Some(date) = delete {
        let _ = app.tx.send(DataCmd::DeleteCachedDay {
            site: app.state.site_id.clone(),
            date,
        });
    }
    if let Some(target) = load {
        use chrono::Timelike;
        let local = target.with_timezone(central);
        let date = local.date_naive();
        app.state.mode = Mode::Archive;
        app.state.loop_state = LoopState::Paused;
        app.state.archive_date = date;
        app.state.archive_minute = local.hour() * 60 + local.minute();
        app.state.archive_jump = Some((target, std::time::Instant::now()));
        let _ = app.tx.send(DataCmd::LoadCachedDay {
            site: app.state.site_id.clone(),
            date,
            target,
        });
    }
}

/// Exact Central date and time for a tooltip.
fn stamp_full(dt: &chrono::DateTime<chrono::Utc>) -> String {
    dt.with_timezone(crate::timefmt::central())
        .format("%a %b %-d, %Y · %-I:%M:%S %p %Z")
        .to_string()
}

/// Compute a human frame-time label for a given fps.
pub fn frame_time_label(fps: f32) -> String {
    format!("{:.0} fps · {:.2}s/frame", fps, 1.0 / fps.max(1.0))
}

#[allow(dead_code)]
fn _unused(_s: &AppState) {}
