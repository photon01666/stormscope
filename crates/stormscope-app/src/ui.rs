//! Top-level layout: auto-hiding controls sidebar, central map, bottom timeline.

use crate::app::StormApp;
use crate::state::{Mode, PaneMax};
use radar_core::Moment;

mod ai;
pub(crate) mod alerts;
mod forecast;
mod map;
mod reports;
mod totals;
mod volume3d;

/// Fixed sidebar width (screen px) used by both the slide offset and the
/// hover hysteresis below — keep the two in sync.
const SIDEBAR_WIDTH: f32 = if cfg!(target_os = "android") {
    160.0
} else {
    250.0
};
/// Pointer distance from the left edge that peeks the sidebar open.
const SIDEBAR_EDGE_PX: f32 = 16.0;
/// Extra x-range past the sidebar that still counts as "inside" so popups,
/// sliders and combo boxes don't slam it shut mid-interaction.
const SIDEBAR_CLOSE_SLOP: f32 = 32.0;
/// Slide animation time (seconds).
const SIDEBAR_ANIM_SECS: f32 = 0.18;

/// Toggle the chrome-free fullscreen mode (map only; the status bar, timeline,
/// chips, badges and windows all hide). On Android the system status bar is
/// hidden too, so the radar truly fills the screen.
pub(crate) fn set_fullscreen(ctx: &egui::Context, app: &mut StormApp) {
    app.state.fullscreen = !app.state.fullscreen;
    if app.state.fullscreen {
        // The windows disappear with the chrome: drop text focus so Gboard
        // does not stay up over an invisible field.
        if let Some(id) = ctx.memory(|m| m.focused()) {
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
    }
    #[cfg(target_os = "android")]
    crate::android_ime::set_system_bars_hidden(app.state.fullscreen);
}

/// Long-press the map (or the 3D pane) to enter/leave fullscreen. A touch
/// held still (egui's `long_touched`) or a mouse held still ~0.6 s both
/// count; the toggle is edge-triggered — one per press, re-armed on release.
pub(crate) fn long_press_toggle_fullscreen(
    ctx: &egui::Context,
    app: &mut StormApp,
    resp: &egui::Response,
) {
    let down = ctx.input(|i| i.pointer.any_down());
    if !down {
        app.state.fullscreen_press_handled = false;
        return;
    }
    if app.state.fullscreen_press_handled {
        return;
    }
    let held_long = resp.long_touched()
        || ctx.input(|i| {
            let p = &i.pointer;
            resp.is_pointer_button_down_on()
                && !p.is_decidedly_dragging()
                && p.press_start_time().is_some_and(|t| i.time - t > 0.6)
        });
    if held_long {
        app.state.fullscreen_press_handled = true;
        set_fullscreen(ctx, app);
    }
}

pub fn layout(ui: &mut egui::Ui, app: &mut StormApp) {
    #[cfg(target_os = "android")]
    {
        // Replay Gboard's commits into egui's event queue before any text
        // field of this frame reads it: egui rebuilds the queue from the new
        // frame's raw input on every frame boundary, so anything pushed after
        // the field has run would be dropped.
        crate::android_ime::pump(ui.ctx(), &mut app.state);
    }

    // Reset each frame; text fields below set it when they hold focus.
    app.state.text_focus = false;

    // True fullscreen: nothing but the radar map — no status bar, timeline,
    // chips, badges or windows. Long-press again (or Esc on a desktop)
    // brings the chrome back.
    if app.state.fullscreen {
        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            set_fullscreen(ui.ctx(), app);
        }
        egui::CentralPanel::default().show(ui, |ui| {
            crate::ui::map::map(ui, app);
            app.map_rect_at = Some(ui.available_rect_before_wrap());
        });
        return;
    }

    // Android: the app window is full-screen and the system status bar is
    // drawn on top of it (black, ~24-26 dp). Reserve just that much: the old
    // fixed 64 pt left an ~88 px dead grey band below the bar in landscape.
    #[cfg(target_os = "android")]
    egui::Panel::top("android_top_inset").show(ui, |ui| {
        let content = ui.ctx().content_rect();
        let portrait = content.height() > content.width();
        let inset = ui
            .ctx()
            .input(|i| i.safe_area_insets().0.top)
            .max(if portrait { 30.0 } else { 26.0 });
        ui.add_space(inset);
    });

    // Top status bar.
    egui::Panel::top("status").show(ui, |ui| {
        status_bar(ui, app);
    });

    // Bottom timeline scrubber.
    // (No Android bottom spacer: the system gesture bar sits outside the app
    // window, and a spacer only added an empty band under the scrub strip.)
    egui::Panel::bottom("timeline").show(ui, |ui| {
        crate::timeline::timeline(ui, app);
    });

    // Middle rect (between top/bottom bars) before CentralPanel consumes it —
    // the overlay sidebar is positioned in these screen coords so it never
    // covers the status bar or timeline.
    let middle = ui.available_rect_before_wrap();

    // Central area: the map, or the map split with the 3D volume pane. The
    // sidebar floats above the panes instead of reserving space, so peeking
    // it in never resizes them (and never triggers a radar re-raster).
    egui::CentralPanel::default().show(ui, |ui| {
        central_panes(ui, app);
    });

    // Floating auto-hide sidebar, drawn over the map.
    sidebar_overlay(ui.ctx(), app, middle);

    // Bulk radar download/stream progress (top-center, above the map).
    load_progress_pill(ui.ctx(), app, middle);

    // Downloaded radar history (from the timeline's "Downloaded…" button).
    crate::timeline::downloads_window(ui.ctx(), app);

    // Alerts browser (its own floating window).
    alerts::alerts_popup(ui.ctx(), app);

    // NOAA detail popup for one alert (browser "Details", or a map click on a
    // warning polygon).
    alerts::alert_detail_popup(ui.ctx(), app);

    // AI meteorologist: the optional brief / question window and its settings.
    ai::ai_windows(ui.ctx(), app, middle);

    // NWS forecast window (station by default, or a point picked on the map).
    forecast::forecast_window(ui.ctx(), app);
    totals::totals_window(ui.ctx(), app);

    // Archive popup (opened from Settings).
    archive_window(ui.ctx(), app);

    // Per-pane control windows (opened from each pane's bottom-right chip).
    map_controls_window(ui.ctx(), app);
    view3d_controls_window(ui.ctx(), app);

    // Product explainer (the (i) beside the Product selector).
    product_help_window(ui.ctx(), app);

    // SPC discussion popup (clicking an outlook outline).
    outlook_detail_window(ui.ctx(), app);
    reports::lsr_detail_window(ui.ctx(), app);
    reports::mcd_detail_window(ui.ctx(), app);

    // Archived NWS text for a historical warning (clicked on the map).
    alerts::archive_detail_popup(ui.ctx(), app);

    // "Really clear …?" confirmation, drawn over everything. Rendered after
    // the panels so it isn't clipped by them; the Settings menu closes
    // itself before opening this.
    confirm_clear_dialog(ui.ctx(), app);
}

/// The bulk radar load pill: spinner, what is downloading, and a progress
/// bar — a multi-second prime or archive window must never read as a stuck
/// app. Auto-hides shortly after completion (or if the stream goes quiet).
fn load_progress_pill(ctx: &egui::Context, app: &mut StormApp, middle: egui::Rect) {
    let Some(p) = app.state.load_progress.clone() else {
        return;
    };
    let finished = p.total > 0 && p.done >= p.total;
    let stale = p.at.elapsed() > std::time::Duration::from_secs(8);
    if stale || (finished && p.at.elapsed() > std::time::Duration::from_millis(1500)) {
        app.state.load_progress = None;
        return;
    }
    let scale = app.sidebar_scale.max(1.0);
    egui::Area::new(egui::Id::new("stormscope.load_progress"))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(
            middle.center().x - 220.0 * scale,
            middle.min.y + 46.0 * scale,
        ))
        .constrain(true)
        .interactable(false)
        .fade_in(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .fill(egui::Color32::from_black_alpha(220))
                .show(ui, |ui| {
                    ui.set_min_width(440.0 * scale);
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(18.0 * scale));
                        ui.label(
                            egui::RichText::new(if finished {
                                format!("{} — rendering", p.label)
                            } else if p.cached {
                                format!("Fetching cache · {}", p.label)
                            } else {
                                format!("Downloading {}", p.label)
                            })
                            .strong()
                            .size(13.0 * scale),
                        );
                        if p.total > 0 {
                            ui.add(
                                egui::ProgressBar::new(p.done as f32 / p.total as f32)
                                    .desired_width(150.0 * scale)
                                    .text(format!("{}/{}", p.done, p.total)),
                            );
                        }
                    });
                });
        });
    // Keep the spinner spinning and the bar moving while the load runs.
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
}

