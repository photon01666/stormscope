//! QPE-vs-gauge verification window.

use crate::app::StormApp;

pub fn verification_window(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    if !app.state.verification_open {
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(250));

    let Some(site) = radar_core::sites::site(&app.state.site_id) else {
        return;
    };
    let target = (site.latitude, site.longitude);
    let hours = app.state.verification_hours;
    let requested = Some((target.0, target.1, hours));
    if app.state.verification_requested != requested && app.state.verification_error.is_none() {
        app.state.verification_requested = requested;
        app.state.verification_pending = true;
        let _ = app.tx.send(crate::app::DataCmd::LoadVerification {
            lat: target.0,
            lon: target.1,
            hours,
        });
    }

    let fill = cfg!(target_os = "android");
    let scale = super::big_text_scale(app);
    let screen = ctx.content_rect().size();
    let size = super::popup_size(ctx, scale, 760.0, 500.0);
    let max_h = super::popup_max_height(ctx, scale);
    let mut open = app.state.verification_open;
    let mut refresh = false;
    let mut window = egui::Window::new("QPE vs rain gauges")
        .id(egui::Id::new("stormscope.verification"))
        .default_pos(super::centered_popup_pos(ctx, size))
        .max_width((screen.x - 16.0).max(200.0))
        .min_width((520.0 * scale).min((screen.x - 24.0).max(200.0)));
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
        // Size to the report instead of pinning a 500-point-tall window and
        // padding the leftover space. The table keeps its own scroll cap, so
        // a long list still fits on screen without an empty band underneath.
        window.auto_sized().max_height(max_h)
    };
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut close = false;
    window.open(&mut open).show(ctx, |ui| {
        #[cfg(target_os = "android")]
        if fill {
            close |= super::phone_header(ui, "QPE vs rain gauges");
        }
        super::apply_text_scale(ui, scale);

        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("MRMS QPE vs NWS gauge").strong());
            let mut new_hours = app.state.verification_hours;
            egui::ComboBox::from_id_salt("verification-hours")
                .selected_text(format!("{new_hours} h"))
                .show_ui(ui, |ui| {
                    for h in [1u16, 3, 6, 24] {
                        ui.selectable_value(&mut new_hours, h, format!("{h} h"));
                    }
                });
            if new_hours != app.state.verification_hours {
                app.state.verification_hours = new_hours;
                app.state.verification = None;
                app.state.verification_requested = None;
                app.state.verification_error = None;
            }
            if ui.small_button("Refresh").clicked() {
                refresh = true;
            }
            ui.label(
                egui::RichText::new(format!(
                    "near {} · {:.0}, {:.0}",
                    site.name, target.0, target.1
                ))
                .weak()
                .small(),
            );
        });

        if app.state.verification_pending && app.state.verification.is_none() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Downloading QPE and station reports…");
            });
        }
        if let Some(err) = &app.state.verification_error {
            ui.label(
                egui::RichText::new(format!("Verification failed: {err}"))
                    .weak()
                    .small(),
            );
        }
        if let Some(report) = app.state.verification.clone() {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!(
                    "{} · valid {} · {} gauge pairs",
                    report.product.label(),
                    crate::timefmt::fmt_central(&report.qpe_time),
                    report.n
                ))
                .weak()
                .small(),
            );
            egui::Grid::new("stormscope.verification.summary")
                .num_columns(4)
                .spacing([18.0, 5.0])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Mean gauge").weak());
                    ui.label(format!("{:.2} in", report.mean_gauge_in));
                    ui.label(egui::RichText::new("Mean QPE").weak());
                    ui.label(format!("{:.2} in", report.mean_qpe_in));
                    ui.end_row();
                    ui.label(egui::RichText::new("Bias").weak());
                    ui.label(format!("{:+.2} in", report.bias_in));
                    ui.label(egui::RichText::new("MAE").weak());
                    ui.label(format!("{:.2} in", report.mae_in));
                    ui.end_row();
                    ui.label(egui::RichText::new("RMSE").weak());
                    ui.label(format!("{:.2} in", report.rmse_in));
                    ui.label(egui::RichText::new("Correlation").weak());
                    ui.label(
                        report
                            .correlation
                            .map(|c| format!("{c:.2}"))
                            .unwrap_or_else(|| "—".into()),
                    );
                    ui.end_row();
                });
            ui.add_space(4.0);
            if report.pairs.is_empty() {
                ui.label(
                    egui::RichText::new(
                        "No stations reported a matching rainfall bucket inside the QPE window.",
                    )
                    .weak(),
                );
            } else {
                let table_h = (report.pairs.len() as f32 * 26.0 * scale)
                    .min((max_h - 250.0 * scale).max(120.0))
                    .max(120.0);
                egui::ScrollArea::vertical()
                    .id_salt("stormscope.verification.table")
                    .max_height(table_h)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        egui::Grid::new("stormscope.verification.pairs")
                            .num_columns(4)
                            .striped(true)
                            .spacing([16.0, 4.0])
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new("Station").strong());
                                ui.label(egui::RichText::new("Gauge").strong());
                                ui.label(egui::RichText::new("QPE").strong());
                                ui.label(egui::RichText::new("QPE − gauge").strong());
                                ui.end_row();
                                for pair in &report.pairs {
                                    ui.label(format!("{} {}", pair.station_id, pair.station_name));
                                    ui.label(format!("{:.2} in", pair.gauge_in));
                                    ui.label(format!("{:.2} in", pair.qpe_in));
                                    let d = pair.error_in();
                                    let color = if d.abs() < 0.10 {
                                        egui::Color32::from_rgb(130, 230, 150)
                                    } else if d.abs() < 0.50 {
                                        egui::Color32::from_rgb(255, 210, 110)
                                    } else {
                                        egui::Color32::from_rgb(255, 120, 110)
                                    };
                                    ui.colored_label(color, format!("{d:+.2} in"));
                                    ui.end_row();
                                }
                            });
                    });
            }
            ui.label(
                egui::RichText::new(
                    "Experimental comparison. Gauge buckets are rolling station reports, \
                     not a formal verification dataset.",
                )
                .weak()
                .small(),
            );
        }
    });
    if close {
        open = false;
    }
    if refresh {
        app.state.verification = None;
        app.state.verification_requested = None;
        app.state.verification_error = None;
        app.state.verification_pending = true;
        let _ = app.tx.send(crate::app::DataCmd::LoadVerification {
            lat: target.0,
            lon: target.1,
            hours,
        });
    }
    app.state.verification_open = open;
}
