//! Point-model comparison: Open-Meteo model, RAP analysis, HRRR forecast and
//! optionally an observed RAOB.

use crate::app::StormApp;
use radar_core::sounding::{Sounding, SoundingLevel};

pub fn model_compare_window(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    if !app.state.model_compare_open {
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(250));
    let Some(site) = radar_core::sites::site(&app.state.site_id) else {
        return;
    };
    let target = (site.latitude, site.longitude);

    // Kick off whichever profiles are missing. The Sounding and HRRR windows
    // use the same state, so this shares their in-flight requests.
    if app.state.sounding.as_ref().is_none_or(|s| !near(s, target))
        && app.state.sounding_requested != Some(target)
        && app.state.sounding_error.is_none()
    {
        app.state.sounding_requested = Some(target);
        app.state.sounding_error = None;
        let _ = app
            .tx
            .send(crate::app::DataCmd::LoadSounding(target.0, target.1));
    }
    if app
        .state
        .rap_sounding
        .as_ref()
        .is_none_or(|s| !near(s, target))
        && app.state.rap_sounding_requested != Some(target)
        && app.state.rap_sounding_error.is_none()
    {
        app.state.rap_sounding_requested = Some(target);
        app.state.rap_sounding_error = None;
        let _ = app
            .tx
            .send(crate::app::DataCmd::LoadRapSounding(target.0, target.1));
    }
    if app.state.hrrr.as_ref().is_none_or(|s| {
        (s.latitude - target.0).abs() > 0.001 || (s.longitude - target.1).abs() > 0.001
    }) && app.state.hrrr_requested != Some(target)
        && app.state.hrrr_error.is_none()
    {
        app.state.hrrr_requested = Some(target);
        app.state.hrrr_error = None;
        let _ = app
            .tx
            .send(crate::app::DataCmd::LoadHrrr(target.0, target.1));
    }
    if app.state.model_compare_include_observed
        && app
            .state
            .observed_sounding
            .as_ref()
            .is_none_or(|s| !near(s, target))
        && app.state.observed_sounding_requested != Some(target)
        && app.state.observed_sounding_error.is_none()
    {
        app.state.observed_sounding_requested = Some(target);
        app.state.observed_sounding_error = None;
        let _ = app.tx.send(crate::app::DataCmd::LoadObservedSounding(
            target.0, target.1,
        ));
    }

    let hrrr_idx = app
        .state
        .hrrr
        .as_ref()
        .map(|s| {
            app.state
                .model_compare_hrrr_hour
                .min(s.profiles.len().saturating_sub(1))
        })
        .unwrap_or(0);
    let model = app.state.sounding.clone();
    let rap = app.state.rap_sounding.clone();
    let hrrr = app
        .state
        .hrrr
        .as_ref()
        .and_then(|s| s.profiles.get(hrrr_idx))
        .cloned();
    let observed = app
        .state
        .model_compare_include_observed
        .then(|| app.state.observed_sounding.clone())
        .flatten();

    let fill = cfg!(target_os = "android");
    let scale = super::big_text_scale(app);
    let screen = ctx.content_rect().size();
    let max_h = super::popup_max_height(ctx, scale);
    // Keep a stable compact window while profiles load: auto-sizing a table
    // inside a scroll area can collapse or snap to a very different size as
    // the first profile arrives.
    let desktop_w = if app.state.model_compare_include_observed {
        600.0
    } else {
        460.0
    } * scale;
    let desktop_h = (430.0 * scale).min(max_h);
    let mut open = app.state.model_compare_open;
    let mut refresh = false;
    let mut window = egui::Window::new("Model comparison")
        .id(egui::Id::new("stormscope.model_compare.compact"))
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .max_width((screen.x - 16.0).max(240.0))
        .resizable(false);
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
        window
            .fixed_size(egui::vec2(desktop_w, desktop_h))
            .collapsible(false)
    };
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut close = false;
    window.open(&mut open).show(ctx, |ui| {
        #[cfg(target_os = "android")]
        if fill {
            close |= super::phone_header(ui, "Model comparison");
        }
        super::apply_text_scale(ui, scale);
        ui.label(
            egui::RichText::new(format!("{} · {:.2},{:.2}", site.name, target.0, target.1))
                .strong(),
        );
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(
                &mut app.state.model_compare_include_observed,
                "Include observed RAOB",
            );
            if ui.small_button("Refresh all").clicked() {
                refresh = true;
            }
            if let Some(series) = &app.state.hrrr
                && !series.profiles.is_empty()
            {
                let mut hour = app
                    .state
                    .model_compare_hrrr_hour
                    .min(series.profiles.len() - 1);
                egui::ComboBox::from_id_salt("compare-hrrr-hour")
                    .selected_text(format!("HRRR f{hour:02}"))
                    .show_ui(ui, |ui| {
                        for i in 0..series.profiles.len().min(24) {
                            let label = series
                                .profiles
                                .get(i)
                                .map(|p| format!("f{i:02} · {}", p.valid_time.format("%H:%MZ")))
                                .unwrap_or_else(|| format!("f{i:02}"));
                            ui.selectable_value(&mut hour, i, label);
                        }
                    });
                app.state.model_compare_hrrr_hour = hour;
            }
        });
        let mut status = Vec::new();
        if app.state.sounding_requested == Some(target) && model.is_none() {
            status.push("model loading");
        }
        if app.state.rap_sounding_requested == Some(target) && rap.is_none() {
            status.push("RAP loading");
        }
        if app.state.hrrr_requested == Some(target) && hrrr.is_none() {
            status.push("HRRR loading");
        }
        if app.state.model_compare_include_observed
            && app.state.observed_sounding_requested == Some(target)
            && observed.is_none()
        {
            status.push("RAOB loading");
        }
        if !status.is_empty() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(status.join(" · "));
            });
        }
        let mut errors = vec![
            ("Model", app.state.sounding_error.as_ref()),
            ("RAP", app.state.rap_sounding_error.as_ref()),
            ("HRRR", app.state.hrrr_error.as_ref()),
        ];
        if app.state.model_compare_include_observed {
            errors.push(("RAOB", app.state.observed_sounding_error.as_ref()));
        }
        for (label, error) in errors {
            if let Some(error) = error {
                ui.label(
                    egui::RichText::new(format!("{label}: {error}"))
                        .weak()
                        .small(),
                );
            }
        }

        let mut profiles: Vec<(String, Option<Sounding>)> = vec![
            ("Model".to_string(), model),
            ("RAP".to_string(), rap),
            (format!("HRRR f{hrrr_idx:02}"), hrrr),
        ];
        if app.state.model_compare_include_observed {
            profiles.push(("Observed RAOB".to_string(), observed));
        }
        if profiles.iter().all(|(_, p)| p.is_none()) {
            ui.label("Waiting for the first profile…");
            return;
        }
        ui.add_space(4.0);
        let table_h = if fill {
            (max_h - 160.0 * scale).max(180.0)
        } else {
            (desktop_h - 140.0 * scale).max(160.0)
        };
        egui::ScrollArea::both()
            .id_salt("stormscope.model_compare.table")
            .max_height(table_h)
            .auto_shrink([true, true])
            .show(ui, |ui| {
                egui::Grid::new("stormscope.model_compare.grid")
                    .num_columns(profiles.len() + 1)
                    .striped(true)
                    .spacing([18.0, 4.0])
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new("Metric").strong());
                        for (label, _) in &profiles {
                            ui.label(egui::RichText::new(label).strong());
                        }
                        ui.end_row();
                        row(ui, "Valid", &profiles, |s| {
                            crate::timefmt::fmt_central(&s.valid_time)
                        });
                        row(ui, "Surface pressure", &profiles, |s| {
                            format!("{:.0} hPa", s.surface_pressure_hpa)
                        });
                        row(ui, "Surface temp", &profiles, |s| {
                            format!("{:.0}°F", to_f(s.surface_temp_c))
                        });
                        row(ui, "Surface dew point", &profiles, |s| {
                            format!("{:.0}°F", to_f(s.surface_dewpoint_c))
                        });
                        row(ui, "CAPE", &profiles, |s| {
                            format!("{:.0} J/kg", s.sb_cape_jkg)
                        });
                        row(ui, "CIN", &profiles, |s| {
                            format!("{:.0} J/kg", s.sb_cin_jkg)
                        });
                        row(ui, "PWAT", &profiles, |s| format!("{:.1} mm", s.pwat_mm));
                        row(ui, "LCL", &profiles, |s| {
                            s.lcl_height_m
                                .map(|m| format!("{:.0} ft", m * 3.280_84))
                                .unwrap_or_else(|| "—".into())
                        });
                        row(ui, "LFC", &profiles, |s| {
                            s.lfc_pressure_hpa
                                .map(|p| format!("{p:.0} hPa"))
                                .unwrap_or_else(|| "—".into())
                        });
                        row(ui, "EL", &profiles, |s| {
                            s.el_pressure_hpa
                                .map(|p| format!("{p:.0} hPa"))
                                .unwrap_or_else(|| "—".into())
                        });
                        row(ui, "0–6 km shear", &profiles, |s| {
                            format!("{:.0} mph", s.shear_0_6km_ms * 2.236_94)
                        });
                        row(ui, "0–3 km SRH", &profiles, |s| {
                            format!("{:.0} m²/s²", s.srh_0_3km)
                        });
                        row(ui, "500 mb temp", &profiles, |s| {
                            at_level(s, 500.0)
                                .map(|l| format!("{:.0}°F", to_f(l.temp_c)))
                                .unwrap_or_else(|| "—".into())
                        });
                        row(ui, "500 mb height", &profiles, |s| {
                            at_level(s, 500.0)
                                .map(|l| format!("{:.0} dam", l.height_m / 10.0))
                                .unwrap_or_else(|| "—".into())
                        });
                        row(ui, "500 mb wind", &profiles, |s| {
                            at_level(s, 500.0)
                                .map(|l| format!("{:.0} kt", l.wind_speed_ms * 1.943_844))
                                .unwrap_or_else(|| "—".into())
                        });
                        row(ui, "700 mb RH", &profiles, |s| {
                            at_level(s, 700.0)
                                .map(|l| format!("{:.0}%", l.rh_pct))
                                .unwrap_or_else(|| "—".into())
                        });
                    });
            });
        if let Some((min_cape, max_cape)) = spread(&profiles, |s| s.sb_cape_jkg) {
            ui.label(
                egui::RichText::new(format!(
                    "CAPE spread: {:.0}–{:.0} J/kg · 0–6 km shear spread: {:.0}–{:.0} mph",
                    min_cape,
                    max_cape,
                    spread(&profiles, |s| s.shear_0_6km_ms * 2.236_94)
                        .map(|(a, _)| a)
                        .unwrap_or(0.0),
                    spread(&profiles, |s| s.shear_0_6km_ms * 2.236_94)
                        .map(|(_, b)| b)
                        .unwrap_or(0.0),
                ))
                .weak()
                .small(),
            );
        }
    });
    if close {
        open = false;
    }
    if refresh {
        app.state.sounding = None;
        app.state.sounding_requested = None;
        app.state.sounding_error = None;
        app.state.rap_sounding = None;
        app.state.rap_sounding_requested = None;
        app.state.rap_sounding_error = None;
        app.state.hrrr = None;
        app.state.hrrr_requested = None;
        app.state.hrrr_error = None;
        app.state.observed_sounding = None;
        app.state.observed_sounding_requested = None;
        app.state.observed_sounding_error = None;
        app.state.sounding_requested = Some(target);
        app.state.rap_sounding_requested = Some(target);
        app.state.hrrr_requested = Some(target);
        let _ = app
            .tx
            .send(crate::app::DataCmd::LoadSounding(target.0, target.1));
        let _ = app
            .tx
            .send(crate::app::DataCmd::LoadRapSounding(target.0, target.1));
        let _ = app
            .tx
            .send(crate::app::DataCmd::LoadHrrr(target.0, target.1));
        if app.state.model_compare_include_observed {
            app.state.observed_sounding_requested = Some(target);
            let _ = app.tx.send(crate::app::DataCmd::LoadObservedSounding(
                target.0, target.1,
            ));
        }
    }
    app.state.model_compare_open = open;
}