/// The central area: 2D map alone, or side-by-side with the 3D volume pane.
///
/// Panes carry small chips in their top-right corner ("Max" / "Split" /
/// "Fit" / "3D") drawn *after* the panes so they sit on top of the map's
/// interaction widget — the pointer lands on the chip, not the map. The
/// split divider is a drag strip between the two panes.
fn central_panes(ui: &mut egui::Ui, app: &mut StormApp) {
    let full = ui.available_rect_before_wrap();
    let scale = app.sidebar_scale.max(1.0);
    let chip_y = |rect: egui::Rect| rect.min.y + 8.0 * scale;

    match (app.state.view3d_open, app.state.pane_max) {
        // 3D closed: the map, with a small opener chip for discoverability.
        (false, _) => {
            crate::ui::map::map(ui, app);
            app.map_rect_at = Some(full);
            let chip = pane_chip(
                ui,
                egui::pos2(full.max.x - 8.0 * scale, chip_y(full)),
                "3D",
                scale,
            )
            .on_hover_text("Show the 3D volume of this scan beside the map");
            if chip.clicked() {
                app.state.view3d_open = true;
                app.state.pane_max = PaneMax::None;
            }
            let controls = pane_chip_bottom(ui, bottom_right(full, scale), "Controls", scale)
                .on_hover_text("Map display controls (Min dBZ, opacity)");
            if controls.clicked() {
                app.state.map_controls_open = !app.state.map_controls_open;
            }
            #[cfg(target_os = "android")]
            settings_chip(ui, controls.rect, app, scale);
            ai_chip(ui, full, app, scale);
        }
        // The 3D pane maximized: volume fills the middle, "Split" brings the
        // map back and "Fit" re-frames the camera.
        (true, PaneMax::View3d) => {
            crate::ui::volume3d::show_pane(ui, app, full);
            app.view3d_rect_at = Some(full);
            let fit = pane_chip(
                ui,
                egui::pos2(full.max.x - 8.0 * scale, chip_y(full)),
                "Fit",
                scale,
            )
            .on_hover_text("Re-fit the camera to this volume");
            let split = pane_chip(
                ui,
                egui::pos2(fit.rect.min.x - 6.0 * scale, chip_y(full)),
                "Split",
                scale,
            )
            .on_hover_text("Show the 2D map beside the 3D view again");
            if fit.clicked() {
                crate::ui::volume3d::fit_view(&mut app.state);
            }
            if split.clicked() {
                app.state.pane_max = PaneMax::None;
            }
            let controls = pane_chip_bottom(ui, bottom_right(full, scale), "Controls", scale)
                .on_hover_text("3D display controls (echo, opacity, height, tilt, altitude)");
            if controls.clicked() {
                app.state.view3d_controls_open = !app.state.view3d_controls_open;
            }
        }
        // The map maximized (3D still on, one click away).
        (true, PaneMax::Map) => {
            crate::ui::map::map(ui, app);
            app.map_rect_at = Some(full);
            let split = pane_chip(
                ui,
                egui::pos2(full.max.x - 8.0 * scale, chip_y(full)),
                "Split",
                scale,
            )
            .on_hover_text("Show the 3D view beside the map again");
            if split.clicked() {
                app.state.pane_max = PaneMax::None;
            }
            let controls = pane_chip_bottom(ui, bottom_right(full, scale), "Controls", scale)
                .on_hover_text("Map display controls (Min dBZ, opacity)");
            if controls.clicked() {
                app.state.map_controls_open = !app.state.map_controls_open;
            }
            #[cfg(target_os = "android")]
            settings_chip(ui, controls.rect, app, scale);
            ai_chip(ui, full, app, scale);
        }
        // Side-by-side split with a draggable divider. On a phone in portrait
        // the split stacks vertically (map on top, volume below) instead —
        // side-by-side panes there are too narrow to be useful.
        (true, PaneMax::None) => {
            let portrait = cfg!(target_os = "android") && full.height() > full.width();
            let frac = app.state.pane_split.clamp(0.25, 0.75);
            // Wider on touch so a finger can grab it.
            let div_w = if cfg!(target_os = "android") {
                20.0
            } else {
                7.0
            };
            let (map_rect, div_rect, v3_rect) = if portrait {
                let usable = (full.height() - div_w).max(80.0);
                let map_rect =
                    egui::Rect::from_min_size(full.min, egui::vec2(full.width(), usable * frac));
                let div_rect = egui::Rect::from_min_size(
                    egui::pos2(full.min.x, map_rect.max.y),
                    egui::vec2(full.width(), div_w),
                );
                let v3_rect =
                    egui::Rect::from_min_max(egui::pos2(full.min.x, div_rect.max.y), full.max);
                (map_rect, div_rect, v3_rect)
            } else {
                let usable = (full.width() - div_w).max(80.0);
                let map_rect =
                    egui::Rect::from_min_size(full.min, egui::vec2(usable * frac, full.height()));
                let div_rect = egui::Rect::from_min_size(
                    egui::pos2(map_rect.max.x, full.min.y),
                    egui::vec2(div_w, full.height()),
                );
                let v3_rect =
                    egui::Rect::from_min_max(egui::pos2(div_rect.max.x, full.min.y), full.max);
                (map_rect, div_rect, v3_rect)
            };

            ui.scope_builder(egui::UiBuilder::new().max_rect(map_rect), |ui| {
                crate::ui::map::map(ui, app);
            });

            // Divider: drag to re-balance the panes (map keeps 25–75%).
            let resp = ui.interact(
                div_rect,
                egui::Id::new("stormscope.pane.divider"),
                egui::Sense::drag(),
            );
            if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(if portrait {
                    egui::CursorIcon::ResizeVertical
                } else {
                    egui::CursorIcon::ResizeHorizontal
                });
            }
            if resp.dragged() {
                if let Some(p) = resp.interact_pointer_pos() {
                    app.state.pane_split = if portrait {
                        ((p.y - full.min.y) / full.height().max(1.0)).clamp(0.25, 0.75)
                    } else {
                        ((p.x - full.min.x) / full.width().max(1.0)).clamp(0.25, 0.75)
                    };
                }
            }
            ui.painter()
                .rect_filled(div_rect, 0.0, egui::Color32::from_rgb(44, 48, 56));

            crate::ui::volume3d::show_pane(ui, app, v3_rect);

            ai_chip(ui, map_rect, app, scale);
            let map_max = pane_chip(
                ui,
                egui::pos2(map_rect.max.x - 8.0 * scale, chip_y(map_rect)),
                "Max",
                scale,
            )
            .on_hover_text("Maximize the 2D map");
            let fit = pane_chip(
                ui,
                egui::pos2(v3_rect.max.x - 8.0 * scale, chip_y(v3_rect)),
                "Fit",
                scale,
            )
            .on_hover_text("Re-fit the camera to this volume");
            let v3_max = pane_chip(
                ui,
                egui::pos2(fit.rect.min.x - 6.0 * scale, chip_y(v3_rect)),
                "Max",
                scale,
            )
            .on_hover_text("Maximize the 3D view");
            if map_max.clicked() {
                app.state.pane_max = PaneMax::Map;
            }
            if fit.clicked() {
                crate::ui::volume3d::fit_view(&mut app.state);
            }
            if v3_max.clicked() {
                app.state.pane_max = PaneMax::View3d;
            }
            app.map_rect_at = Some(map_rect);
            app.view3d_rect_at = Some(v3_rect);
            let map_controls =
                pane_chip_bottom(ui, bottom_right(map_rect, scale), "Controls", scale)
                    .on_hover_text("Map display controls (Min dBZ, opacity)");
            #[cfg(target_os = "android")]
            settings_chip(ui, map_controls.rect, app, scale);
            let v3_controls = pane_chip_bottom(ui, bottom_right(v3_rect, scale), "Controls", scale)
                .on_hover_text("3D display controls (echo, opacity, height, tilt, altitude)");
            if map_controls.clicked() {
                app.state.map_controls_open = !app.state.map_controls_open;
            }
            if v3_controls.clicked() {
                app.state.view3d_controls_open = !app.state.view3d_controls_open;
            }
        }
    }
}

/// Bottom-right anchor for a pane's chips.
fn bottom_right(rect: egui::Rect, scale: f32) -> egui::Pos2 {
    egui::pos2(rect.max.x - 8.0 * scale, rect.max.y - 8.0 * scale)
}

/// Android: a visible "Settings" chip that opens the controls sidebar. Touch
/// has no hover, so the auto-hide sidebar needs an explicit opener next to the
/// map's own Controls chip.
#[cfg(target_os = "android")]
fn settings_chip(ui: &mut egui::Ui, left_of: egui::Rect, app: &mut StormApp, scale: f32) {
    let chip = pane_chip_bottom(
        ui,
        egui::pos2(left_of.min.x - 6.0 * scale, left_of.max.y),
        "Settings",
        scale,
    )
    .on_hover_text("Open or hide the controls sidebar");
    if chip.clicked() {
        // Toggle: tapping again (or the ✕ in the header) closes it.
        app.state.sidebar_pinned = !app.state.sidebar_pinned;
    }
}

/// Like [`pane_chip`], but pinned by its bottom-right corner.
fn pane_chip_bottom(
    ui: &mut egui::Ui,
    right_bottom: egui::Pos2,
    label: &str,
    scale: f32,
) -> egui::Response {
    let font = egui::TextStyle::Small.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, egui::Color32::WHITE);
    let size = galley.size() + egui::vec2(16.0 * scale, 9.0 * scale);
    let rect = egui::Rect::from_min_size(
        egui::pos2(right_bottom.x - size.x, right_bottom.y - size.y),
        size,
    );
    ui.put(
        rect,
        egui::Button::new(egui::RichText::new(label).small())
            .fill(egui::Color32::from_black_alpha(170)),
    )
}

/// Like [`pane_chip_bottom`], but pinned by its bottom-left corner.
fn pane_chip_bottom_left(
    ui: &mut egui::Ui,
    left_bottom: egui::Pos2,
    label: &str,
    scale: f32,
) -> egui::Response {
    let font = egui::TextStyle::Small.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, egui::Color32::WHITE);
    let size = galley.size() + egui::vec2(16.0 * scale, 9.0 * scale);
    let rect = egui::Rect::from_min_size(egui::pos2(left_bottom.x, left_bottom.y - size.y), size);
    ui.put(
        rect,
        egui::Button::new(egui::RichText::new(label).small())
            .fill(egui::Color32::from_black_alpha(170)),
    )
}

/// The AI meteorologist opener, bottom-left of whichever 2D map is on screen.
fn ai_chip(ui: &mut egui::Ui, rect: egui::Rect, app: &mut StormApp, scale: f32) {
    let chip = pane_chip_bottom_left(
        ui,
        egui::pos2(rect.min.x + 8.0 * scale, rect.max.y - 8.0 * scale),
        "AI Meteorologist",
        scale,
    )
    .on_hover_text(
        "An AI meteorologist writes a short now / next-90-minutes brief for every marker, \
         answers questions, and can move the 2D/3D maps to show its answer. Configure the \
         endpoint in Settings → AI meteorologist.",
    );
    if chip.clicked() {
        log::debug!("AI meteorologist window opened from the map chip");
        app.state.ai_open = true;
    }
}

/// Place a controls window near the matching pane's bottom-right, clamped
/// into the screen. `pane` is the pane's last drawn rect (None before first
/// layout: falls back to the screen corner).
fn anchored_pos(
    ctx: &egui::Context,
    pane: Option<egui::Rect>,
    size: egui::Vec2,
    scale: f32,
) -> egui::Pos2 {
    let screen = ctx.content_rect();
    let margin = 12.0 * scale;
    let want = match pane {
        Some(r) => egui::pos2(
            r.max.x - size.x - margin,
            r.max.y - size.y - margin - 26.0 * scale,
        ),
        None => egui::pos2(
            screen.max.x - size.x - margin,
            screen.max.y - size.y - margin,
        ),
    };
    egui::pos2(
        want.x.clamp(
            screen.min.x + margin,
            (screen.max.x - size.x - margin).max(screen.min.x + margin),
        ),
        want.y.clamp(
            screen.min.y + margin,
            (screen.max.y - size.y - margin).max(screen.min.y + margin),
        ),
    )
}

/// Wheel over one of a control window's sliders adjusts it (one notch, one
/// step) and consumes the event, so the pane underneath cannot zoom.
fn control_window_wheel(
    ui: &egui::Ui,
    sliders: &[(u8, egui::Rect)],
    mut apply: impl FnMut(u8, f32),
) {
    let Some(pointer) = ui.ctx().pointer_hover_pos() else {
        return;
    };
    if !sliders.iter().any(|(_, r)| r.contains(pointer)) {
        return;
    }
    let mut acc = 0.0f32;
    let steps = ui
        .ctx()
        .input_mut(|i| map::consume_wheel_steps(&mut i.events, &mut acc));
    if steps == 0.0 {
        return;
    }
    ui.ctx()
        .input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
    if let Some((target, _)) = sliders.iter().find(|(_, r)| r.contains(pointer)) {
        apply(*target, steps);
    }
}

/// chrono → jiff for the date picker (egui_extras 0.36's datepicker speaks
/// jiff civil dates; the rest of the app is chrono).
fn jiff_from_naive(d: chrono::NaiveDate) -> jiff::civil::Date {
    use chrono::Datelike;
    jiff::civil::Date::new(d.year() as i16, d.month() as i8, d.day() as i8)
        .expect("valid civil date")
}

/// jiff → chrono, `None` for out-of-range dates (the picker clamps anyway).
fn naive_from_jiff(d: jiff::civil::Date) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::from_ymd_opt(d.year() as i32, d.month() as u32, d.day() as u32)
}

/// What one product shows, and how to read it. Copy lives with the UI
/// because that is where it is read.
struct ProductDoc {
    name: &'static str,
    what: &'static str,
    how: &'static str,
}

