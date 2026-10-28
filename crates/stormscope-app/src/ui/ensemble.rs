//! GEFS point plume: every member, mean and the 10–90% range.

use crate::app::StormApp;

pub fn ensemble_window(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    if !app.state.ensemble_open {
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(250));

    let Some(site) = radar_core::sites::site(&app.state.site_id) else {
        return;
    };
    let field = app.state.ensemble_field;
    if app.state.ensemble_requested != Some((site.latitude, site.longitude, field))
        && app.state.ensemble_error.is_none()
    {
        app.state.ensemble_requested = Some((site.latitude, site.longitude, field));
        app.state.ensemble_pending = true;
        let _ = app.tx.send(crate::app::DataCmd::LoadEnsemble {
            field,
            lat: site.latitude,
            lon: site.longitude,
        });
    }

    let fill = cfg!(target_os = "android");
    let scale = super::big_text_scale(app);
    let screen = ctx.content_rect().size();
    let size = super::popup_size(ctx, scale, 860.0, 520.0);
    let max_h = super::popup_max_height(ctx, scale);
    let mut open = app.state.ensemble_open;
    let mut refresh = false;
    let mut window = egui::Window::new("Ensemble plume")
        .id(egui::Id::new("stormscope.ensemble"))
        .default_pos(super::centered_popup_pos(ctx, size))
        .max_width((screen.x - 16.0).max(200.0))
        .min_size(egui::vec2(520.0 * scale, 280.0 * scale));
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
            close |= super::phone_header(ui, "Ensemble plume");
        }
        super::apply_text_scale(ui, scale);
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("GEFS · global ensemble").strong());
            let mut new_field = app.state.ensemble_field;
            egui::ComboBox::from_id_salt("ensemble-field")
                .selected_text(new_field.label())
                .show_ui(ui, |ui| {
                    for field in [
                        radar_data::EnsembleField::Temperature,
                        radar_data::EnsembleField::Wind,
                        radar_data::EnsembleField::Precipitation,
                        radar_data::EnsembleField::Cape,
                    ] {
                        ui.selectable_value(&mut new_field, field, field.label());
                    }
                });
            if new_field != app.state.ensemble_field {
                app.state.ensemble_field = new_field;
                app.state.ensemble = None;
                app.state.ensemble_requested = None;
                app.state.ensemble_error = None;
                app.state.ensemble_pending = true;
            }
            if ui.small_button("Refresh").clicked() {
                refresh = true;
            }
            ui.label(
                egui::RichText::new(format!(
                    "{} · {:.2},{:.2}",
                    site.name, site.latitude, site.longitude
                ))
                .weak()
                .small(),
            );
        });
        if app.state.ensemble_pending && app.state.ensemble.is_none() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Downloading GEFS members…");
            });
        }
        if let Some(err) = &app.state.ensemble_error {
            ui.label(egui::RichText::new(format!("Ensemble failed: {err}")).weak());
        }
        if let Some(series) = app.state.ensemble.clone() {
            if series.times.is_empty() || series.members.is_empty() {
                ui.label("The ensemble returned no usable members.");
                if !fill {
                    super::fill_popup_height(ui);
                }
                return;
            }
            let now_idx = nearest_time(&series.times);
            let (mean, p10, p90) = (
                series.mean.get(now_idx).copied().unwrap_or(f64::NAN),
                series.p10.get(now_idx).copied().unwrap_or(f64::NAN),
                series.p90.get(now_idx).copied().unwrap_or(f64::NAN),
            );
            ui.label(
                egui::RichText::new(format!(
                    "{} · {} members · nearest hour {} · mean {:.1} {} · 10–90% {:.1}–{:.1} {}",
                    series.field.label(),
                    series.members.len(),
                    crate::timefmt::fmt_central(&series.times[now_idx]),
                    mean,
                    series.field.unit(),
                    p10,
                    p90,
                    series.field.unit(),
                ))
                .weak()
                .small(),
            );
            ui.add_space(4.0);
            plume_chart(
                ui,
                &series,
                egui::vec2(ui.available_width().max(380.0), 300.0 * scale),
            );
            if !fill {
                super::fill_popup_height(ui);
            }
        }
    });
    if close {
        open = false;
    }
    if refresh {
        app.state.ensemble = None;
        app.state.ensemble_requested = None;
        app.state.ensemble_error = None;
        app.state.ensemble_pending = true;
        let _ = app.tx.send(crate::app::DataCmd::LoadEnsemble {
            field: app.state.ensemble_field,
            lat: site.latitude,
            lon: site.longitude,
        });
    }
    app.state.ensemble_open = open;
}

