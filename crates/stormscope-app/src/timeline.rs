//! Bottom timeline scrubber: play/pause, per-frame ticks with scan times,
//! click-to-seek, and an fps control.

use crate::app::{DataCmd, StormApp};
use crate::state::{AppState, LoopState, Mode};

pub fn timeline(ui: &mut egui::Ui, app: &mut StormApp) {
    // Expose every downloaded scan for the selected site, not just the 24 in
    // the ring. This is a directory scan, so refresh it lazily.
    if app.state.cached_scans_site != app.state.site_id
        || app
            .state
            .cached_scans_at
            .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(20))
    {
        app.refresh_cached_scans();
    }

    ui.horizontal(|ui| {
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
        if let Some(f) = app.state.current() {
            ui.label(crate::timefmt::fmt_central(&f.volume_start));
        } else {
            ui.label("no frames");
        }
    });

    // Scrub strip: one tick per retained frame.
    let n = app.state.ring.len();
    if n == 0 {
        disk_history(ui, app);
        return;
    }
    let height = 18.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
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

    if response.clicked() {
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

    disk_history(ui, app);
}

/// A time-proportional strip of every cached scan for the selected site.
/// Clicking near a tick loads that day around that scan (cache-first, so
/// nothing already on disk is downloaded again).
fn disk_history(ui: &mut egui::Ui, app: &mut StormApp) {
    let scans = app.state.cached_scans.clone();
    if scans.is_empty() {
        ui.label(
            egui::RichText::new(format!(
                "On disk: no cached {} scans yet",
                app.state.site_id
            ))
            .small()
            .weak(),
        );
        return;
    }
    let total: u64 = scans.iter().map(|s| s.bytes).sum();
    let first = scans.first().map(|s| s.start).unwrap_or_default();
    let last = scans.last().map(|s| s.start).unwrap_or_default();
    let central = crate::timefmt::central();
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!(
                "On disk {}: {} scans · {:.1} GB · {} – {} · click a tick to load",
                app.state.site_id,
                scans.len(),
                total as f64 / 1e9,
                first.with_timezone(central).format("%b %d"),
                last.with_timezone(central).format("%b %d"),
            ))
            .small()
            .weak(),
        );
        if ui
            .small_button("Downloaded…")
            .on_hover_text("Browse every cached day for this station")
            .clicked()
        {
            app.state.downloads_open = true;
        }
    });

    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), egui::Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, egui::Color32::from_rgb(24, 26, 32));
    let span = (last - first).num_seconds().max(1) as f64;
    for scan in &scans {
        let t = (scan.start - first).num_seconds() as f64 / span;
        let x = rect.left() + t as f32 * rect.width();
        let in_ring = app
            .state
            .ring
            .iter()
            .any(|f| (f.volume_start - scan.start).num_seconds().abs() <= 90);
        let (color, h) = if in_ring {
            (egui::Color32::from_rgb(90, 180, 255), 14.0)
        } else {
            (egui::Color32::from_rgb(72, 78, 92), 9.0)
        };
        painter.rect_filled(
            egui::Rect::from_center_size(egui::pos2(x, rect.center().y), egui::vec2(1.5, h)),
            0.0,
            color,
        );
    }

    let pointer = response.interact_pointer_pos();
    let hovered = response.hover_pos().and_then(|pos| {
        let frac = ((pos.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0) as f64;
        let target = first + chrono::Duration::seconds((frac * span) as i64);
        scans
            .iter()
            .min_by_key(|s| (s.start - target).num_seconds().abs())
            .copied()
    });
    let clicked = response.clicked();
    let response = if let Some(scan) = hovered {
        response.on_hover_text(format!(
            "{} · {:.1} MB — click to load this day/time",
            stamp_full(&scan.start),
            scan.bytes as f64 / 1e6
        ))
    } else {
        response
    };

    if clicked {
        if let Some(pos) = pointer {
            let frac = ((pos.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0) as f64;
            let target = first + chrono::Duration::seconds((frac * span) as i64);
            if let Some(scan) = scans
                .iter()
                .min_by_key(|s| (s.start - target).num_seconds().abs())
            {
                use chrono::Timelike;
                let local = scan.start.with_timezone(central);
                let date = local.date_naive();
                let minute = local.hour() * 60 + local.minute();
                app.state.mode = Mode::Archive;
                app.state.loop_state = LoopState::Paused;
                app.state.archive_date = date;
                app.state.archive_minute = minute;
                app.state.archive_jump = Some((scan.start, std::time::Instant::now()));
                let _ = app.tx.send(DataCmd::LoadCachedDay {
                    site: app.state.site_id.clone(),
                    date,
                    target: scan.start,
                });
            }
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

    egui::Window::new(format!("Downloaded radar — {site}"))
        .id(egui::Id::new("stormscope.downloads"))
        .open(&mut open)
        .default_size(egui::vec2(560.0, 440.0))
        .resizable(true)
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