fn product_doc(m: Moment) -> ProductDoc {
    match m {
        Moment::Reflectivity => ProductDoc {
            name: "Base Reflectivity",
            what: "Returned power from precipitation, in dBZ — rain, hail, snow, and anything else \
                   that reflects. The core product: where the storm is, how it is shaped, how \
                   strong it is.",
            how: "40 dBZ is moderate rain, 50+ heavy, 60+ likely hail. Look for hook echoes, \
                  bowing lines and hail cores. Raise Min dBZ (map Controls) to thin light \
                  returns and see the structure.",
        },
        Moment::CompositeReflectivity => ProductDoc {
            name: "Composite Reflectivity",
            what: "The strongest return anywhere in each column, in dBZ — a storm's peak \
                   intensity rather than one tilt's slice.",
            how: "Use it like reflectivity when you care about the worst of a column: \
                  intense cores show even when the current tilt is above or below them.",
        },
        Moment::Velocity => ProductDoc {
            name: "Base Velocity",
            what: "How fast targets move toward or away from the radar, in m/s — only the \
                   component along the beam, so motion across the beam is invisible.",
            how: "Rotation reads as a tight inbound/outbound couplet; divergence as opposite \
                  colors spreading apart. Use the lowest tilts near the site: higher up, the \
                  beam is far from the storm's base.",
        },
        Moment::SpectrumWidth => ProductDoc {
            name: "Spectrum Width",
            what: "The spread of velocities inside one gate, in m/s — turbulence, shear, and \
                   mixed targets (rain plus hail, debris, ground clutter).",
            how: "High width along a gust front or inside a turbulent core backs up a velocity \
                  signature, and it lights up debris.",
        },
        Moment::DifferentialReflectivity => ProductDoc {
            name: "Differential Reflectivity",
            what: "How different the target looks horizontally vs vertically, in dB — a shape \
                   clue about the hydrometeors.",
            how: "Big raindrops read 2–5 dB, hail and tumbling or round drops near 0. A \
                  low-ZDR column above a storm's inflow can mark hail.",
        },
        Moment::DifferentialPhase => ProductDoc {
            name: "Differential Phase",
            what: "Accumulated phase shift along the beam, in degrees — the raw field behind \
                   KDP.",
            how: "Rarely read directly; use KDP, the range derivative computed from it.",
        },
        Moment::SpecificDifferentialPhase => ProductDoc {
            name: "Specific Differential Phase",
            what: "How fast phase accumulates with range, in deg/km. Less affected by hail \
                   contamination and attenuation than reflectivity.",
            how: "Heavy-rain cores read 1–3 deg/km. High KDP, often where reflectivity is \
                  modest, flags hail; use it where hail makes dBZ ambiguous.",
        },
        Moment::CorrelationCoefficient => ProductDoc {
            name: "Correlation Coefficient",
            what: "How consistent the returned pulse shapes are, 0–1.",
            how: "Near 1 is uniform precipitation. Values below ~0.8 mark hail, the melting \
                  layer, debris and clutter; a tornado debris signature is low CC inside a \
                  velocity couplet.",
        },
        Moment::Vil => ProductDoc {
            name: "Vertically Integrated Liquid",
            what: "The total water in the column, in kg/m², integrated from reflectivity over \
                   the beam heights.",
            how: "20–40 is ordinary convection, 50+ strong, 60–70+ hail potential — pair it \
                  with MEHS. Hail inflates VIL, so treat it as a severity hint, not a rain \
                  gauge.",
        },
        Moment::EchoTops => ProductDoc {
            name: "Echo Tops",
            what: "The height of the top of the 18.5 dBZ echo, in kft (thousands of feet) — \
                   how tall the storm is.",
            how: "Climbing tops mean an intensifying updraft; 50+ kft is strong. Within \
                  150–200 km tops are true heights; farther out the beams themselves are \
                  high, so read it over the storms.",
        },
        Moment::Mesh => ProductDoc {
            name: "Maximum Estimated Hail Size",
            what: "A hail-size estimate in inches, from the VIL column (0.1·sqrt(VIL), \
                   Witt et al.).",
            how: "A quick hint — 1 in+ is worth watching. It is an estimate, not a \
                  measurement, and only as good as the VIL beneath it.",
        },
        Moment::StormRelativeVelocity => ProductDoc {
            name: "Storm-Relative Velocity",
            what: "Radial velocity minus the storm's own motion, in m/s, so the storm's \
                   translation cancels out.",
            how: "Set the storm motion (toward + speed) in the 3D Controls window. Rotation \
                  then reads as a cleaner couplet; a wrong motion smears the signature.",
        },
        Moment::Hydrometeor => ProductDoc {
            name: "Hydrometeor Classification",
            what: "What the radar thinks the targets are, in eight classes: rain, heavy rain, \
                   dry snow, wet snow, mixed, ice crystals, graupel/hail, clutter/debris. \
                   Built from Z, ZDR, CC and KDP on the lowest cut.",
            how: "The winter workhorse: dry snow reads white, wet snow pale ice, the melting \
                  layer lavender, rain blue. Read it with the 0 °C level and the snow rate — \
                  the probe names the class under the cursor.",
        },
        Moment::SnowRate => ProductDoc {
            name: "Snowfall Rate",
            what: "Snowfall rate in inches per hour from reflectivity, via a winter Z-S \
                   relation (20 dBZ ≈ 0.5 in/h, 30 dBZ ≈ 1.5 in/h, about a 10:1 ratio).",
            how: "A hint, not a gauge: it is masked above 38 dBZ (rain or hail, not snow) \
                  and below 5 dBZ (noise). Use it for the banding structure, not totals.",
        },
        Moment::MeltingLayer => ProductDoc {
            name: "0 °C Level",
            what: "The melting-layer height in kft, from the correlation-coefficient bright \
                   band — the radar's view of the 0 °C level (a few hundred feet below it).",
            how: "Rain below, snow above; the map's 0 °C level overlay contours it every \
                  2 kft. A falling level means the snow is reaching lower.",
        },
    }
}

/// Reading habits that apply across the products, shown at the bottom of the
/// explainer.
const READING_TIPS: [&str; 5] = [
    "Tilt matters: low cuts show the storm base, high cuts the mid and upper structure.",
    "Trends beat single frames — scrub the timeline or play the loop.",
    "Hail inflates reflectivity and VIL; KDP, CC and ZDR help tell hail from heavy rain.",
    "The 3D pane, the cross-section and the probe show vertical structure and values at a point.",
    "Storm-relative velocity needs a plausible storm motion; the 3D Controls window has it.",
];

/// The product explainer, opened from the (i) beside the Product selector.
fn product_help_window(ctx: &egui::Context, app: &mut StormApp) {
    if !app.state.product_help_open {
        return;
    }
    let scale = app.sidebar_scale.max(1.0);
    let size = popup_size(ctx, scale, 560.0, 660.0);
    let mut open = true;
    egui::Window::new("Products — what they show")
        .id(egui::Id::new("stormscope.product_help"))
        .open(&mut open)
        .default_size(size)
        .max_height(popup_max_height(ctx, scale))
        .min_size(egui::vec2(420.0 * scale, 260.0 * scale))
        .default_pos(centered_popup_pos(ctx, size))
        .resizable(true)
        .show(ctx, |ui| {
            apply_text_scale(ui, scale);
            ui.label(
                egui::RichText::new(
                    "The selector lists what the current volume carries. Units follow each \
                     product — dBZ, m/s, deg/km, kg/m², kft, inches.",
                )
                .weak()
                .small(),
            );
            ui.separator();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .max_height((ui.available_height() - 6.0).max(160.0))
                .show(ui, |ui| {
                    for m in crate::state::available_moments(&app.state) {
                        let doc = product_doc(m);
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(m.label())
                                    .strong()
                                    .monospace()
                                    .color(egui::Color32::from_rgb(255, 214, 90)),
                            );
                            ui.label(egui::RichText::new(doc.name).strong());
                            if !m.unit().is_empty() {
                                ui.label(
                                    egui::RichText::new(format!("({})", m.unit()))
                                        .weak()
                                        .small(),
                                );
                            }
                        });
                        ui.label(egui::RichText::new(format!("What: {}", doc.what)));
                        ui.label(egui::RichText::new(format!("Use: {}", doc.how)));
                    }
                    ui.add_space(10.0);
                    ui.separator();
                    ui.label(egui::RichText::new("Reading tips").strong());
                    for tip in READING_TIPS {
                        ui.label(egui::RichText::new(format!("• {tip}")).weak());
                    }
                });
        });
    if !open {
        app.state.product_help_open = false;
    }
}

/// The 2D map's display controls (its bottom-right "Controls" chip).
fn map_controls_window(ctx: &egui::Context, app: &mut StormApp) {
    if !app.state.map_controls_open {
        return;
    }
    let scale = app.sidebar_scale.max(1.0);
    let size = egui::vec2(300.0 * scale, 340.0 * scale);
    let mut open = true;
    egui::Window::new("Map controls")
        .id(egui::Id::new("stormscope.map_controls"))
        .open(&mut open)
        .resizable(false)
        .default_size(size)
        .default_pos(anchored_pos(ctx, app.map_rect_at, size, scale))
        .show(ctx, |ui| {
            apply_text_scale(ui, scale);
            let mut sliders: Vec<(u8, egui::Rect)> = Vec::new();
            let ref_moment = matches!(
                app.state.moment,
                Moment::Reflectivity | Moment::CompositeReflectivity
            );
            if ref_moment {
                let r = ui.add(
                    egui::Slider::new(&mut app.state.min_dbz, 0.0..=75.0)
                        .step_by(5.0)
                        .text("Min dBZ"),
                );
                sliders.push((0, r.rect));
                ui.label(
                    egui::RichText::new("filters weak returns on REF products · scroll to adjust")
                        .weak()
                        .small(),
                );
            } else {
                ui.label(
                    egui::RichText::new("Min dBZ applies to REF products only")
                        .weak()
                        .small(),
                );
            }
            let r = ui.add(egui::Slider::new(&mut app.state.opacity, 0.0..=1.0).text("Opacity"));
            sliders.push((1, r.rect));
            if app.state.show_mrms {
                let r =
                    ui.add(egui::Slider::new(&mut app.state.mrms_opacity, 0.05..=1.0).text("MRMS"));
                sliders.push((10, r.rect));
            }
            if app.state.show_goes {
                let r =
                    ui.add(egui::Slider::new(&mut app.state.goes_opacity, 0.05..=1.0).text("GOES"));
                sliders.push((11, r.rect));
                ui.horizontal(|ui| {
                    ui.label("Satellite");
                    egui::ComboBox::from_id_salt("goes-layer")
                        .selected_text(app.state.goes_layer.label())
                        .show_ui(ui, |ui| {
                            for layer in [
                                radar_data::goes::GoesLayer::Infrared,
                                radar_data::goes::GoesLayer::GeoColor,
                            ] {
                                ui.selectable_value(
                                    &mut app.state.goes_layer,
                                    layer,
                                    layer.label(),
                                );
                            }
                        });
                });
            }
            ui.checkbox(&mut app.state.map_probe, "Data probe")
                .on_hover_text(
                    "Hover the map for the value under the cursor: product value, \
                     beam height, distance and bearing from the radar",
                );
            ui.checkbox(&mut app.state.show_melting_layer, "0 °C level")
                .on_hover_text(
                    "Isohypses of the melting-layer product (the radar's 0 °C level, \
                     from the CC bright band) every 2 kft — rain below, snow above",
                );
            ui.checkbox(&mut app.state.show_lightning, "Lightning (GLM)")
                .on_hover_text(
                    match (&app.state.lightning_satellite, app.state.lightning.len()) {
                        (Some(satellite), flashes) if flashes > 0 => format!(
                            "{satellite} · {flashes} flashes in the last 20 minutes. NOAA GOES GLM, \
                             one 20-second file per minute (~0.4 GB/day while on)."
                        ),
                        _ => "NOAA GOES-East/West GLM flashes, last 20 minutes. One 20-second \
                              file per minute (~0.4 GB/day). Official public-domain data; off by default."
                            .to_string(),
                    },
                );
            if app.state.show_lightning {
                let near = radar_core::sites::site(&app.state.site_id).map_or(0, |site| {
                    app.state
                        .lightning
                        .iter()
                        .filter(|strike| {
                            radar_render::surface_distance_m(
                                site.longitude,
                                site.latitude,
                                strike.lon,
                                strike.lat,
                            ) < 300_000.0
                        })
                        .count()
                });
                let status = if app.state.lightning_pending {
                    "fetching the next GLM file…".to_string()
                } else {
                    format!(
                        "{} · {} total · {} within 186 mi",
                        app.state.lightning_satellite.as_deref().unwrap_or("GOES"),
                        app.state.lightning.len(),
                        near
                    )
                };
                ui.label(egui::RichText::new(status).small().weak());
            }
            ui.add_space(4.0);
            let fill = warning_fill_slider(ui, app);
            sliders.push((12, fill));
            control_window_wheel(ui, &sliders, |target, steps| match target {
                0 => {
                    app.state.min_dbz = (app.state.min_dbz + 5.0 * steps).clamp(0.0, 75.0);
                }
                1 => {
                    app.state.opacity = (app.state.opacity + 0.05 * steps).clamp(0.0, 1.0);
                }
                10 => {
                    app.state.mrms_opacity =
                        (app.state.mrms_opacity + 0.05 * steps).clamp(0.05, 1.0);
                }
                11 => {
                    app.state.goes_opacity =
                        (app.state.goes_opacity + 0.05 * steps).clamp(0.05, 1.0);
                }
                12 => {
                    app.state.alerts_fill =
                        (app.state.alerts_fill + 0.05 * steps).clamp(0.0, 1.0);
                }
                _ => {}
            });
        });
    if !open {
        app.state.map_controls_open = false;
    }
}