fn nearest_time(times: &[chrono::DateTime<chrono::Utc>]) -> usize {
    let now = chrono::Utc::now();
    times
        .iter()
        .enumerate()
        .min_by_key(|(_, t)| (**t - now).num_seconds().abs())
        .map(|(i, _)| i)
        .unwrap_or(0)
}

fn plume_chart(ui: &mut egui::Ui, series: &radar_data::EnsembleSeries, size: egui::Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, egui::Color32::from_gray(10));
    let left = rect.left() + 48.0;
    let right = rect.right() - 8.0;
    let top = rect.top() + 14.0;
    let bottom = rect.bottom() - 24.0;
    if right <= left || bottom <= top {
        return;
    }
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for row in &series.members {
        for v in row.iter().filter(|v| v.is_finite()) {
            lo = lo.min(*v);
            hi = hi.max(*v);
        }
    }
    if !lo.is_finite() || !hi.is_finite() {
        return;
    }
    let pad = (hi - lo).abs().max(1.0) * 0.08;
    lo -= pad;
    hi += pad;
    for i in 0..=4 {
        let t = i as f32 / 4.0;
        let y = top + t * (bottom - top);
        p.line_segment(
            [egui::pos2(left, y), egui::pos2(right, y)],
            egui::Stroke::new(0.5, egui::Color32::from_gray(35)),
        );
        let v = hi - f64::from(t) * (hi - lo);
        p.text(
            egui::pos2(rect.left() + 2.0, y),
            egui::Align2::LEFT_CENTER,
            format!("{v:.1}"),
            egui::FontId::monospace(9.0),
            egui::Color32::from_gray(120),
        );
    }
    p.text(
        egui::pos2(rect.left() + 2.0, rect.top() + 2.0),
        egui::Align2::LEFT_TOP,
        series.field.unit(),
        egui::FontId::monospace(9.0),
        egui::Color32::from_gray(120),
    );
    let n = series.times.len();
    let dx = if n > 1 {
        (right - left) / (n - 1) as f32
    } else {
        0.0
    };
    let y = |v: f64| bottom - ((v - lo) / (hi - lo).max(f64::EPSILON)) as f32 * (bottom - top);
    let band = series
        .p10
        .iter()
        .zip(&series.p90)
        .enumerate()
        .filter(|(_, (a, b))| a.is_finite() && b.is_finite())
        .map(|(i, (a, b))| {
            (
                egui::pos2(left + i as f32 * dx, y(*b)),
                egui::pos2(left + i as f32 * dx, y(*a)),
            )
        })
        .collect::<Vec<_>>();
    for (a, b) in &band {
        p.line_segment(
            [*a, *b],
            egui::Stroke::new(1.0, egui::Color32::from_white_alpha(40)),
        );
    }
    for row in &series.members {
        let pts = row
            .iter()
            .enumerate()
            .filter(|(_, v)| v.is_finite())
            .map(|(i, v)| egui::pos2(left + i as f32 * dx, y(*v)))
            .collect::<Vec<_>>();
        if pts.len() > 1 {
            p.add(egui::Shape::line(
                pts,
                egui::Stroke::new(
                    0.8,
                    egui::Color32::from_rgba_unmultiplied(120, 190, 255, 70),
                ),
            ));
        }
    }
    let mean = series
        .mean
        .iter()
        .enumerate()
        .filter(|(_, v)| v.is_finite())
        .map(|(i, v)| egui::pos2(left + i as f32 * dx, y(*v)))
        .collect::<Vec<_>>();
    if mean.len() > 1 {
        p.add(egui::Shape::line(
            mean,
            egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 210, 120)),
        ));
    }
    for i in [0usize, n / 2, n - 1] {
        if let Some(t) = series.times.get(i) {
            p.text(
                egui::pos2(left + i as f32 * dx, rect.bottom() - 2.0),
                egui::Align2::CENTER_BOTTOM,
                t.format("%HZ").to_string(),
                egui::FontId::monospace(9.0),
                egui::Color32::from_gray(120),
            );
        }
    }
}
