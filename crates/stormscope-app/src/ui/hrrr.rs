//! HRRR point forecast: hourly model profiles and a time-height section.
//!
//! Data comes through Open-Meteo's `gfs_hrrr` pressure-level endpoint, so the
//! request stays small enough for phones: one JSON document, no GRIB grids.

use crate::app::StormApp;

pub fn hrrr_window(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    if !app.state.hrrr_open {
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(250));

    let target = radar_core::sites::site(&app.state.site_id).map(|s| (s.latitude, s.longitude));
    if let Some((lat, lon)) = target {
        let loaded_matches =
            app.state.hrrr.as_ref().is_some_and(|s| {
                (s.latitude - lat).abs() < 0.001 && (s.longitude - lon).abs() < 0.001
            });
        let need = app.state.hrrr_requested != Some((lat, lon))
            && !loaded_matches
            && app.state.hrrr_error.is_none();
        if need {
            app.state.hrrr_requested = Some((lat, lon));
            app.state.hrrr_error = None;
            let _ = app.tx.send(crate::app::DataCmd::LoadHrrr(lat, lon));
        }
    }

    let fill = cfg!(target_os = "android");
    let scale = super::big_text_scale(app);
    let screen = ctx.content_rect().size();
    let size = super::popup_size(ctx, scale, 860.0, 720.0);
    let max_h = super::popup_max_height(ctx, scale);
    let mut open = app.state.hrrr_open;
    let mut refresh = false;
    let mut window = egui::Window::new("HRRR forecast")
        .id(egui::Id::new("stormscope.hrrr"))
        .default_pos(super::centered_popup_pos(ctx, size))
        .max_width((screen.x - 16.0).max(200.0))
        .min_size(egui::vec2(560.0 * scale, 320.0 * scale));
    #[cfg(target_os = "android")]
    if fill {
        window = window.title_bar(false);
    }
    window = if fill {
        let inset = area.shrink2(egui::vec2(4.0, 8.0));
        window
            .fixed_rect(inset)
            .movable(false)
            .resizable(false)
            .constrain_to(inset)
    } else {
        window.default_size(size).max_height(max_h).resizable(true)
    };
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut close = false;
    window.open(&mut open).show(ctx, |ui| {
        #[cfg(target_os = "android")]
        if fill {
            close |= super::phone_header(ui, "HRRR forecast");
        }
        super::apply_text_scale(ui, scale);
        ui.horizontal_wrapped(|ui| {
            if let Some((lat, lon)) = target {
                ui.label(
                    egui::RichText::new(format!("HRRR · site {lat:.3}, {lon:.3}"))
                        .weak()
                        .small(),
                );
            }
            if ui.small_button("Refresh").clicked() {
                app.state.hrrr_requested = None;
                app.state.hrrr_error = None;
                app.state.hrrr = None;
                refresh = true;
            }
        });
        if app.state.hrrr_requested.is_some()
            && app.state.hrrr.is_none()
            && app.state.hrrr_error.is_none()
        {
            ui.label(egui::RichText::new("Loading HRRR profiles…").weak());
        }
        if let Some(err) = &app.state.hrrr_error {
            ui.label(egui::RichText::new(format!("HRRR failed: {err}")).weak());
        }
        if let Some(series) = app.state.hrrr.clone() {
            let n = series.profiles.len();
            if n == 0 {
                return;
            }
            let idx = app.state.hrrr_hour.min(n - 1);
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("Time-height").strong());
                for (v, label) in [(0u8, "Temperature"), (1, "Humidity"), (2, "Wind")] {
                    if ui
                        .selectable_label(app.state.hrrr_field == v, label)
                        .clicked()
                    {
                        app.state.hrrr_field = v;
                    }
                }
            });
            time_height(
                ui,
                app,
                &series,
                egui::vec2(ui.available_width().max(380.0), 300.0 * scale),
            );
            ui.add_space(4.0);
            let mut hour = idx;
            ui.add(
                egui::Slider::new(&mut hour, 0..=n - 1)
                    .text("Forecast hour")
                    .custom_formatter(|v, _| {
                        series
                            .profiles
                            .get(v as usize)
                            .map(|p| p.valid_time.format("%H:%MZ").to_string())
                            .unwrap_or_default()
                    }),
            );
            app.state.hrrr_hour = hour;
            let profile = &series.profiles[idx];
            ui.label(
                egui::RichText::new(format!(
                    "{} · HRRR model profile (not observed)",
                    crate::timefmt::fmt_central(&profile.valid_time)
                ))
                .weak()
                .small(),
            );
            egui::CollapsingHeader::new("Selected profile")
                .default_open(false)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height((max_h - 380.0 * scale).max(160.0))
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if ui.available_width() < 620.0 * scale {
                                super::sounding::skewt(
                                    ui,
                                    profile,
                                    egui::vec2(ui.available_width().max(240.0), 280.0 * scale),
                                );
                                ui.add_space(6.0);
                                super::sounding::hodograph(
                                    ui,
                                    profile,
                                    egui::vec2(220.0 * scale, 220.0 * scale),
                                );
                                ui.add_space(6.0);
                                super::sounding::params(ui, profile);
                            } else {
                                ui.columns(2, |cols| {
                                    cols[0].vertical(|ui| {
                                        super::sounding::skewt(
                                            ui,
                                            profile,
                                            egui::vec2(300.0 * scale, 300.0 * scale),
                                        );
                                        ui.add_space(6.0);
                                        super::sounding::hodograph(
                                            ui,
                                            profile,
                                            egui::vec2(220.0 * scale, 220.0 * scale),
                                        );
                                    });
                                    cols[1].vertical(|ui| {
                                        super::sounding::params(ui, profile);
                                    });
                                });
                            }
                        });
                });
        }
    });
    if close {
        open = false;
    }
    if refresh {
        if let Some((lat, lon)) = target {
            let _ = app.tx.send(crate::app::DataCmd::LoadHrrr(lat, lon));
            app.state.hrrr_requested = Some((lat, lon));
        }
    }
    app.state.hrrr_open = open;
}