/// The 3D pane's display controls (its bottom-right "Controls" chip).
fn view3d_controls_window(ctx: &egui::Context, app: &mut StormApp) {
    if !app.state.view3d_controls_open || !app.state.view3d_open {
        return;
    }
    let scale = app.sidebar_scale.max(1.0);
    let size = egui::vec2(330.0 * scale, 470.0 * scale);
    let mut open = true;
    egui::Window::new("3D controls")
        .id(egui::Id::new("stormscope.view3d_controls"))
        .open(&mut open)
        .resizable(false)
        .default_size(size)
        .default_pos(anchored_pos(ctx, app.view3d_rect_at, size, scale))
        .show(ctx, |ui| {
            apply_text_scale(ui, scale);
            let mut sliders: Vec<(u8, egui::Rect)> = Vec::new();
            ui.horizontal(|ui| {
                ui.label("Product");
                let before = app.state.view3d_moment;
                egui::ComboBox::from_id_salt("view3d-product")
                    .selected_text(app.state.view3d_moment.label())
                    .show_ui(ui, |ui| {
                        for moment in crate::state::available_moments(&app.state) {
                            let label = if moment.unit().is_empty() {
                                moment.label().to_string()
                            } else {
                                format!("{} ({})", moment.label(), moment.unit())
                            };
                            ui.selectable_value(&mut app.state.view3d_moment, moment, label);
                        }
                    });
                if app.state.view3d_moment != before {
                    let (lo, hi) =
                        radar_render::ColorTable::for_moment(app.state.view3d_moment).range();
                    app.state.view3d_threshold = app.state.view3d_threshold.clamp(lo, hi);
                    app.view3d_section_key = None;
                }
            });
            let (lo, hi) = radar_render::ColorTable::for_moment(app.state.view3d_moment).range();
            ui.horizontal(|ui| {
                ui.label("Echo ≥");
                let r = ui
                    .add(egui::Slider::new(&mut app.state.view3d_threshold, lo..=hi).step_by(1.0));
                sliders.push((0, r.rect));
            });
            let r =
                ui.add(egui::Slider::new(&mut app.state.view3d_alpha, 0.1..=1.0).text("Opacity"));
            sliders.push((1, r.rect));
            let r = ui.add(
                egui::Slider::new(&mut app.state.view3d_ve, 1.0..=20.0)
                    .step_by(0.5)
                    .text("Height ×"),
            );
            sliders.push((2, r.rect));

            let mut tilts: Vec<f32> = app
                .state
                .current()
                .map(|f| {
                    f.volume
                        .sweeps
                        .iter()
                        .filter(|sw| sw.moment(app.state.view3d_moment).is_some())
                        .map(|sw| sw.elevation_deg)
                        .collect()
                })
                .unwrap_or_default();
            tilts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            tilts.dedup_by(|a, b| (*a - *b).abs() < 0.01);
            ui.horizontal(|ui| {
                ui.label("Tilt");
                let label =
                    |v: Option<f32>| v.map_or_else(|| "all".to_string(), |t| format!("{t:.1}°"));
                egui::ComboBox::from_id_salt("view3d-tilt-lo")
                    .selected_text(label(app.state.view3d_tilt_lo))
                    .width(64.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut app.state.view3d_tilt_lo, None, "all");
                        for t in &tilts {
                            ui.selectable_value(
                                &mut app.state.view3d_tilt_lo,
                                Some(*t),
                                format!("{t:.1}°"),
                            );
                        }
                    });
                ui.label("to");
                egui::ComboBox::from_id_salt("view3d-tilt-hi")
                    .selected_text(label(app.state.view3d_tilt_hi))
                    .width(64.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut app.state.view3d_tilt_hi, None, "all");
                        for t in &tilts {
                            ui.selectable_value(
                                &mut app.state.view3d_tilt_hi,
                                Some(*t),
                                format!("{t:.1}°"),
                            );
                        }
                    });
            });
            if let (Some(lo), Some(hi)) = (app.state.view3d_tilt_lo, app.state.view3d_tilt_hi) {
                if lo > hi {
                    app.state.view3d_tilt_hi = Some(lo);
                }
            }

            let r = ui.add(
                egui::Slider::new(&mut app.state.view3d_alt_lo_ft, 0.0..=65_000.0)
                    .step_by(1_000.0)
                    .text("Altitude from")
                    .suffix(" ft"),
            );
            sliders.push((3, r.rect));
            let r = ui.add(
                egui::Slider::new(&mut app.state.view3d_alt_hi_ft, 0.0..=65_000.0)
                    .step_by(1_000.0)
                    .text("Altitude to")
                    .suffix(" ft"),
            );
            sliders.push((4, r.rect));
            if app.state.view3d_alt_lo_ft > app.state.view3d_alt_hi_ft {
                app.state.view3d_alt_hi_ft = app.state.view3d_alt_lo_ft;
            }

            ui.horizontal(|ui| {
                ui.label("Detail");
                egui::ComboBox::from_id_salt("view3d-detail")
                    .selected_text(app.state.view3d_detail.label())
                    .show_ui(ui, |ui| {
                        for d in radar_render::MeshDetail::ALL {
                            ui.selectable_value(&mut app.state.view3d_detail, d, d.label());
                        }
                    });
                if ui
                    .button("Fit view")
                    .on_hover_text("Re-fit the camera to this volume")
                    .clicked()
                {
                    crate::ui::volume3d::fit_view(&mut app.state);
                }
            });

            // --- Cross-section curtain (RHI) -----------------------------
            ui.separator();
            ui.checkbox(&mut app.state.view3d_section, "Cross-section")
                .on_hover_text(
                    "Cut a vertical slice through the volume along one azimuth \
                     (solid = data, transparent = no measurement)",
                );
            if app.state.view3d_section {
                ui.horizontal(|ui| {
                    ui.label("Azimuth");
                    let r = ui.add(
                        egui::Slider::new(&mut app.state.view3d_section_az, 0.0..=359.0)
                            .step_by(1.0)
                            .suffix("°"),
                    );
                    sliders.push((5, r.rect));
                });
            }

            // --- Isosurfaces (echo shells) -------------------------------
            let ref_like = matches!(
                app.state.moment,
                Moment::Reflectivity | Moment::CompositeReflectivity
            );
            ui.horizontal(|ui| {
                ui.label("Shells");
                for (i, dbz) in app.state.view3d_shell_preset.dbz().into_iter().enumerate() {
                    let mut on = app.state.view3d_iso[i];
                    let resp =
                        ui.add_enabled(ref_like, egui::Checkbox::new(&mut on, format!("{dbz:.0}")));
                    if resp.changed() {
                        app.state.view3d_iso[i] = on;
                    }
                }
                ui.label(
                    egui::RichText::new(if ref_like { "dBZ" } else { "REF only" })
                        .weak()
                        .small(),
                );
                egui::ComboBox::from_id_salt("view3d-shell-preset")
                    .selected_text(app.state.view3d_shell_preset.label())
                    .width(88.0 * scale)
                    .show_ui(ui, |ui| {
                        for preset in crate::state::ShellPreset::ALL {
                            ui.selectable_value(
                                &mut app.state.view3d_shell_preset,
                                preset,
                                preset.label(),
                            );
                        }
                    })
                    .response
                    .on_hover_text(
                        "Which dBZ the three shells mean: the convective set \
                         (30/50/65) or winter (10/20/30, where snow lives)",
                    );
            });

            // --- Storm-relative motion (the SRM product's inputs) --------
            if app.state.moment == Moment::StormRelativeVelocity {
                ui.separator();
                ui.label(
                    egui::RichText::new("Storm motion (SRM subtracts this)")
                        .small()
                        .weak(),
                );
                let r = ui.add(
                    egui::Slider::new(&mut app.state.srm_dir_deg, 0.0..=359.0)
                        .step_by(1.0)
                        .text("Toward")
                        .suffix("°"),
                );
                sliders.push((6, r.rect));
                let r = ui.add(
                    egui::Slider::new(&mut app.state.srm_speed_kt, 0.0..=80.0)
                        .step_by(1.0)
                        .text("Speed")
                        .suffix(" kt"),
                );
                sliders.push((7, r.rect));
            }

            // --- Ground overlays -----------------------------------------
            ui.checkbox(&mut app.state.view3d_alerts, "Warning polygons")
                .on_hover_text(
                    "NWS warning polygons draped on the 3D ground — its own \
                     switch, so the map's alert layer can differ",
                );
            if app.state.view3d_alerts {
                ui.horizontal(|ui| {
                    ui.add_space(18.0);
                    ui.label("Fill");
                    let r = ui.add(
                        egui::Slider::new(&mut app.state.view3d_alerts_fill, 0.0..=1.0)
                            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                    );
                    sliders.push((8, r.rect));
                });
                ui.horizontal(|ui| {
                    ui.add_space(18.0);
                    ui.label("Walls ≤");
                    let r = ui.add(
                        egui::Slider::new(&mut app.state.view3d_alerts_top_kft, 0.0..=70.0)
                            .step_by(5.0)
                            .suffix(" k ft"),
                    );
                    sliders.push((9, r.rect));
                })
                .response
                .on_hover_text(
                    "How high the warning walls rise: the polygon wraps the storm in \
                     3D instead of lying flat on the ground (0 = flat)",
                );
            }
            ui.checkbox(&mut app.state.view3d_probe, "Data probe")
                .on_hover_text(
                    "Hover the pane: first echo and strongest echo under the \
                     cursor, with height and distance",
                );

            control_window_wheel(ui, &sliders, |target, steps| match target {
                0 => {
                    app.state.view3d_threshold =
                        (app.state.view3d_threshold + 2.0 * steps).clamp(lo, hi);
                }
                1 => {
                    app.state.view3d_alpha =
                        (app.state.view3d_alpha + 0.05 * steps).clamp(0.1, 1.0);
                }
                2 => {
                    app.state.view3d_ve = (app.state.view3d_ve + 0.5 * steps).clamp(1.0, 20.0);
                }
                3 => {
                    app.state.view3d_alt_lo_ft = (app.state.view3d_alt_lo_ft + 1_000.0 * steps)
                        .clamp(0.0, app.state.view3d_alt_hi_ft);
                }
                4 => {
                    app.state.view3d_alt_hi_ft = (app.state.view3d_alt_hi_ft + 1_000.0 * steps)
                        .clamp(app.state.view3d_alt_lo_ft, 65_000.0);
                }
                5 => {
                    app.state.view3d_section_az =
                        (app.state.view3d_section_az + steps).rem_euclid(360.0);
                }
                6 => {
                    app.state.srm_dir_deg = (app.state.srm_dir_deg + steps).rem_euclid(360.0);
                }
                7 => {
                    app.state.srm_speed_kt = (app.state.srm_speed_kt + steps).clamp(0.0, 80.0);
                }
                8 => {
                    app.state.view3d_alerts_fill =
                        (app.state.view3d_alerts_fill + 0.05 * steps).clamp(0.0, 1.0);
                }
                9 => {
                    app.state.view3d_alerts_top_kft =
                        (app.state.view3d_alerts_top_kft + 5.0 * steps).clamp(0.0, 70.0);
                }
                _ => {}
            });
        });
    if !open {
        app.state.view3d_controls_open = false;
    }
}

/// A small chip button pinned by its right edge at `right_top`, used for the
/// pane controls. Sized to its label so "Max"/"Split"/"Fit" all fit.
fn pane_chip(ui: &mut egui::Ui, right_top: egui::Pos2, label: &str, scale: f32) -> egui::Response {
    let font = egui::TextStyle::Small.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, egui::Color32::WHITE);
    let size = galley.size() + egui::vec2(16.0 * scale, 9.0 * scale);
    let rect = egui::Rect::from_min_size(egui::pos2(right_top.x - size.x, right_top.y), size);
    ui.put(
        rect,
        egui::Button::new(egui::RichText::new(label).small())
            .fill(egui::Color32::from_black_alpha(170)),
    )
}

/// Extra text/element scale for the sidebar on high-res monitors.
///
/// Keyed off the *monitor's physical height* (`monitor points × native
/// scale`), not the window size: a small window on a 4K screen still gets
/// the boost. The boost tops the sidebar up so its total on-screen size
/// matches a 1080p baseline — i.e. `extra = (phys / 1080) / os_scale`:
///
/// * 4K @1x OS scale → 2.5x sidebar text (capped)
/// * 4K at 150% OS scale → ~1.67x on top of the 1.5x
/// * 4K @2x OS scale → 1.0 (the OS already doubled everything)
/// * 1080p → 1.0
///
/// Unknown monitor falls back to the window's physical height. Boost is
/// doubled past the baseline and capped at 2.5x.
fn hidpi_text_scale(
    monitor_points_h: Option<f32>,
    native_ppp: Option<f32>,
    pixels_per_point: f32,
    window_logical_h: f32,
) -> f32 {
    let phys_h = match (monitor_points_h, native_ppp) {
        (Some(mh), Some(nppp)) => mh * nppp,
        _ => window_logical_h * pixels_per_point,
    };
    let raw = (phys_h / 1080.0) / pixels_per_point.max(0.5);
    (1.0 + (raw - 1.0) * 2.0).clamp(1.0, 2.5)
}

/// Enlarge fonts + spacing by `scale` (see [`hidpi_text_scale`]). Applied to
/// a panel/window's root Ui each frame (a fresh style copy per frame, so
/// sizes never compound). Combo/menu popups live in their own layers and
/// keep context style.
pub(super) fn apply_text_scale(ui: &mut egui::Ui, scale: f32) {
    if (scale - 1.0).abs() < 0.01 {
        return;
    }
    let style = ui.style_mut();
    for font in style.text_styles.values_mut() {
        font.size = (font.size * scale).clamp(9.0, 64.0);
    }
    style.spacing.item_spacing *= scale;
    style.spacing.button_padding *= scale;
    style.spacing.indent *= scale;
    style.spacing.slider_width *= scale;
    style.spacing.combo_width *= scale;
    style.spacing.text_edit_width *= scale;
}

