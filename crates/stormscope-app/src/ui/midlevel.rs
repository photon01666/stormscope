//! HRRR 500 mb mid-level wind overlay controls and animation.

use crate::app::StormApp;

pub fn midlevel_window(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    if !app.state.midlevel_open {
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(100));

    if app.state.midlevel.is_none()
        && !app.state.midlevel_pending
        && app.state.midlevel_error.is_none()
    {
        app.state.midlevel_pending = true;
        app.state.midlevel_requested_at = Some(std::time::Instant::now());
        let _ = app.tx.send(crate::app::DataCmd::LoadMidLevel);
    }

    let fill = cfg!(target_os = "android");
    let scale = super::big_text_scale(app);
    let screen = ctx.content_rect().size();
    let pos = super::centered_popup_pos(ctx, egui::vec2(520.0, 250.0));
    let mut open = app.state.midlevel_open;
    let mut refresh = false;
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut window = egui::Window::new("Mid-level wind (500 mb)")
        .id(egui::Id::new("stormscope.midlevel"))
        .default_pos(pos)
        .max_width((screen.x - 16.0).max(200.0))
        .auto_sized()
        .resizable(false);
    #[cfg(target_os = "android")]
    if fill {
        window = window.title_bar(false);
    }
    let window = if fill {
        let inset = area.shrink2(egui::vec2(4.0, 8.0));
        window
            .fixed_rect(inset)
            .movable(false)
            .resizable(false)
            .constrain_to(inset)
    } else {
        window
    };
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut close = false;
    window.open(&mut open).show(ctx, |ui| {
        #[cfg(target_os = "android")]
        if fill {
            close |= super::phone_header(ui, "Mid-level wind");
        }
        super::apply_text_scale(ui, scale);
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut app.state.show_midlevel, "Show on map");
            if ui
                .selectable_label(app.state.midlevel_playing, {
                    if app.state.midlevel_playing {
                        "⏸ Pause"
                    } else {
                        "▶ Play"
                    }
                })
                .clicked()
            {
                app.state.midlevel_playing = !app.state.midlevel_playing;
                app.state.midlevel_last_step = ui.input(|i| i.time);
            }
            if ui.small_button("Refresh").clicked() {
                refresh = true;
            }
            ui.label(
                egui::RichText::new("HRRR · 500 mb · 2-hour steps")
                    .weak()
                    .small(),
            );
        });
        if app.state.midlevel_pending && app.state.midlevel.is_none() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Downloading HRRR 500 mb wind and height fields…");
                ui.label(egui::RichText::new("~20–30 s for 10 frames").weak().small());
            });
        }
        if let Some(err) = &app.state.midlevel_error {
            ui.label(egui::RichText::new(format!("Mid-level wind failed: {err}")).weak());
        }
        if let Some(run) = app.state.midlevel.clone() {
            if run.frames.is_empty() {
                ui.label("No forecast frames loaded.");
            } else {
                if app.state.midlevel_playing {
                    let now = ui.input(|i| i.time);
                    if now - app.state.midlevel_last_step >= 0.8 {
                        app.state.midlevel_frame =
                            (app.state.midlevel_frame + 1) % run.frames.len();
                        app.state.midlevel_last_step = now;
                    }
                    ui.ctx().request_repaint();
                }
                let n = run.frames.len();
                let idx = app.state.midlevel_frame.min(n - 1);
                let frame = &run.frames[idx];
                ui.label(
                    egui::RichText::new(format!(
                        "Init {} · f{:02} valid {} · {} kt max",
                        run.init.format("%H:%MZ"),
                        frame.hour,
                        crate::timefmt::fmt_central(&frame.valid),
                        frame
                            .speed_kt
                            .iter()
                            .filter(|v| v.is_finite())
                            .fold(0.0f32, |a, b| a.max(*b))
                            .round(),
                    ))
                    .weak()
                    .small(),
                );
                let mut frame_idx = idx;
                ui.add(
                    egui::Slider::new(&mut frame_idx, 0..=n - 1)
                        .text("Forecast frame")
                        .custom_formatter(|v, _| {
                            run.frames
                                .get(v as usize)
                                .map(|f| format!("f{:02} · {}", f.hour, f.valid.format("%H:%MZ")))
                                .unwrap_or_default()
                        }),
                );
                if frame_idx != idx {
                    app.state.midlevel_frame = frame_idx;
                    app.state.midlevel_playing = false;
                }
                let mut opacity = app.state.midlevel_opacity;
                ui.add(egui::Slider::new(&mut opacity, 0.05..=1.0).text("Wind speed opacity"));
                app.state.midlevel_opacity = opacity;
                speed_legend(ui);
                ui.label(
                    egui::RichText::new(
                        "Color = 500 mb wind speed (kt). Contours = 500 mb height (dam). \
                         Barbs = wind direction/speed.",
                    )
                    .weak()
                    .small(),
                );
            }
        }
    });
    if close {
        open = false;
    }
    if refresh {
        app.state.midlevel = None;
        app.state.midlevel_error = None;
        app.state.midlevel_pending = true;
        app.state.midlevel_requested_at = Some(std::time::Instant::now());
        let _ = app.tx.send(crate::app::DataCmd::LoadMidLevel);
    }
    app.state.midlevel_open = open;
}

fn speed_legend(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().min(360.0), 16.0),
        egui::Sense::hover(),
    );
    let p = ui.painter_at(rect);
    let n = 120;
    for i in 0..n {
        let t = i as f32 / (n - 1) as f32;
        let kt = 10.0 + t * 90.0;
        let color = radar_data::midlevel::wind_rgba(kt);
        let x0 = rect.left() + t * rect.width();
        let x1 = rect.left() + (t + 1.0 / (n - 1) as f32) * rect.width();
        p.rect_filled(
            egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom())),
            0.0,
            egui::Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]),
        );
    }
    p.text(
        egui::pos2(rect.left(), rect.bottom() + 1.0),
        egui::Align2::LEFT_TOP,
        "10 kt",
        egui::FontId::monospace(9.0),
        egui::Color32::from_gray(150),
    );
    p.text(
        egui::pos2(rect.right(), rect.bottom() + 1.0),
        egui::Align2::RIGHT_TOP,
        "100 kt",
        egui::FontId::monospace(9.0),
        egui::Color32::from_gray(150),
    );
    ui.add_space(12.0);
}
