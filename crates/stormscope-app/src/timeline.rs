//! Bottom timeline scrubber: play/pause, per-frame ticks with scan times,
//! click-to-seek, and an fps control.

use crate::app::{DataCmd, StormApp};
use crate::state::{AppState, LoopState, Mode};

pub fn timeline(ui: &mut egui::Ui, app: &mut StormApp) {
    // FutureCast is a mode, not a second scrubber under the radar ring: the
    // live sweep is hidden while it is on, so the bottom panel becomes the
    // forecast loop's own timeline.
    if app.state.show_futurecast {
        futurecast_timeline(ui, app);
        return;
    }

    ui.horizontal_wrapped(|ui| {
        // Play/pause. The phone gets a larger touch target; the skip-to-end
        // buttons are gone on every platform because the scrub strip already
        // covers that job.
        let playing = app.state.loop_state == LoopState::Playing;
        let label = if playing { "Pause" } else { "Play" };
        let play = if cfg!(target_os = "android") {
            ui.add(
                egui::Button::new(egui::RichText::new(label).size(16.0).strong())
                    .min_size(egui::vec2(76.0, 40.0)),
            )
        } else {
            ui.button(label)
        };
        if play.clicked() {
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
        let last2 = if cfg!(target_os = "android") {
            ui.add(egui::Button::new("Last 2hr").min_size(egui::vec2(84.0, 40.0)))
        } else {
            ui.button("Last 2hr")
        };
        if last2
            .on_hover_text(
                "Download and loop the last 2 hours of radar scans instead of only the cached loop",
            )
            .clicked()
        {
            let _ = app
                .tx
                .send(DataCmd::LoadLastTwoHours(app.state.site_id.clone()));
        }
        #[cfg(target_os = "android")]
        {
            let on = app.state.dbz_crosshair;
            let label = if on { "dBZ ON" } else { "dBZ" };
            let button = ui.add(egui::Button::new(label).min_size(egui::vec2(68.0, 32.0)));
            if button
                .on_hover_text("Drag the map to inspect reflectivity with the dBZ crosshair")
                .clicked()
            {
                app.state.dbz_crosshair = !on;
                if app.state.dbz_crosshair && app.state.dbz_crosshair_pos.is_none() {
                    app.state.dbz_crosshair_pos =
                        Some((app.state.view.center_lat, app.state.view.center_lon));
                }
            }
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
    });

    // Scrub strip: one tick per retained frame.
    let n = app.state.ring.len();
    if n == 0 {
        return;
    }
    // A taller strip is easier to drag with a finger than with a mouse.
    // 32 pt is about a fingertip; below that the touch target gets fiddly.
    let height = if cfg!(target_os = "android") {
        32.0
    } else {
        18.0
    };
    // Use the panel's full width, not the space left over after the wrapped
    // control row: on a phone that leftover can be a sliver, which made the
    // strip a few dozen pixels wide and impossible to scrub.
    let strip_width = ui.max_rect().width();
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(strip_width, height),
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
/// FutureCast's bottom panel: model run + valid time, play/pause, and an
/// hourly strip. Frames arrive progressively; missing slots stay dark but
/// clickable (the strip says "loading" for them).
fn futurecast_timeline(ui: &mut egui::Ui, app: &mut StormApp) {
    let cyan = egui::Color32::from_rgb(120, 215, 255);
    ui.spacing_mut().item_spacing.y = 2.0;

    // Unsupported sites (Alaska/Hawaii) never get a run.
    let supported = radar_core::sites::site(&app.state.site_id)
        .is_some_and(|site| radar_data::futurecast::supported(site.latitude, site.longitude));

    let Some(futurecast) = app.state.futurecast.as_ref() else {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("FutureCast").strong().color(cyan))
                .on_hover_text(
                    "NOAA HRRR simulated composite reflectivity — a model forecast, not observed radar",
                );
            if supported {
                ui.add(egui::Spinner::new().size(14.0));
                ui.label(
                    egui::RichText::new(if app.state.futurecast_pending {
                        "fetching the HRRR forecast…"
                    } else {
                        "waiting for the HRRR run…"
                    })
                    .weak(),
                );
            } else {
                ui.label(egui::RichText::new("HRRR FutureCast covers the CONUS only").weak());
            }
        });
        return;
    };

    // Copy the tiny plan/summary out of the state before drawing closures:
    // the header and strip both mutate the selected slot.
    let init = futurecast.init;
    let plan = futurecast.plan.clone();
    let loaded_mask: Vec<bool> = futurecast.frames.iter().map(|f| f.is_some()).collect();
    let plan_len = plan.len();
    let loaded = loaded_mask.iter().filter(|ready| **ready).count();
    let selected_info = futurecast.selected_info().cloned();
    let selected_frame = futurecast.selected_frame().cloned();
    let peak_dbz = selected_frame.as_ref().and_then(|frame| frame.max_dbz());
    let selected = futurecast.selected;
    let extended = futurecast.extended;
    let playing = app.state.futurecast_playing;

    // Row 1: mode, transport, run and the selected valid time.
    ui.horizontal_wrapped(|ui| {
        #[cfg(not(target_os = "android"))]
        ui.label(egui::RichText::new("FutureCast").strong().color(cyan))
            .on_hover_text(if extended {
                "NOAA HRRR 48-hour extended run — simulated composite reflectivity, a model forecast, not observed radar"
            } else {
                "NOAA HRRR 18-hour run — simulated composite reflectivity, a model forecast, not observed radar"
            });
        let play_label = if playing { "Pause" } else { "Play" };
        let play = if cfg!(target_os = "android") {
            ui.add(
                egui::Button::new(egui::RichText::new(play_label).size(16.0).strong())
                    .min_size(egui::vec2(76.0, 40.0)),
            )
        } else {
            ui.button(play_label)
        };
        if play.clicked() {
            app.state.futurecast_playing = !playing;
            app.state.futurecast_last_step = ui.input(|i| i.time);
            if app.state.futurecast_playing {
                app.state.loop_state = LoopState::Paused;
            }
        }
        let run_label = if cfg!(target_os = "android") {
            format!(
                "{}Z f{:02}",
                init.format("%H"),
                selected_info.as_ref().map_or(0, |i| i.hour)
            )
        } else {
            format!(
                "HRRR {}Z run · f{:02}{}",
                init.format("%H"),
                selected_info.as_ref().map_or(0, |i| i.hour),
                if extended { " · 48h" } else { "" }
            )
        };
        ui.label(egui::RichText::new(run_label).strong());
        if let Some(peak) = peak_dbz {
            let peak_label = if cfg!(target_os = "android") {
                format!("{peak:.0} dBZ")
            } else {
                format!("peak {peak:.0} dBZ")
            };
            ui.label(egui::RichText::new(peak_label).small().weak())
                .on_hover_text("strongest simulated echo in this forecast frame");
        }
        if loaded < plan_len {
            ui.add(egui::Spinner::new().size(12.0));
            ui.label(
                egui::RichText::new(format!("fetching {loaded}/{plan_len}"))
                    .small()
                    .weak(),
            );
        } else {
            // The 18/48 mode switch and the type/reflectivity colour toggle
            // only appear once the current run is fully in hand; during
            // loading the spinner owns this row.
            if futurecast_horizon_button(ui, extended).clicked() {
                app.toggle_futurecast_horizon();
            }
            let type_label = if app.state.futurecast_precip_type {
                if cfg!(target_os = "android") { "TYPE" } else { "type" }
            } else if cfg!(target_os = "android") {
                "REFL"
            } else {
                "refl"
            };
            let type_button = if cfg!(target_os = "android") {
                ui.add(egui::Button::new(type_label).min_size(egui::vec2(52.0, 38.0)))
            } else {
                ui.button(type_label)
            };
            if type_button
                .on_hover_text(if app.state.futurecast_precip_type {
                    "Colouring by HRRR precipitation type: snow blue, ice purple, freezing rain pink"
                } else {
                    "Standard reflectivity colours (switch back to precipitation type)"
                })
                .clicked()
            {
                app.state.futurecast_precip_type = !app.state.futurecast_precip_type;
            }
        }
    });
    // Hourly strip, painted exactly like the radar scrubber. The selected
    // frame's exact time/day is drawn large on the map (map.rs), not here —
    // the bar stays a compact transport control, but the phone strip is tall
    // enough to grab and drag with a finger.
    let n = plan_len.max(1);
    let height = if cfg!(target_os = "android") {
        56.0
    } else {
        18.0
    };
    let strip_width = ui.max_rect().width();
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(strip_width, height),
        egui::Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, egui::Color32::from_rgb(30, 32, 40));
    let slot = rect.width() / n as f32;
    for i in 0..plan_len {
        let x0 = rect.left() + i as f32 * slot;
        let cx = x0 + slot / 2.0;
        let is_loaded = loaded_mask[i];
        let is_selected = i == selected;
        let color = if is_selected {
            cyan
        } else if is_loaded {
            egui::Color32::from_rgb(90, 180, 255)
        } else {
            egui::Color32::from_rgb(55, 58, 68)
        };
        let bar = egui::Rect::from_center_size(
            egui::pos2(cx, rect.center().y),
            egui::vec2(
                (slot * 0.55).max(2.0),
                if is_selected {
                    height - 4.0
                } else if is_loaded {
                    height - 9.0
                } else {
                    height - 14.0
                },
            ),
        );
        painter.rect_filled(bar, 1.0, color);
    }
    if response.clicked() || response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            let idx = (((pos.x - rect.left()) / slot).floor() as isize)
                .clamp(0, plan_len.saturating_sub(1) as isize) as usize;
            if let Some(fc) = app.state.futurecast.as_mut() {
                fc.selected = idx;
            }
            app.state.futurecast_last_step = ui.input(|i| i.time);
        }
    }
    if let Some(pos) = response.hover_pos() {
        let idx = (((pos.x - rect.left()) / slot).floor() as isize)
            .clamp(0, plan_len.saturating_sub(1) as isize) as usize;
        if let Some(info) = plan.get(idx) {
            let local = info.valid.with_timezone(crate::timefmt::central());
            let state = if loaded_mask[idx] { "ready" } else { "loading" };
            response.on_hover_text(format!(
                "f{:02} · valid {} · {state}",
                info.hour,
                local.format("%a %-I:%M %p %Z")
            ));
        }
    }

    // Playback advancement lives in `StormApp::advance_futurecast_playback`
    // so it keeps running when fullscreen hides this panel.
}

/// The 18/48-hour switch is a primary mode control on the phone, so it gets
/// a larger target than the transport buttons.
fn futurecast_horizon_button(ui: &mut egui::Ui, extended: bool) -> egui::Response {
    let label = if cfg!(target_os = "android") {
        if extended { "18 HR" } else { "48 HR" }
    } else {
        if extended { "18 hr" } else { "48 hr" }
    };
    let response = if cfg!(target_os = "android") {
        ui.add(
            egui::Button::new(egui::RichText::new(label).size(15.0).strong())
                .min_size(egui::vec2(82.0, 38.0)),
        )
    } else {
        // Desktop/Windows: same stock size as the Play/Pause button.
        ui.button(label)
    };
    response.on_hover_text(if extended {
        "Switch back to the latest hourly 18-hour HRRR run"
    } else {
        "Load the 48-hour extended HRRR run (00/06/12/18Z; older but longer)"
    })
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