/// Pure open/close decision for the auto-hide sidebar (kept free of egui
/// input plumbing so it can be unit-tested).
///
/// * `pinned` — user asked to keep the sidebar open.
/// * `was_open` — the target from last frame (drives the hysteresis: a
///   narrow edge zone opens it, a wider zone keeps it open).
/// * `pressing` — any pointer button held: suppresses peeking open so a map
///   pan ending at the left edge doesn't pop the sidebar mid-drag.
/// * `pointer` — latest pointer position in screen coords (`None` when the
///   pointer left the window / touch without hover).
/// * `middle` — the rect between the top status bar and bottom timeline.
fn sidebar_want_open(
    pinned: bool,
    was_open: bool,
    pressing: bool,
    pointer: Option<egui::Pos2>,
    middle: egui::Rect,
    panel_w: f32,
) -> bool {
    if pinned {
        return true;
    }
    let Some(p) = pointer else {
        return false;
    };
    if !middle.y_range().contains(p.y) {
        return false;
    }
    // Narrow zone at the left edge peeks it open (never mid-gesture).
    if !pressing && p.x - middle.min.x < SIDEBAR_EDGE_PX {
        return true;
    }
    // Wide zone keeps it open while interacting (sliders, combos, popups).
    was_open && p.x < middle.min.x + panel_w + SIDEBAR_CLOSE_SLOP
}

/// Full-height target for the sidebar panel: the middle rect minus a small
/// margin, floored so it stays usable on short windows.
fn sidebar_panel_height(middle: egui::Rect) -> f32 {
    (middle.height() - 16.0).max(120.0)
}

/// Auto-hiding controls sidebar: slides in from the left when the pointer
/// touches the left edge (or when pinned), slides back out once the pointer
/// leaves the sidebar zone.
fn sidebar_overlay(ctx: &egui::Context, app: &mut StormApp, middle: egui::Rect) {
    // High-res auto-scale from the *monitor*: larger panel + text on tall
    // physical displays (e.g. 4K), topping up whatever OS scaling exists.
    let ppp = ctx.pixels_per_point();
    let (mon_h, nppp) = ctx.input(|i| {
        (
            i.viewport().monitor_size.map(|s| s.y),
            i.viewport().native_pixels_per_point,
        )
    });
    let scale = hidpi_text_scale(mon_h, nppp, ppp, middle.height());
    app.sidebar_scale = scale;
    let panel_w = SIDEBAR_WIDTH * scale;

    let pointer = ctx.pointer_latest_pos();
    // Don't peek open mid-gesture: a map pan ending at the left edge must
    // not pop the sidebar over the map mid-drag.
    let pressing = ctx.input(|i| i.pointer.any_down());
    let want_open = sidebar_want_open(
        app.state.sidebar_pinned,
        app.sidebar_open,
        pressing,
        pointer,
        middle,
        panel_w,
    );
    app.sidebar_open = want_open;

    let frac = ctx.animate_bool_with_time(
        egui::Id::new("stormscope.sidebar"),
        want_open,
        SIDEBAR_ANIM_SECS,
    );

    if frac <= 0.001 {
        edge_hint(ctx, app, middle, scale);
        return;
    }
    if frac < 1.0 {
        ctx.request_repaint();
    }

    // Slide from fully off-screen (left of the middle rect) to docked.
    let x_off = -(panel_w + 12.0) * (1.0 - frac);
    let pos = egui::pos2(middle.min.x + x_off, middle.min.y);

    egui::Area::new(egui::Id::new("stormscope.sidebar.area"))
        .fixed_pos(pos)
        .movable(false)
        .interactable(true)
        .constrain(false)
        .fade_in(false)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            apply_text_scale(ui, scale);
            let frame = egui::Frame::side_top_panel(ui.style());
            frame.show(ui, |ui| {
                ui.set_min_width(panel_w);
                ui.set_max_width(panel_w);
                // Pin the full middle height: a bare Area sizes to its
                // content, which left the sidebar as a short floating box.
                // With the height fixed, long control lists scroll inside
                // instead of spilling over the timeline.
                let h = sidebar_panel_height(middle);
                ui.set_min_height(h);
                ui.set_max_height(h);
                egui::ScrollArea::vertical()
                    .max_height(h)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        controls(ui, app);
                    });
            });
        });
}

/// Slim "›" tab at the left edge while the sidebar is hidden — a visual cue
/// that controls live there. Non-interactive (clicks pass through to the
/// map); hovering it already peeks the sidebar via the edge zone.
fn edge_hint(ctx: &egui::Context, app: &mut StormApp, middle: egui::Rect, scale: f32) {
    let cy = middle.center().y;
    let pos = egui::pos2(middle.min.x + 3.0, cy - 26.0);
    let resp = egui::Area::new(egui::Id::new("stormscope.sidebar.hint"))
        .fixed_pos(pos)
        .movable(false)
        .interactable(true)
        .constrain(false)
        .fade_in(false)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(egui::Color32::from_black_alpha(150))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(5, 10))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new("❯")
                            .color(egui::Color32::from_white_alpha(200))
                            .size(14.0 * scale),
                    );
                });
        })
        .response;
    if resp.clicked() {
        app.state.sidebar_pinned = true;
    }
    if resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
}

fn status_bar(ui: &mut egui::Ui, app: &mut StormApp) {
    ui.horizontal_wrapped(|ui| {
        ui.strong(if cfg!(target_os = "android") {
            "Stormscope (android)"
        } else {
            "Stormscope"
        });
        ui.separator();
        ui.label(format!("site {}", app.state.site_id));
        ui.label(format!("{}", app.state.moment.label()));
        if !cfg!(target_os = "android") {
            ui.label(format!("{:.1}°", app.state.tilt_deg));
        }
        if let Some(n) = app.state.newest_start() {
            ui.label(format!("scan {}", crate::timefmt::fmt_central(&n)));
        }
        if !cfg!(target_os = "android") {
            if let Some(lat) = app.state.latency() {
                ui.label(format!("+{}m", lat.num_minutes()));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(app.state.status_text.clone());
            });
        }
    });
}

