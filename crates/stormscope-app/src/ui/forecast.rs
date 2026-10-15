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

    let scale = app.sidebar_scale.max(1.0);
    let size = super::popup_size(ctx, scale, 580.0, 520.0);
    let max_h = super::popup_max_height(ctx, scale);
    let site_id = app.state.site_id.clone();
    let mut open = app.state.forecast_open;
    let mut refetch = false;
    let mut pick_from_map = false;
    let mut clear_point = false;

    egui::Window::new("NWS forecast")
        .id(egui::Id::new("stormscope.forecast"))
        .open(&mut open)
        .default_size(size)
        .max_height(max_h)
        .min_size(egui::vec2(420.0 * scale, 220.0 * scale))
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .resizable(true)
        .show(ctx, |ui| {
            super::apply_text_scale(ui, scale);

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

            // --- Current conditions -------------------------------------
            match app.state.observation.as_ref() {
                Some(o) => current_conditions_ui(ui, o),
                None => {
                    ui.label(
                        egui::RichText::new("Current conditions: loading…")
                            .weak()
                            .small(),
                    );
                }
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
                        .max_height((ui.available_height() - 12.0 * scale).max(120.0))
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
    // Target changed: drop what is on screen rather than showing the old
    // point's numbers under the new point's label.
    if let Some((plat, plon)) = app.state.forecast_requested {
        if (plat - lat).abs() > 1e-6 || (plon - lon).abs() > 1e-6 {
            app.state.forecast = None;
            app.state.observation = None;
        }
    }
    app.state.forecast_requested = Some((lat, lon));
    app.state.forecast_site = match picked {
        Some(_) => "pinned".to_string(),
        None => app.state.site_id.clone(),
    };
    let _ = app.tx.send(crate::app::DataCmd::LoadForecast(lat, lon));
}

/// One line of "72°F · Light Rain · dew 61°F · …" for the current conditions,
/// converted to the US units the rest of the app uses.
fn current_conditions_ui(ui: &mut egui::Ui, o: &radar_core::forecast::Observation) {
    fn f(c: Option<f32>) -> Option<f32> {
        c.map(|v| v * 9.0 / 5.0 + 32.0)
    }
    let station = if o.station_name.is_empty() {
        o.station_id.clone()
    } else {
        format!("{} · {}", o.station_id, o.station_name)
    };
    ui.horizontal_wrapped(|ui| {
        ui.strong("Current conditions");
        ui.label(egui::RichText::new(station).weak().small());
    });

    let mut parts: Vec<String> = Vec::new();
    match f(o.temperature_c) {
        Some(t) => parts.push(format!("{t:.0}°F")),
        None => parts.push("—°F".into()),
    }
    if !o.text.is_empty() {
        parts.push(o.text.clone());
    }
    if let Some(d) = f(o.dewpoint_c) {
        parts.push(format!("dew {d:.0}°F"));
    }
    if let Some(h) = o.humidity_pct {
        parts.push(format!("RH {h:.0}%"));
    }
    if let Some(sp) = o.wind_speed_kmh {
        let mph = sp * 0.621371;
        let dir = o
            .wind_direction_deg
            .map(|d| format!(" {}", super::map::compass16(d as f64)))
            .unwrap_or_default();
        let gust = o
            .wind_gust_kmh
            .map(|g| format!(" gust {:.0}", g * 0.621371))
            .unwrap_or_default();
        parts.push(format!("wind{dir} {mph:.0} mph{gust}"));
    }
    if let Some(p) = o.pressure_pa {
        parts.push(format!("{:.2} inHg", p / 3386.389));
    }
    if let Some(v) = o.visibility_m {
        parts.push(format!("vis {:.1} mi", v / 1609.344));
    }
    if let Some(mm) = o.precip_last_hour_mm {
        if mm > 0.0 {
            parts.push(format!("{:.2} in/hr", mm / 25.4));
        }
    }
    if let Some(hi) = f(o.heat_index_c) {
        parts.push(format!("heat index {hi:.0}°F"));
    }
    if let Some(wc) = f(o.wind_chill_c) {
        parts.push(format!("wind chill {wc:.0}°F"));
    }
    ui.label(egui::RichText::new(parts.join(" · ")));

    if let Some(ts) = o.timestamp {
        let age = chrono::Utc::now() - ts;
        let mins = age.num_minutes().max(0);
        let age_text = if mins < 60 {
            format!("{mins} min ago")
        } else {
            format!("{} h {} min ago", mins / 60, mins % 60)
        };
        ui.label(
            egui::RichText::new(format!(
                "observed {} ({age_text}) · source api.weather.gov",
                crate::timefmt::fmt_central(&ts)
            ))
            .weak()
            .small(),
        );
    }
}