fn near(s: &Sounding, target: (f64, f64)) -> bool {
    (s.latitude - target.0).abs() < 0.001 && (s.longitude - target.1).abs() < 0.001
}

fn to_f(c: f64) -> f64 {
    c * 9.0 / 5.0 + 32.0
}

fn at_level(s: &Sounding, pressure: f64) -> Option<&SoundingLevel> {
    s.levels.iter().min_by(|a, b| {
        (a.pressure_hpa - pressure)
            .abs()
            .partial_cmp(&(b.pressure_hpa - pressure).abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn row(
    ui: &mut egui::Ui,
    label: &str,
    profiles: &[(String, Option<Sounding>)],
    f: impl Fn(&Sounding) -> String,
) {
    ui.label(egui::RichText::new(label).weak());
    for (_, profile) in profiles {
        ui.label(profile.as_ref().map(&f).unwrap_or_else(|| "—".into()));
    }
    ui.end_row();
}

fn spread(
    profiles: &[(String, Option<Sounding>)],
    f: impl Fn(&Sounding) -> f64,
) -> Option<(f64, f64)> {
    let mut values = profiles
        .iter()
        .filter_map(|(_, p)| p.as_ref())
        .map(&f)
        .filter(|v| v.is_finite())
        .collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some((values[0], *values.last().unwrap()))
}