fn controls(ui: &mut egui::Ui, app: &mut StormApp) {
    ui.horizontal(|ui| {
        ui.heading("Controls");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("✕")
                .on_hover_text("Hide the controls")
                .clicked()
            {
                app.state.sidebar_pinned = false;
            }
            settings_menu(ui, app);
            // Android opens/closes the sidebar from the map's Settings chip
            // and the ✕ here, so the Pin toggle is a desktop-only affordance.
            if !cfg!(target_os = "android") {
                ui.checkbox(&mut app.state.sidebar_pinned, "Pin")
                    .on_hover_text("Keep the sidebar open instead of auto-hiding");
            }
        });
    });
    if !app.state.sidebar_pinned {
        ui.label(
            egui::RichText::new(if cfg!(target_os = "android") {
                "tap Settings (map corner) to open · ✕ to close"
            } else {
                "hover the left edge to peek · Pin to keep open"
            })
            .weak()
            .small(),
        );
    }
    if app.sidebar_scale > 1.01 {
        ui.label(
            egui::RichText::new(format!(
                "high-res display · sidebar ×{:.2}",
                app.sidebar_scale
            ))
            .weak()
            .small(),
        );
    }
    ui.add_space(6.0);

    // Site selector.
    ui.add_space(6.0);
    ui.label("Radar site");
    let sites: Vec<&str> = app
        .data
        .lock()
        .unwrap()
        .sites
        .iter()
        .map(|s| s.id)
        .collect();
    egui::ComboBox::from_id_salt("site")
        .selected_text(app.state.site_id.clone())
        .show_ui(ui, |ui| {
            for id in sites {
                ui.selectable_value(&mut app.state.site_id, id.to_string(), id);
            }
        });
    if ui.button("Apply site").clicked() {
        let _ = app
            .tx
            .send(crate::app::DataCmd::SetSite(app.state.site_id.clone()));
    }

    // Product (moment) selector: whatever the volume actually carries —
    // decoded base moments plus the derived products attached at decode
    // (VIL / Echo Tops / MEHS) and SRM, which is derived on demand from the
    // motion sliders.
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("Product");
        if ui
            .small_button("(i)")
            .on_hover_text("What each product shows, and when to use it")
            .clicked()
        {
            app.state.product_help_open = !app.state.product_help_open;
        }
    });
    ui.horizontal_wrapped(|ui| {
        for m in crate::state::available_moments(&app.state) {
            let selected = app.state.moment == m;
            if ui.selectable_label(selected, m.label()).clicked() {
                app.state.moment = m;
            }
        }
    });

    // Tilt selector.
    ui.add_space(6.0);
    let tilts = app.data.lock().unwrap().tilts.clone();
    ui.label("Tilt (°)");
    let mut tilt_idx = tilts
        .iter()
        .enumerate()
        .min_by(|a, b| {
            (a.1 - app.state.tilt_deg)
                .abs()
                .partial_cmp(&(b.1 - app.state.tilt_deg))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(0);
    egui::ComboBox::from_id_salt("tilt")
        .selected_text(format!(
            "{:.1}°",
            tilts.get(tilt_idx).copied().unwrap_or(0.5)
        ))
        .show_ui(ui, |ui| {
            for (i, t) in tilts.iter().enumerate() {
                ui.selectable_value(&mut tilt_idx, i, format!("{t:.1}°"));
            }
        });
    if let Some(t) = tilts.get(tilt_idx) {
        app.state.tilt_deg = *t;
    }

    // The radar display sliders (opacity / Min dBZ) moved into the map's
    // "Controls" window, bottom-right of the map itself.

    // Alerts toggle.
    ui.add_space(6.0);
    ui.checkbox(&mut app.state.show_mrms, "MRMS mosaic")
        .on_hover_text(match &app.state.mrms {
            Some((m, at)) => {
                let first = m.first();
                format!(
                    "{} — the multi-radar composite ({} · fetched {} min ago, \
                     CONUS + a sharp radar window). Downloads the newest file when \
                     enabled and every 10 minutes.",
                    first.map_or("MRMS", |x| x.product.label()),
                    first.map_or("--:--Z".to_string(), |x| x
                        .time
                        .format("%H:%MZ")
                        .to_string()),
                    at.elapsed().as_secs() / 60
                )
            }
            None if app.state.mrms_requested_at.is_some() => {
                "Downloading the multi-radar composite…".to_string()
            }
            None => "Multi-radar composite reflectivity, under the radar layer — \
                     downloads on enable."
                .to_string(),
        });
    ui.checkbox(&mut app.state.show_goes, "GOES satellite")
        .on_hover_text(match &app.state.goes {
            Some((img, at)) => {
                let first = img.first();
                format!(
                    "{} from {} — NASA GIBS, fetched {} min ago (CONUS + a sharp \
                     radar window; refreshes every 10 minutes)",
                    first.map_or("Satellite", |x| x.layer.label()),
                    first.map_or("GOES", |x| if x.west_satellite {
                        "GOES-West"
                    } else {
                        "GOES-East"
                    }),
                    at.elapsed().as_secs() / 60
                )
            }
            None if app.state.goes_pending.is_some() => {
                "Downloading the satellite frame…".to_string()
            }
            None => "GOES infrared/GeoColor composite under the radar — downloads on \
                     enable."
                .to_string(),
        });
    ui.checkbox(&mut app.state.show_alerts, "Show warnings");
    ui.checkbox(&mut app.state.show_tracks, "Show storm tracks");
    ui.checkbox(&mut app.state.show_lsrs, "Storm reports")
        .on_hover_text("NWS Preliminary Local Storm Reports from the last 12 hours, plotted where the report places them; white-ringed dots are trained spotters");
    ui.checkbox(&mut app.state.show_mcds, "Mesoscale discussions")
        .on_hover_text(
            "SPC mesoscale discussion areas — hover for the watch chance, click for the full text",
        );
    ui.checkbox(&mut app.state.show_totals, "Station totals on map")
        .on_hover_text(
            "Diamonds at the NWS stations in this site's area, tagged with their \
             strongest rain/snow number; hover for the table, click for the totals window",
        );
    let alert_count = if app.state.all_alerts.is_empty() {
        app.state.alerts.len()
    } else {
        app.state.all_alerts.len()
    };
    if ui
        .button(format!("⚠ Alerts ({alert_count})"))
        .on_hover_text("Browse active alerts: all US, or just this station")
        .clicked()
    {
        app.state.alerts_popup_open = !app.state.alerts_popup_open;
    }
    if ui
        .button("💧 Rain & snow totals")
        .on_hover_text(
            "What nearby NWS stations measured: rolling rain/snow buckets \
             and an event total summed from their hourly observations",
        )
        .clicked()
    {
        app.state.totals_open = !app.state.totals_open;
    }

    // 3D volume pane: the split/max controls live on the panes themselves
    // (top-right corner chips); this is the on/off + the display settings.
    ui.add_space(6.0);
    let mut view3d_open = app.state.view3d_open;
    if ui
        .checkbox(&mut view3d_open, "3D radar pane")
        .on_hover_text(
            "All elevation cuts of the current scan as one rotatable volume, \
             beside the map: drag the 3D pane to orbit, right-drag (or \
             middle-drag) to pan freely, wheel to zoom, double-click (or \
             Fit) to re-frame, and the bottom-right \"Controls\" chip opens \
             the display sliders. The \"Max\" chip maximizes either pane; \
             the divider between them drags",
        )
        .changed()
    {
        app.state.view3d_open = view3d_open;
        if !view3d_open {
            app.state.pane_max = crate::state::PaneMax::None;
        }
    }
    // The 3D display controls (echo / opacity / height / tilt / altitude /
    // detail) live in the pane's own "Controls" window now.
    // --- SPC convective outlook overlay ------------------------------------
    ui.add_space(6.0);
    let toggled = ui
        .checkbox(&mut app.state.show_outlook, "Show SPC outlook")
        .changed();
    if toggled && app.state.show_outlook {
        // Fetch the selected day fresh when the layer is switched on.
        let _ = app.tx.send(crate::app::DataCmd::LoadOutlook(
            app.state.outlook_day,
            app.state.outlook_type,
        ));
    }
    if app.state.show_outlook {
        ui.horizontal(|ui| {
            for d in 1u8..=3 {
                if ui
                    .selectable_label(app.state.outlook_day == d, format!("Day {d}"))
                    .clicked()
                {
                    app.state.outlook_day = d;
                    // Day 3 has no probability products: fall back so the
                    // selector and the fetch always agree.
                    if d == 3 && app.state.outlook_type.is_probability() {
                        app.state.outlook_type = radar_core::outlooks::OutlookType::Categorical;
                    }
                    let _ = app
                        .tx
                        .send(crate::app::DataCmd::LoadOutlook(d, app.state.outlook_type));
                }
            }
        });
        // Product type, as SPC offers it: categorical always; the
        // probabilistic outlooks (chance within 25 miles of a point) exist
        // for days 1–2 only, so day 3 disables them.
        ui.horizontal_wrapped(|ui| {
            use radar_core::outlooks::OutlookType as T;
            for t in [T::Categorical, T::Tornado, T::Wind, T::Hail] {
                let enabled = t == T::Categorical || app.state.outlook_day < 3;
                let selected = app.state.outlook_type == t;
                // egui 0.36 has no disabled selectable widget (only the Ui
                // method remains), so unavailable options explain themselves
                // on hover instead of looking broken.
                let resp = ui.selectable_label(selected, t.label());
                let resp = if enabled {
                    resp
                } else {
                    resp.on_hover_text("SPC publishes no probability outlooks for day 3")
                };
                if enabled && resp.clicked() {
                    app.state.outlook_type = t;
                    let _ = app
                        .tx
                        .send(crate::app::DataCmd::LoadOutlook(app.state.outlook_day, t));
                }
            }
        });
        match &app.state.outlook {
            Some(o) if o.day == app.state.outlook_day && o.ty == app.state.outlook_type => {
                let issued = o
                    .issue
                    .map(|t| crate::timefmt::fmt_central(&t))
                    .unwrap_or_else(|| "?".into());
                ui.label(
                    egui::RichText::new(format!("{} — issued {issued}", o.forecaster))
                        .weak()
                        .small(),
                );
                if let Some(expire) = o.expire {
                    ui.label(
                        egui::RichText::new(format!(
                            "valid until {}",
                            crate::timefmt::fmt_central(&expire)
                        ))
                        .weak()
                        .small(),
                    );
                }
                ui.add_space(2.0);
                if o.ty == radar_core::outlooks::OutlookType::Categorical {
                    // Legend: one row per risk category present, most severe
                    // first, with the NWS wording for what that level means.
                    let mut features: Vec<&radar_core::outlooks::OutlookFeature> =
                        o.features.iter().collect();
                    features.sort_by_key(|f| std::cmp::Reverse(f.risk_level().unwrap_or(0)));
                    for f in features {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                            ui.painter().rect_filled(
                                rect,
                                2.0,
                                egui::Color32::from_rgb(f.stroke[0], f.stroke[1], f.stroke[2]),
                            );
                            ui.label(egui::RichText::new(f.headline()).strong());
                        });
                        ui.label(egui::RichText::new(f.description()).weak().small());
                    }
                } else {
                    // Probability legend: SPC's own LABEL2 per area ("2%
                    // Tornado Risk") in SPC's own colors, plus once the
                    // definition SPC uses for these outlooks.
                    if o.features.is_empty() {
                        ui.label(
                            egui::RichText::new(format!(
                                "no {} areas in the day {} outlook",
                                o.ty.label().to_lowercase(),
                                o.day
                            ))
                            .weak()
                            .small(),
                        );
                    }
                    for f in &o.features {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                            ui.painter().rect_filled(
                                rect,
                                2.0,
                                egui::Color32::from_rgb(f.stroke[0], f.stroke[1], f.stroke[2]),
                            );
                            let text = if f.name.is_empty() {
                                f.probability_text()
                            } else {
                                f.name.clone()
                            };
                            ui.label(egui::RichText::new(text).strong());
                        });
                    }
                    if !o.features.is_empty() {
                        ui.label(
                            egui::RichText::new(format!(
                                "chance of {} within 25 miles of a point",
                                o.ty.noun()
                            ))
                            .weak()
                            .small(),
                        );
                    }
                }
            }
            _ => {
                ui.label(
                    egui::RichText::new(format!(
                        "loading {} outlook day {}…",
                        app.state.outlook_type.label(),
                        app.state.outlook_day
                    ))
                    .weak()
                    .small(),
                );
            }
        }
        ui.label("Outlook fill");
        ui.add(egui::Slider::new(&mut app.state.outlook_opacity, 0.0..=1.0));
        ui.label(
            egui::RichText::new("risk fills sit under the radar; outlines on top")
                .weak()
                .small(),
        );
    }

    // --- Measure tool (RadarScope-style distance line + blue bubble) -----
    ui.add_space(8.0);
    ui.label("Measure");
    ui.horizontal(|ui| {
        let label = if app.state.measure_mode {
            "✕ Stop measuring"
        } else {
            "📏 Measure distance"
        };
        if ui
            .button(label)
            .on_hover_text("Drag on the map to measure — ends snap to pins")
            .clicked()
        {
            app.state.measure_mode = !app.state.measure_mode;
            app.state.measure_drag = None;
        }
        if (app.state.measure_a.is_some() || app.state.measure_b.is_some())
            && ui
                .button("Clear")
                .on_hover_text("Clear the measurement")
                .clicked()
        {
            app.state.measure_a = None;
            app.state.measure_b = None;
            app.state.measure_drag = None;
            app.state.measure_bubble_rect = None;
        }
    });
    match (
        app.state.measure_mode,
        app.state.measure_a,
        app.state.measure_b,
    ) {
        (true, Some(a), Some(b)) => {
            let m = radar_render::surface_distance_m(a.0, a.1, b.0, b.1);
            ui.label(
                egui::RichText::new(format!(
                    "{} · tap bubble for km/brg",
                    crate::ui::map::format_miles(m / 1609.344)
                ))
                .strong(),
            );
        }
        (true, _, _) => {
            ui.label(
                egui::RichText::new("drag on the map — ends snap to pins")
                    .weak()
                    .small(),
            );
        }
        (false, _, _) => {}
    }

    // --- User-placed map markers ------------------------------------------
    ui.add_space(8.0);
    ui.label("Markers");
    ui.horizontal(|ui| {
        let label = if app.state.add_marker_mode {
            "✕ Cancel placement"
        } else {
            "＋ Place marker"
        };
        if ui
            .button(label)
            .on_hover_text("Then click the map (Shift+click works too)")
            .clicked()
        {
            app.state.add_marker_mode = !app.state.add_marker_mode;
        }
        if !app.state.markers.is_empty()
            && ui
                .button("Clear all")
                .on_hover_text("Remove every marker")
                .clicked()
        {
            app.state.markers.clear();
            app.state.pending_marker = None;
            app.state.pending_marker_label.clear();
            app.state.persist_markers();
        }
    });

    // Rows: click = pan to it, ✎ = rename, ✕ = delete.
    let mut pan_to: Option<(f64, f64)> = None;
    let mut edit: Option<usize> = None;
    let mut del: Option<usize> = None;
    for (i, m) in app.state.markers.iter().enumerate() {
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::new(m.display()).wrap())
                .on_hover_text(format!(
                    "{:.4}, {:.4} — click to center the map",
                    m.lat, m.lon
                ))
                .clicked()
            {
                pan_to = Some((m.lon, m.lat));
            }
            if ui.small_button("✎").on_hover_text("Rename").clicked() {
                edit = Some(i);
            }
            if ui.small_button("✕").on_hover_text("Delete").clicked() {
                del = Some(i);
            }
        });
        // Incoming-cell ETA under the row, when the tracker has one.
        if let Some(eta) = app.marker_etas.get(i).and_then(|e| e.as_ref()) {
            ui.label(
                egui::RichText::new(format!(
                    "⛈ ~{} · {:.0} mph {}",
                    crate::ui::map::fmt_eta(eta.minutes),
                    eta.speed_mph,
                    eta.compass
                ))
                .weak()
                .small(),
            );
        }
    }
    if app.state.markers.is_empty() {
        ui.label(
            egui::RichText::new("Shift+click the map to drop a pin")
                .weak()
                .small(),
        );
    }
    if let Some((lon, lat)) = pan_to {
        app.state.view.center_lon = lon;
        app.state.view.center_lat = lat;
    }
    if let Some(i) = edit {
        // Seed the persistent draft the popup edits (immediate-mode TextEdit).
        app.state.pending_marker_label = app
            .state
            .markers
            .get(i)
            .map(|m| m.label.clone())
            .unwrap_or_default();
        app.state.pending_marker = Some(i);
    }
    if let Some(i) = del {
        if i < app.state.markers.len() {
            app.state.markers.remove(i);
        }
        if app.state.pending_marker == Some(i) {
            app.state.pending_marker = None;
            app.state.pending_marker_label.clear();
        }
        app.state.persist_markers();
    }
}

/// App-level "Settings" popup: live per-section cache usage and one clear
/// button per section (each gated behind [`confirm_clear_dialog`]).
fn settings_menu(ui: &mut egui::Ui, app: &mut StormApp) {
    ui.menu_button("Settings", |ui| {
        ui.set_min_width(260.0);
        ui.label(egui::RichText::new("Disk cache").strong());
        ui.add_space(4.0);
        cache_row(
            ui,
            "Radar data",
            "Downloaded radar volumes",
            app.state.cache_radar_bytes,
            crate::app::ClearTarget::Radar,
            app,
        );
        ui.separator();
        cache_row(
            ui,
            "Map data",
            "Cached basemap tiles",
            app.state.cache_tile_bytes,
            crate::app::ClearTarget::Tiles,
            app,
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("cap 10 GB across both — oldest evicted first")
                .weak()
                .small(),
        );
        ui.separator();
        ui.label(egui::RichText::new("Data").strong());
        ui.add_space(4.0);
        if ui
            .button("Archive…")
            .on_hover_text(
                "Browse the past: pick a day and a time of day, page through \
                 the day's scans, and loop them",
            )
            .clicked()
        {
            app.state.archive_popup_open = true;
        }
        ui.label(
            egui::RichText::new(match app.state.mode {
                Mode::Live => "now: live",
                Mode::Archive => "now: archive",
            })
            .weak()
            .small(),
        );
        ui.separator();
        ui.label(egui::RichText::new("AI").strong());
        ui.add_space(4.0);
        if ui
            .button("AI meteorologist…")
            .on_hover_text("Configure the endpoint, model and API key for the brief")
            .clicked()
        {
            app.state.ai_settings_open = true;
        }
        ui.label(
            egui::RichText::new("OpenCode Go · deepseek-v4.1-flash by default")
                .weak()
                .small(),
        );
    });
}

