//! NWS forecast window: the selected station's area by default, or a point
//! picked on the map.
//!
//! Forecasts come from `api.weather.gov`'s gridpoint endpoints (see
//! `radar_data::ForecastSource`); this module is only the presentation and
//! the station-vs-point bookkeeping.

use crate::app::StormApp;

/// The window (no-op while closed).
pub fn forecast_window(ctx: &egui::Context, app: &mut StormApp) {
    if !app.state.forecast_open {
        return;
    }
    // Following the station and the station changed (or nothing loaded yet):
    // refetch. A picked point is left alone.
    if app.state.forecast_point.is_none()
        && (app.state.forecast_site != app.state.site_id || app.state.forecast.is_none())
    {
        request_forecast_if_stale(app);
    }

    let scale = app.ui_scale.max(1.0);
    let site_id = app.state.site_id.clone();
    let mut open = app.state.forecast_open;
    let mut refetch = false;
    let mut pick_from_map = false;
    let mut clear_point = false;

    egui::Window::new("NWS forecast")
        .id(egui::Id::new("stormscope.forecast"))
        .open(&mut open)
        .default_size([560.0 * scale, 460.0 * scale])
        .min_width(420.0 * scale)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .resizable(true)
        .show(ctx, |ui| {
            // Which point this forecast is for.
            ui.horizontal_wrapped(|ui| {
                let following_station = app.state.forecast_point.is_none();
                if ui
                    .selectable_label(following_station, format!("{site_id} (station)"))
                    .on_hover_text("Forecast for the selected radar site's grid point")
                    .clicked()
                    && !following_station
                {
                    clear_point = true;
                    refetch = true;
                }
                let picking = app.state.forecast_pick_mode;
                if ui
                    .selectable_label(picking, "Pick on map…")
                    .on_hover_text("Then click anywhere on the map to forecast that spot")
                    .clicked()
                {
                    pick_from_map = !picking;
                }
                if let Some((lat, lon)) = app.state.forecast_point {
                    ui.label(
                        egui::RichText::new(format!("· pinned at {lat:.3}, {lon:.3}"))
                            .weak()
                            .small(),
                    );
                    if ui.small_button("Refresh").clicked() {
                        refetch = true;
                    }
                }
            });
            if app.state.forecast_pick_mode {
                ui.label(
                    egui::RichText::new("Click anywhere on the map to forecast that point.")
                        .strong()
                        .small(),
                );
            }
            ui.separator();

            match app.state.forecast.as_ref() {
                None => {
                    ui.label(
                        egui::RichText::new(if app.state.forecast_requested.is_some() {
                            "Loading forecast…"
                        } else {
                            "No forecast loaded yet."
                        })
                        .weak(),
                    );
                }
                Some(fc) => {
                    let mut meta = Vec::new();
                    if !fc.place.is_empty() {
                        ui.heading(&fc.place);
                    } else {
                        ui.heading("Forecast");
                    }
                    if !fc.office.is_empty() {
                        meta.push(format!("NWS grid {}", fc.office));
                    }
                    if !fc.grid.is_empty() {
                        meta.push(fc.grid.clone());
                    }
                    if let Some(u) = fc.updated {
                        meta.push(format!("updated {}", crate::timefmt::fmt_central(&u)));
                    }
                    if !meta.is_empty() {
                        ui.label(egui::RichText::new(meta.join(" · ")).weak().small());
                    }
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if fc.periods.is_empty() {
                                ui.label(
                                    egui::RichText::new(
                                        "The API returned no periods for this point.",
                                    )
                                    .weak(),
                                );
                                return;
                            }
                            for (i, p) in fc.periods.iter().enumerate() {
                                let temp = if p.temperature.is_finite() {
                                    format!("{:.0}°{}", p.temperature, p.unit)
                                } else {
                                    "—".to_string()
                                };
                                let title = if p.wind.is_empty() {
                                    format!("{} — {temp} · {}", p.name, p.short)
                                } else {
                                    format!("{} — {temp} · {} · {}", p.name, p.short, p.wind)
                                };
                                egui::CollapsingHeader::new(title)
                                    .id_salt(("forecast.period", i))
                                    .default_open(i == 0)
                                    .show(ui, |ui| {
                                        if p.detailed.is_empty() {
                                            ui.label(egui::RichText::new(&p.short).weak());
                                        } else {
                                            for para in p.detailed.split('\n') {
                                                if !para.trim().is_empty() {
                                                    ui.label(para);
                                                }
                                            }
                                        }
                                    });
                            }
                        });
                }
            }
        });

    app.state.forecast_open = open;
    if pick_from_map {
        app.state.forecast_pick_mode = true;
    }
    if clear_point {
        app.state.forecast_point = None;
        app.state.forecast_site.clear(); // force the refetch below
    }
    if refetch {
        if app.state.forecast_point.is_some() {
            // Same point, new data: forget the dedupe stamp.
            app.state.forecast_requested = None;
        }
        request_forecast_if_stale(app);
    }
}

/// Send a forecast request unless the loaded/requested one already matches
/// what the window would show: in station mode that means the selected
/// station, otherwise the picked point.
pub(super) fn request_forecast_if_stale(app: &mut StormApp) {
    let picked = app.state.forecast_point;
    let (lat, lon) = match picked {
        Some(p) => p,
        None => match radar_core::sites::site(&app.state.site_id) {
            Some(s) => (s.latitude, s.longitude),
            None => return,
        },
    };
    let already = app
        .state
        .forecast_requested
        .is_some_and(|(rlat, rlon)| (rlat - lat).abs() < 1e-6 && (rlon - lon).abs() < 1e-6);
    if already {
        return;
    }
    // In station mode a loaded forecast for this station counts as fresh.
    // (A picked point is only refreshed when the user asks, so it is exempt.)
    if picked.is_none()
        && app.state.forecast.is_some()
        && app.state.forecast_site == app.state.site_id
    {
        return;
    }
    app.state.forecast_requested = Some((lat, lon));
    app.state.forecast_site = match picked {
        Some(_) => "pinned".to_string(),
        None => app.state.site_id.clone(),
    };
    let _ = app.tx.send(crate::app::DataCmd::LoadForecast(lat, lon));
}