fn time_height(
    ui: &mut egui::Ui,
    app: &StormApp,
    series: &radar_data::SoundingSeries,
    size: egui::Vec2,
) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, egui::Color32::from_gray(10));
    let left = rect.left() + 42.0;
    let right = rect.right() - 8.0;
    let top = rect.top() + 8.0;
    let bottom = rect.bottom() - 22.0;
    let n = series.profiles.len().max(1);
    let dx = (right - left) / n as f32;
    let pmin = 100.0f64;
    let pmax = 1000.0f64;
    let py = |pressure: f64| -> f32 {
        let lp = (pressure.ln() - pmin.ln()) / (pmax.ln() - pmin.ln());
        top + lp as f32 * (bottom - top)
    };
    // Pressure grid labels.
    for &pr in &[1000.0, 850.0, 700.0, 500.0, 300.0, 200.0, 100.0] {
        let y = py(pr).clamp(top + 6.0, bottom - 6.0);
        p.line_segment(
            [egui::pos2(left, y), egui::pos2(right, y)],
            egui::Stroke::new(0.5, egui::Color32::from_gray(35)),
        );
        p.text(
            egui::pos2(rect.left() + 2.0, y),
            egui::Align2::LEFT_CENTER,
            format!("{pr:.0}"),
            egui::FontId::monospace(9.0),
            egui::Color32::from_gray(120),
        );
    }
    for (i, profile) in series.profiles.iter().enumerate() {
        let x0 = left + i as f32 * dx;
        let x1 = x0 + dx.max(1.0);
        for pair in profile.levels.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            let y0 = py(a.pressure_hpa);
            let y1 = py(b.pressure_hpa);
            if (y1 - y0).abs() < 0.5 {
                continue;
            }
            let value = match app.state.hrrr_field {
                0 => a.temp_c as f32,
                1 => a.rh_pct as f32,
                _ => a.wind_speed_ms as f32,
            };
            let color = time_height_color(app.state.hrrr_field, value);
            p.rect_filled(
                egui::Rect::from_min_max(egui::pos2(x0, y0.min(y1)), egui::pos2(x1, y0.max(y1))),
                0.0,
                color,
            );
        }
        if i % 6 == 0 {
            p.text(
                egui::pos2(x0 + dx * 0.5, bottom + 3.0),
                egui::Align2::CENTER_TOP,
                profile.valid_time.format("%HZ").to_string(),
                egui::FontId::monospace(9.0),
                egui::Color32::from_gray(120),
            );
        }
    }
    let sel = rect.left() + 42.0 + (app.state.hrrr_hour as f32 + 0.5) * dx;
    p.line_segment(
        [egui::pos2(sel, top), egui::pos2(sel, bottom)],
        egui::Stroke::new(1.5, egui::Color32::WHITE),
    );
}

fn time_height_color(field: u8, value: f32) -> egui::Color32 {
    let (lo, hi, cold, warm) = match field {
        0 => (
            -40.0,
            30.0,
            egui::Color32::from_rgb(50, 90, 220),
            egui::Color32::from_rgb(230, 60, 40),
        ),
        1 => (
            0.0,
            100.0,
            egui::Color32::from_rgb(90, 60, 30),
            egui::Color32::from_rgb(60, 200, 110),
        ),
        _ => (
            0.0,
            50.0,
            egui::Color32::from_rgb(40, 70, 160),
            egui::Color32::from_rgb(240, 60, 50),
        ),
    };
    let t = ((value - lo) / (hi - lo)).clamp(0.0, 1.0);
    egui::Color32::from_rgba_unmultiplied(
        (cold.r() as f32 + (warm.r() as f32 - cold.r() as f32) * t) as u8,
        (cold.g() as f32 + (warm.g() as f32 - cold.g() as f32) * t) as u8,
        (cold.b() as f32 + (warm.b() as f32 - cold.b() as f32) * t) as u8,
        220,
    )
}