/// The Archive popup (opened from Settings): day + time-of-day pickers, the
/// load actions, paging, and real feedback — the times of what is loaded and
/// where the window sits in the day, instead of bare "earlier/later".
fn archive_window(ctx: &egui::Context, app: &mut StormApp) {
    if !app.state.archive_popup_open {
        return;
    }
    // Feedback stays fresh while a load streams in.
    ctx.request_repaint_after(std::time::Duration::from_millis(400));
    let scale = app.sidebar_scale.max(1.0);
    let size = popup_size(ctx, scale, 430.0, 470.0);
    let mut open = app.state.archive_popup_open;

    egui::Window::new("Archive")
        .id(egui::Id::new("stormscope.archive"))
        .open(&mut open)
        .default_size(size)
        .min_size(egui::vec2(330.0 * scale, 300.0 * scale))
        .default_pos(centered_popup_pos(ctx, size))
        .resizable(true)
        .show(ctx, |ui| {
            apply_text_scale(ui, scale);
            // Mode.
            ui.horizontal(|ui| {
                let mut live = app.state.mode == Mode::Live;
                if ui.selectable_value(&mut live, true, "Live").changed() && live {
                    app.snap_to_live();
                }
                let mut arch = app.state.mode == Mode::Archive;
                if ui.selectable_value(&mut arch, true, "Archive").changed() && arch {
                    app.state.mode = Mode::Archive;
                }
            });
            ui.add_space(6.0);

            // Day (the same shared archive day as the alerts Archive tab).
            ui.label("Day (Central)");
            ui.horizontal(|ui| {
                if ui.button("◀").clicked() {
                    app.state.archive_date = app
                        .state
                        .archive_date
                        .pred_opt()
                        .unwrap_or(app.state.archive_date);
                }
                // Clickable date: calendar picking, like the alerts tab.
                let today = chrono::Utc::now()
                    .with_timezone(crate::timefmt::central())
                    .date_naive();
                let mut picked = jiff_from_naive(app.state.archive_date);
                ui.add(egui_extras::DatePickerButton::new(&mut picked))
                    .on_hover_text("Pick a day from the calendar");
                if let Some(picked) = naive_from_jiff(picked) {
                    if picked != app.state.archive_date {
                        app.state.archive_date = picked.min(today);
                    }
                }
                if ui.button("▶").clicked() {
                    if app.state.archive_date < today {
                        app.state.archive_date = app
                            .state
                            .archive_date
                            .succ_opt()
                            .unwrap_or(app.state.archive_date);
                    }
                }
            });

            // Time of day.
            ui.add_space(4.0);
            ui.label("Time of day (Central)");
            ui.add(
                egui::Slider::new(&mut app.state.archive_minute, 0..=1435)
                    .step_by(5.0)
                    .custom_formatter(|v, _| {
                        format!("{:02}:{:02}", (v as u32) / 60, (v as u32) % 60)
                    }),
            );
            ui.horizontal(|ui| {
                if ui
                    .button("Load at time")
                    .on_hover_text("Load the scans around this time")
                    .clicked()
                {
                    app.state.mode = Mode::Archive;
                    let _ = app.tx.send(crate::app::DataCmd::LoadArchiveDate(
                        app.state.site_id.clone(),
                        app.state.archive_date,
                        Some(app.state.archive_minute),
                    ));
                }
                if ui
                    .button("Newest first")
                    .on_hover_text("Load the newest scans of that day")
                    .clicked()
                {
                    app.state.mode = Mode::Archive;
                    let _ = app.tx.send(crate::app::DataCmd::LoadArchiveDate(
                        app.state.site_id.clone(),
                        app.state.archive_date,
                        None,
                    ));
                }
            });
            if ui
                .button("⟳ Loop last hour")
                .on_hover_text("Load the newest hour of scans and loop them")
                .clicked()
            {
                app.state.mode = Mode::Archive;
                let _ = app.tx.send(crate::app::DataCmd::LoadLastHour(app.state.site_id.clone()));
            }

            // Feedback.
            ui.add_space(6.0);
            ui.separator();
            ui.label(egui::RichText::new(&app.state.status_text).weak().small());
            if let (Some(first), Some(last)) = (app.state.ring.front(), app.state.ring.back()) {
                let tz = crate::timefmt::central();
                ui.label(format!(
                    "loaded {} scans · {} → {} (Central)",
                    app.state.ring.len(),
                    first.volume_start.with_timezone(tz).format("%H:%M"),
                    last.volume_start.with_timezone(tz).format("%H:%M"),
                ));
            }
            if app.state.archive_total > 0 {
                let window_len = app.state.archive_len.max(1);
                let windows = app.state.archive_total.div_ceil(window_len);
                let window_no = app.state.archive_start / window_len + 1;
                let first = app.state.archive_start + 1;
                let last = app.state.archive_start + app.state.archive_len;
                ui.horizontal(|ui| {
                    let at_oldest = app.state.archive_start == 0;
                    if ui
                        .add_enabled(at_oldest, egui::Button::new("◀ earlier"))
                        .clicked()
                    {
                        let _ = app.tx.send(crate::app::DataCmd::ArchiveWindowShift(-1));
                    }
                    if ui
                        .add_enabled(
                            last < app.state.archive_total,
                            egui::Button::new("later ▶"),
                        )
                        .clicked()
                    {
                        let _ = app.tx.send(crate::app::DataCmd::ArchiveWindowShift(1));
                    }
                });
                ui.label(
                    egui::RichText::new(format!(
                        "window {window_no} of {windows} · scans {first}–{last} of {} (newest first)",
                        app.state.archive_total
                    ))
                    .weak()
                    .small(),
                );
            }
        });
    app.state.archive_popup_open = open;
}

/// The 2D warning-polygon fill slider, in the map's Controls window (the 3D
/// pane has its own in 3D controls). 0% leaves the outlines and hides the wash
/// entirely.
pub(super) fn warning_fill_slider(ui: &mut egui::Ui, app: &mut StormApp) -> egui::Rect {
    let mut rect = egui::Rect::NOTHING;
    ui.horizontal(|ui| {
        ui.label("Warning polygon fill");
        let response = ui.add_sized(
            [120.0, 18.0],
            egui::Slider::new(&mut app.state.alerts_fill, 0.0..=1.0)
                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
        );
        rect = response.rect;
        response.on_hover_text(
            "Warning polygons are filled at rest and clear to the outline while \
             the pointer is on them, so the radar is visible where you inspect \
             it. This scales the resting fill (0% = outlines only, 100% = 95%). \
             Scroll over the slider to adjust it.",
        );
    });
    rect
}

/// One cache section: label, live size, and its "Clear …" button.
fn cache_row(
    ui: &mut egui::Ui,
    label: &str,
    hint: &str,
    bytes: Option<u64>,
    target: crate::app::ClearTarget,
    app: &mut StormApp,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.label(egui::RichText::new(hint).weak().small());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(bytes.map(human_bytes).unwrap_or_else(|| "…".into()))
                    .monospace(),
            );
        });
    });
    let (verb, hover) = match target {
        crate::app::ClearTarget::Radar => (
            "Clear radar data",
            "Delete every downloaded radar volume from disk",
        ),
        crate::app::ClearTarget::Tiles => (
            "Clear map data",
            "Delete every cached basemap tile from disk and memory",
        ),
    };
    if ui.button(verb).on_hover_text(hover).clicked() {
        app.confirm_clear = Some(target);
        ui.close(); // dismiss the menu so the dialog stands alone
    }
}

/// Modal "are you sure?" for the Settings clear buttons. Shows only while
/// `app.confirm_clear` is set; the accepted branch sends the matching
/// command (deletion runs off-thread in the coordinator) and, for map data,
/// also drops the UI's in-memory tile textures.
fn confirm_clear_dialog(ctx: &egui::Context, app: &mut StormApp) {
    let Some(target) = app.confirm_clear else {
        return;
    };
    let (title, body, verb, cmd) = match target {
        crate::app::ClearTarget::Radar => (
            "Clear radar data?",
            "Delete every downloaded radar volume from disk.\n\n\
             Scans already on screen stay until you reload or switch \
             station; future scans re-download on demand.",
            "Clear radar data",
            crate::app::DataCmd::ClearRadar,
        ),
        crate::app::ClearTarget::Tiles => (
            "Clear map data?",
            "Delete every cached basemap tile from disk and from memory.\n\n\
             The map re-downloads tiles as you pan and zoom.",
            "Clear map data",
            crate::app::DataCmd::ClearTiles,
        ),
    };

    let mut accepted = false;
    let mut dismissed = false;
    let res =
        egui::containers::Modal::new(egui::Id::new("stormscope.confirm-clear")).show(ctx, |ui| {
            ui.set_min_width(360.0);
            ui.heading(title);
            ui.add_space(8.0);
            ui.label(body);
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    dismissed = true;
                }
                if ui
                    .add(egui::Button::new(verb).fill(egui::Color32::from_rgb(150, 52, 52)))
                    .clicked()
                {
                    accepted = true;
                }
            });
        });
    if res.should_close() {
        // Backdrop click or Esc.
        dismissed = true;
    }

    if accepted {
        let _ = app.tx.send(cmd);
        if matches!(target, crate::app::ClearTarget::Tiles) {
            app.tiles.clear_in_memory();
        }
        dismissed = true;
    }
    if dismissed {
        app.confirm_clear = None;
    }
}

/// Scaled popup size, clamped so a popup always fits on screen with room to
/// spare: without the clamp a content-driven window can grow taller than the
/// display, which is how the outlook popup ended up hanging off the bottom.
/// Where a popup of `size` should be placed when it first opens: centered on
/// the screen, positioned by its top-left corner.
pub(super) fn centered_popup_pos(ctx: &egui::Context, size: egui::Vec2) -> egui::Pos2 {
    ctx.content_rect().center() - size * 0.5
}

pub(super) fn popup_max_height(ctx: &egui::Context, scale: f32) -> f32 {
    (ctx.content_rect().height() - 90.0 * scale).max(240.0)
}

pub(super) fn popup_size(ctx: &egui::Context, scale: f32, w: f32, h: f32) -> egui::Vec2 {
    let screen = ctx.content_rect().size();
    egui::vec2(
        (w * scale).min(screen.x - 40.0 * scale),
        (h * scale).min(screen.y - 90.0 * scale),
    )
}

/// Fill whatever vertical space a popup's content did not use, so the window
/// stays exactly as tall as what it draws.
///
/// This is what keeps border-dragging sane. egui lays a resizable window's
/// content out inside the size it was offered, but the resize drag takes its
/// base rect from the *window* rectangle while treating it as a content size:
/// if the content ends short of the window — a scroll area capped below the
/// space it was handed, a footer that does not use the rest — the two
/// disagree, and the first frame of a border drag yanks the border up by
/// exactly that gap, leaving the pointer grabbing thin air. Measured with a
/// probe driving synthetic input: a 50 px shortfall produced a 50 px jump, and
/// padding the leftover removes it (the border then tracks the pointer 1:1).
pub(super) fn fill_popup_height(ui: &mut egui::Ui) {
    let rest = ui.available_height();
    if rest > 0.0 {
        ui.add_space(rest);
    }
}

/// SPC forecaster discussion popup, opened by clicking an outlook outline on
/// the map (or its category in the legend): the clicked risk level's meaning
/// plus the full text of that day's outlook.
fn outlook_detail_window(ctx: &egui::Context, app: &mut StormApp) {
    if !app.state.outlook_detail_open {
        return;
    }
    let day = app.state.outlook_day;
    // The popup follows the selected day: fetch its text if we do not have it.
    if app.state.outlook_discussion.as_ref().map(|(d, _)| *d) != Some(day)
        && app.state.outlook_discussion_requested != Some(day)
    {
        app.state.outlook_discussion_requested = Some(day);
        let _ = app.tx.send(crate::app::DataCmd::LoadOutlookDiscussion(day));
    }

    let scale = app.sidebar_scale.max(1.0);
    let size = popup_size(ctx, scale, 640.0, 560.0);
    let max_h = popup_max_height(ctx, scale);
    let mut open = true;
    egui::Window::new(format!("SPC Day {day} outlook"))
        .id(egui::Id::new("stormscope.outlook.detail"))
        .open(&mut open)
        .default_size(size)
        .max_height(max_h)
        .min_size(egui::vec2(460.0 * scale, 220.0 * scale))
        // Centered *by its top-left corner*, not by a pivot: egui's pivot
        // is for anchored areas, and with a center pivot every size change
        // also shifts the stored position — which fights the resize drag.
        .default_pos(centered_popup_pos(ctx, size))
        .resizable(true)
        .show(ctx, |ui| {
            // The risk level that was clicked, as a summary card.
            if let (Some(dn), Some(o)) = (app.state.outlook_detail_dn, app.state.outlook.as_ref()) {
                ui.label(
                    egui::RichText::new("What this level means")
                        .strong()
                        .small(),
                );
                egui::Frame::group(ui.style())
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        if let Some(f) = o.features.iter().find(|f| f.dn == dn) {
                            ui.horizontal_wrapped(|ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(12.0 * scale, 12.0 * scale),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(
                                    rect,
                                    2.0,
                                    egui::Color32::from_rgb(f.stroke[0], f.stroke[1], f.stroke[2]),
                                );
                                ui.heading(f.headline());
                            });
                            ui.label(egui::RichText::new(f.description()));
                        }
                        let mut meta = vec![format!("SPC Day {day} Convective Outlook")];
                        if !o.forecaster.is_empty() {
                            meta.push(o.forecaster.clone());
                        }
                        if let Some(issue) = o.issue {
                            meta.push(format!("issued {}", crate::timefmt::fmt_central(&issue)));
                        }
                        if let Some(expire) = o.expire {
                            meta.push(format!(
                                "valid until {}",
                                crate::timefmt::fmt_central(&expire)
                            ));
                        }
                        ui.label(egui::RichText::new(meta.join(" · ")).weak().small());
                    });
            }

            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Forecaster discussion")
                    .strong()
                    .small(),
            );
            // Height budget: the window keeps the size it opened with instead
            // of growing to fit the whole text.
            // Grows with the window: dragging the popup taller reveals more
            // of the discussion instead of leaving dead space.
            let text_height = (ui.available_height() - 60.0 * scale).max(140.0);

            match app.state.outlook_discussion.as_ref() {
                Some((d, text)) if *d == day => {
                    egui::Frame::NONE
                        .fill(ui.visuals().extreme_bg_color)
                        .corner_radius(4.0)
                        .inner_margin(egui::Margin::same(8))
                        .show(ui, |ui| {
                            // The SPC product is pre-formatted for a fixed
                            // column width, so it is *not* re-wrapped: a
                            // wrapped galley of the whole discussion would be
                            // re-laid-out on every resize frame (visible
                            // jitter while dragging the window edge) and the
                            // widest lines would shift as the window changes.
                            // Both scrollbars are always reserved so growing
                            // or shrinking the window cannot make a bar
                            // appear and disappear, which oscillates the
                            // layout.
                            egui::ScrollArea::both()
                                .max_height(text_height)
                                .auto_shrink([false, false])
                                .scroll_bar_visibility(
                                    egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                                )
                                .show(ui, |ui| {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(text)
                                                .monospace()
                                                .size(12.0 * scale),
                                        )
                                        .wrap_mode(egui::TextWrapMode::Extend),
                                    );
                                });
                        });
                }
                _ => {
                    ui.allocate_ui(egui::vec2(ui.available_width(), text_height), |ui| {
                        ui.label(egui::RichText::new("Loading the forecaster discussion…").weak());
                    });
                }
            }
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if ui
                    .button("Reload text")
                    .on_hover_text("Refetch the discussion from spc.noaa.gov")
                    .clicked()
                {
                    app.state.outlook_discussion = None;
                    app.state.outlook_discussion_requested = Some(day);
                    let _ = app.tx.send(crate::app::DataCmd::LoadOutlookDiscussion(day));
                }
                ui.label(
                    egui::RichText::new("source: spc.noaa.gov (public domain)")
                        .weak()
                        .small(),
                );
            });
            fill_popup_height(ui);
        });

    if !open {
        app.state.outlook_detail_open = false;
    }
}

/// Human-readable byte size (B/KiB/MiB/GiB) for the Settings menu.
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn middle() -> egui::Rect {
        // Screen-like coords: status bar occupies y 0..30, timeline the
        // bottom 60 px of an 800 px window.
        egui::Rect::from_min_size(egui::pos2(0.0, 30.0), egui::vec2(1280.0, 710.0))
    }

    fn at(x: f32, y: f32) -> Option<egui::Pos2> {
        Some(egui::pos2(x, y))
    }

    fn open(
        pinned: bool,
        was_open: bool,
        pressing: bool,
        pointer: Option<egui::Pos2>,
        middle: egui::Rect,
    ) -> bool {
        sidebar_want_open(pinned, was_open, pressing, pointer, middle, SIDEBAR_WIDTH)
    }

    #[test]
    fn sidebar_opens_on_left_edge_hover() {
        assert!(open(false, false, false, at(5.0, 400.0), middle()));
    }

    #[test]
    fn sidebar_stays_shut_away_from_edge() {
        assert!(!open(false, false, false, at(500.0, 400.0), middle()));
    }

    #[test]
    fn sidebar_suppressed_mid_gesture() {
        // A map pan ending at the left edge must not pop the sidebar.
        assert!(!open(false, false, true, at(5.0, 400.0), middle()));
    }

    #[test]
    fn sidebar_stays_open_inside_wide_zone() {
        // Pointer over the open sidebar (or its popups/sliders) keeps it up.
        assert!(open(false, true, false, at(200.0, 400.0), middle()));
        // Held buttons (e.g. dragging a slider) don't close it either.
        assert!(open(false, true, true, at(200.0, 400.0), middle()));
    }

    #[test]
    fn sidebar_wide_zone_follows_scaled_panel() {
        // A 2x-scaled panel keeps the pointer at x=400 inside its zone.
        assert!(sidebar_want_open(
            false,
            true,
            false,
            at(400.0, 400.0),
            middle(),
            SIDEBAR_WIDTH * 2.0,
        ));
        assert!(!open(false, true, false, at(400.0, 400.0), middle()));
    }

    #[test]
    fn sidebar_closes_when_pointer_leaves_for_map() {
        assert!(!open(false, true, false, at(600.0, 400.0), middle()));
    }

    #[test]
    fn sidebar_ignores_pointer_over_bars_or_off_window() {
        // Over the status bar / timeline: not the sidebar's business.
        assert!(!open(false, false, false, at(5.0, 5.0), middle()));
        assert!(!open(false, true, false, at(5.0, 780.0), middle()));
        // Pointer gone (touch / left window): shut unless pinned.
        assert!(!open(false, true, false, None, middle()));
        assert!(open(true, false, false, None, middle()));
    }

    #[test]
    fn sidebar_pinned_stays_open() {
        assert!(open(true, false, false, at(900.0, 400.0), middle()));
    }

    #[test]
    fn hidpi_scale_tops_up_os_scaling_to_1080p_equivalent() {
        // Plain 1080p: no change.
        assert_eq!(hidpi_text_scale(Some(1080.0), Some(1.0), 1.0, 800.0), 1.0);
        // 4K, no OS scaling: doubled boost, capped at 2.5x.
        assert_eq!(hidpi_text_scale(Some(2160.0), Some(1.0), 1.0, 800.0), 2.5);
        // 4K monitor at 150% scale (1440pt logical) → ~1.67x extra.
        let s = hidpi_text_scale(Some(1440.0), Some(1.5), 1.5, 700.0);
        assert!(
            (s - (1.0 + (2.0 / 1.5 - 1.0) * 2.0)).abs() < 1e-6,
            "got {s}"
        );
        // 4K at 200% OS scale: the OS already doubled everything.
        assert_eq!(hidpi_text_scale(Some(1080.0), Some(2.0), 2.0, 700.0), 1.0);
        // 1440p monitor, no OS scaling: ~1.67x.
        let s = hidpi_text_scale(Some(1440.0), Some(1.0), 1.0, 800.0);
        assert!(
            (s - (1.0 + (1440.0 / 1080.0 - 1.0) * 2.0)).abs() < 1e-6,
            "got {s}"
        );
        // Beyond the cap: clamps at 2.5x.
        assert_eq!(hidpi_text_scale(Some(4320.0), Some(1.0), 1.0, 800.0), 2.5);
        // Unknown monitor: falls back to window physical height.
        assert_eq!(hidpi_text_scale(None, None, 1.0, 500.0), 1.0);
        let s = hidpi_text_scale(None, None, 1.0, 1500.0);
        assert!(
            (s - (1.0 + (1500.0 / 1080.0 - 1.0) * 2.0)).abs() < 1e-6,
            "got {s}"
        );
    }

    /// The default echo floor has to serve both seasons: low enough that light
    /// snow and drizzle (10-25 dBZ) actually draw, high enough that a
    /// clear-air scan is not a field of speckle. Snow is the stricter case —
    /// a 30 dBZ floor hid all but the heaviest winter cores — so the default
    /// sits at 10 and the map Controls slider is the escape hatch.
    #[test]
    fn default_min_dbz_shows_snow_but_not_zero() {
        let floor = crate::state::AppState::default().min_dbz;
        assert!(floor <= 15.0, "snow lives at 10-25 dBZ, floor is {floor}");
        assert!(floor > 0.0, "a zero floor is clear-air noise");
    }

    #[test]
    fn sidebar_panel_height_fills_middle_rect() {
        let h = sidebar_panel_height(middle());
        assert!(
            (h - (710.0 - 16.0)).abs() < 1e-6,
            "should span the middle rect, got {h}"
        );
    }

    #[test]
    fn sidebar_panel_height_floored_on_short_windows() {
        let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 30.0), egui::vec2(1280.0, 50.0));
        assert_eq!(sidebar_panel_height(tiny), 120.0);
    }

    /// Dragging a popup's bottom border must not move it: egui derives the
    /// drag base from the window rectangle, so content that ends short of the
    /// window makes the first drag frame yank the border up by the shortfall
    /// (the "border jumps up and the mouse is left behind" bug). Drive a real
    /// popup — built like the shipped ones, scroll area and all — with
    /// synthetic pointer input and check the border follows the pointer.
    #[test]
    fn popup_border_drag_stays_glued_to_the_pointer() {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0));
        let long: String = (0..40)
            .map(|i| format!("line {i}: the quick brown fox jumps over the lazy dog\n"))
            .collect();

        let mut time = 0.0f64;
        let mut frame = |events: Vec<egui::Event>| -> egui::Rect {
            time += 1.0 / 60.0;
            let input = egui::RawInput {
                screen_rect: Some(screen),
                time: Some(time),
                events,
                ..Default::default()
            };
            let mut rect = egui::Rect::ZERO;
            let out = ctx.run_ui(input, |ui| {
                let win = egui::Window::new("Popup drag probe")
                    .id(egui::Id::new("stormscope.test.popup_drag"))
                    .default_size(popup_size(ui.ctx(), 1.0, 620.0, 500.0))
                    .max_height(popup_max_height(ui.ctx(), 1.0))
                    .min_size(egui::vec2(420.0, 220.0))
                    .default_pos(egui::pos2(200.0, 100.0))
                    .resizable(true)
                    .show(ui.ctx(), |ui| {
                        ui.heading("NOAA alert details");
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .max_height((ui.available_height() - 50.0).max(120.0))
                            .show(ui, |ui| {
                                ui.label(&long);
                            });
                        ui.horizontal(|ui| {
                            let _ = ui.button("Reload text");
                            ui.label("source: spc.noaa.gov");
                        });
                        fill_popup_height(ui);
                    });
                rect = win.expect("window shown").response.rect;
            });
            out.drop_without_applying_deltas();
            rect
        };

        let mut rect = egui::Rect::ZERO;
        for _ in 0..5 {
            rect = frame(vec![]);
        }
        let steady = rect;
        let bottom = egui::pos2(steady.center().x, steady.max.y - 1.0);
        frame(vec![egui::Event::PointerMoved(bottom)]);
        frame(vec![egui::Event::PointerButton {
            pos: bottom,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        }]);
        // The grab frame itself: egui may re-run the pass while a drag starts.
        let mut grab = steady;
        for _ in 0..2 {
            frame(vec![egui::Event::PointerMoved(bottom)]);
            grab = frame(vec![]);
        }
        assert!(
            (grab.height() - steady.height()).abs() <= 2.0,
            "grabbing the bottom border moved it: {} -> {}",
            steady.height(),
            grab.height()
        );
        // And now the drag proper: 100 px of pointer must be 100 px of border.
        let target = bottom + egui::vec2(0.0, 100.0);
        let mut grown = grab;
        for _ in 0..2 {
            frame(vec![egui::Event::PointerMoved(target)]);
            grown = frame(vec![]);
        }
        assert!(
            (grown.height() - grab.height() - 100.0).abs() <= 2.0,
            "border did not follow the pointer: {} -> {} for a 100 px drag",
            grab.height(),
            grown.height()
        );
    }
}
