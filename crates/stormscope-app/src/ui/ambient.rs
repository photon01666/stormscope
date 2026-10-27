//! Ambient Weather personal-station popup.
//!
//! Ambient's `lastData` is already in US units (tempf, windspeedmph,
//! baromrelin, ...), so this window mostly labels and orders it. The layout
//! matches the Synoptic/NWS windows: a big temperature headline with an
//! animated glyph, then a label/value table. Android fills the pane width and
//! centres a short card; desktop is a fixed-size dialog.

use crate::app::StormApp;
use crate::ui::forecast::{WeatherIcon, draw_weather_icon};
use radar_data::ambient::AmbientStation;

pub fn ambient_window(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    if !app.state.ambient_open {
        return;
    }
    let fill = cfg!(target_os = "android");
    let scale = super::big_text_scale(app);
    let size = super::popup_size(ctx, 1.0, 320.0, 1000.0);
    let mut open = app.state.ambient_open;
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut close_requested = false;

    let mut window = egui::Window::new("My weather station")
        .id(egui::Id::new("stormscope.ambient"))
        .default_pos(super::centered_popup_pos(ctx, size));
    #[cfg(target_os = "android")]
    if fill {
        window = window.title_bar(false);
    }
    window = if fill {
        let inset = area.shrink2(egui::vec2(4.0, 8.0));
        let max_h = (inset.height() - 8.0).max(160.0);
        window
            .default_size(egui::vec2(inset.width(), max_h))
            .max_height(max_h)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .constrain_to(inset)
            .movable(false)
            .resizable(false)
    } else {
        window
            .default_size(size)
            .max_height(super::popup_max_height(ctx, scale))
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .resizable(false)
    };
    let window = window.open(&mut open);
    window.show(ctx, |ui| {
        #[cfg(target_os = "android")]
        if fill {
            close_requested |= super::phone_header(ui, "My weather station");
        }
        super::apply_text_scale(ui, scale);

        let station = app.state.ambient_selected.as_ref().and_then(|mac| {
            app.state
                .ambient_stations
                .iter()
                .find(|s| &s.mac_address == mac)
                .cloned()
        });
        let Some(station) = station else {
            ui.label("No station selected.");
            return;
        };

        if fill {
            // Phone: full pane width.
            let w = ui.available_width();
            ui.set_width(w);
        } else {
            // Desktop: let the label/value table decide the width (with a
            // comfortable floor) instead of forcing a 420 pt box with a big
            // empty right-hand side.
            ui.set_min_width(280.0);
            ui.set_max_width(320.0);
        }

        egui::ScrollArea::vertical()
            .id_salt("stormscope.ambient.scroll")
            .auto_shrink([!fill, true])
            .show(ui, |ui| {
                header_ui(ui, &station);
                ui.separator();
                let rows = ambient_rows(&station);
                if rows.is_empty() {
                    ui.label(egui::RichText::new("No observation data available.").weak());
                } else {
                    egui::Grid::new("stormscope.ambient.grid")
                        .num_columns(2)
                        .spacing([16.0, 6.0])
                        .show(ui, |ui| {
                            for (label, value) in &rows {
                                ui.label(egui::RichText::new(label).weak().size(15.0));
                                ui.label(egui::RichText::new(value).strong().size(18.0));
                                ui.end_row();
                            }
                        });
                }
                if app.state.ambient_pending {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Loading…");
                    });
                }
                if let Some(e) = &app.state.ambient_error {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(e)
                            .small()
                            .color(egui::Color32::from_rgb(255, 120, 120)),
                    );
                }
            });
    });
    if close_requested {
        open = false;
    }
    app.state.ambient_open = open;
}

fn header_ui(ui: &mut egui::Ui, st: &AmbientStation) {
    let ctx = ui.ctx().clone();
    let temp = temp_f(st);
    let kind = icon_kind(st);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(56.0, 56.0), egui::Sense::hover());
        // The rain/snow strokes extend below the glyph box; shift the glyph
        // up slightly so it does not run into the station name underneath.
        let icon_rect = egui::Rect::from_center_size(
            rect.center() - egui::vec2(0.0, 4.0),
            egui::vec2(44.0, 44.0),
        );
        draw_weather_icon(&ctx, ui.painter(), icon_rect, kind);
        ui.vertical(|ui| {
            match temp {
                Some(t) => {
                    let shown = ui.animate_value_with_time(
                        egui::Id::new(("stormscope.ambient.temp", st.mac_address.as_str())),
                        t as f32,
                        0.6,
                    );
                    ui.label(
                        egui::RichText::new(format!("{shown:.0}°F"))
                            .size(34.0)
                            .strong(),
                    );
                }
                None => {
                    if let Some((label, value)) = ambient_rows(st).first() {
                        ui.label(egui::RichText::new(value).size(26.0).strong());
                        ui.label(egui::RichText::new(label).weak().size(13.0));
                    } else {
                        ui.label(egui::RichText::new("—").size(30.0).strong().weak());
                    }
                }
            }
            ui.label(egui::RichText::new(condition_word(st)).size(17.0));
        });
    });
    ui.add_space(2.0);
    ui.label(egui::RichText::new(st.label()).strong().size(18.0));
    let mut meta = Vec::new();
    if !st.location.is_empty() {
        meta.push(st.location.clone());
    }
    if !st.mac_address.is_empty() {
        meta.push(st.mac_address.clone());
    }
    if !meta.is_empty() {
        ui.horizontal(|ui| {
            let (dot, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
            let pulse = 0.5 + 0.5 * (ctx.input(|i| i.time) * 2.0).sin();
            ui.painter().circle_filled(
                dot.center(),
                4.5,
                egui::Color32::from_rgba_unmultiplied(90, 220, 120, (110.0 + 110.0 * pulse) as u8),
            );
            ui.label(egui::RichText::new(meta.join(" · ")).weak().small());
        });
    }
}

fn condition_word(st: &AmbientStation) -> &'static str {
    match icon_kind(st) {
        WeatherIcon::Rain => "Rain",
        WeatherIcon::Showers => "Showers",
        WeatherIcon::Thunder => "Thunderstorms",
        WeatherIcon::Snow => "Snow",
        WeatherIcon::Fog => "Fog",
        WeatherIcon::Cloudy => "Cloudy",
        WeatherIcon::MostlyDay => "Mostly cloudy",
        WeatherIcon::PartlyDay | WeatherIcon::PartlyNight => "Partly cloudy",
        WeatherIcon::Sunny => "Sunny",
        WeatherIcon::ClearNight => "Clear",
    }
}

fn icon_kind(st: &AmbientStation) -> WeatherIcon {
    let rain = num(st, "hourlyrainin")
        .or_else(|| num(st, "dailyrainin"))
        .unwrap_or(0.0);
    let temp = temp_f(st);
    let rh = num(st, "humidity");
    let wind = num(st, "windspeedmph").unwrap_or(0.0);
    let solar = num(st, "solarradiation");
    let uv = num(st, "uv");
    let daytime = is_daytime(st);

    if rain > 0.0 {
        if temp.is_some_and(|t| t <= 33.0) {
            return WeatherIcon::Snow;
        }
        return WeatherIcon::Rain;
    }
    if let Some(rh) = rh
        && rh >= 95.0
        && wind < 3.0
    {
        return WeatherIcon::Fog;
    }
    if let Some(s) = solar {
        if s >= 500.0 {
            return WeatherIcon::Sunny;
        }
        if s >= 180.0 {
            return if daytime {
                WeatherIcon::PartlyDay
            } else {
                WeatherIcon::PartlyNight
            };
        }
        if daytime {
            return WeatherIcon::Cloudy;
        }
    }
    if let Some(uv) = uv
        && uv >= 3.0
    {
        return WeatherIcon::Sunny;
    }
    if daytime {
        WeatherIcon::Sunny
    } else {
        WeatherIcon::ClearNight
    }
}

fn is_daytime(st: &AmbientStation) -> bool {
    let utc_hour = chrono::Utc::now().hour() as f64;
    let lon = st.coords.as_ref().map(|c| c.lon).unwrap_or(-97.0);
    let local = (utc_hour + lon / 15.0).rem_euclid(24.0);
    (6.5..19.5).contains(&local)
}

use chrono::Timelike as _;

/// Ambient station temperature in °F.
pub(super) fn temp_f(st: &AmbientStation) -> Option<f64> {
    num(st, "tempf")
}

/// One numeric `lastData` value (Ambient occasionally sends strings).
fn num(st: &AmbientStation, key: &str) -> Option<f64> {
    let v = st.last_data.get(key)?;
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

pub(super) fn ambient_rows(st: &AmbientStation) -> Vec<(String, String)> {
    const ORDER: &[(&str, &str, &str, usize)] = &[
        // key, label, suffix, decimals
        ("tempf", "Temperature", "°F", 1),
        ("feelsLike", "Feels like", "°F", 1),
        ("dewPoint", "Dew point", "°F", 1),
        ("humidity", "Humidity", "%", 0),
        ("windspeedmph", "Wind speed", "mph", 1),
        ("windgustmph", "Wind gust", "mph", 1),
        ("maxdailygust", "Max gust today", "mph", 1),
        ("baromrelin", "Pressure (rel)", "inHg", 2),
        ("baromabsin", "Pressure (abs)", "inHg", 2),
        ("hourlyrainin", "Precip (1 h)", "in", 2),
        ("eventrainin", "Precip (event)", "in", 2),
        ("dailyrainin", "Precip (today)", "in", 2),
        ("weeklyrainin", "Precip (week)", "in", 2),
        ("monthlyrainin", "Precip (month)", "in", 2),
        ("yearlyrainin", "Precip (year)", "in", 2),
        ("solarradiation", "Solar radiation", "W/m²", 0),
        ("uv", "UV index", "", 0),
        ("tempinf", "Indoor temp", "°F", 1),
        ("humidityin", "Indoor humidity", "%", 0),
    ];
    let mut rows = Vec::new();
    for (key, label, suffix, decimals) in ORDER {
        let Some(v) = num(st, key) else { continue };
        let value = if suffix.is_empty() {
            format!("{v:.decimals$}")
        } else {
            format!("{v:.decimals$} {suffix}")
        };
        rows.push((label.to_string(), value));
    }
    // Wind direction reads better as a compass point plus degrees.
    if let Some(dir) = num(st, "winddir") {
        let c = super::map::compass16(dir);
        rows.insert(
            5.min(rows.len()),
            ("Wind direction".to_string(), format!("{c} ({dir:.0}°)")),
        );
    }
    if let Some(rain) = st.last_data.get("lastRain").and_then(|v| v.as_str()) {
        rows.push(("Last rain".to_string(), rain.to_string()));
    }
    rows
}

/// Settings popup for the Ambient Weather API + application keys.
pub fn ambient_settings_window(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    if !app.state.ambient_settings_open {
        return;
    }
    let fill = cfg!(target_os = "android");
    let scale = super::big_text_scale(app);
    let size = super::popup_size(ctx, scale, 540.0, 480.0);
    let mut open = app.state.ambient_settings_open;
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut close_requested = false;

    let mut window = egui::Window::new("Ambient station settings")
        .id(egui::Id::new("stormscope.ambient.settings"))
        .default_pos(super::centered_popup_pos(ctx, size));
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
            .fixed_size(size)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .resizable(false)
    };
    let window = window.open(&mut open);
    window.show(ctx, |ui| {
        #[cfg(target_os = "android")]
        if fill {
            close_requested |= super::phone_header(ui, "Ambient station settings");
        }
        super::apply_text_scale(ui, scale);
        if !fill {
            let w = (size.x - 40.0).max(320.0);
            ui.set_width(w);
        }
        egui::ScrollArea::vertical()
            .id_salt("stormscope.ambient.settings.scroll")
            .auto_shrink([false, !fill])
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(
                        "Your own Ambient Weather personal station, shown as a \
                         green thermometer next to the Synoptic network. Both \
                         keys are free from AmbientWeather.net → Account → API \
                         Keys: an API key and an Application key.",
                    )
                    .weak(),
                );
                ui.separator();
                ui.label("API key");
                let api_resp = ui.add(
                    egui::TextEdit::singleline(&mut app.state.ambient_api_key)
                        .password(!app.state.ambient_show_keys)
                        .desired_width(ui.available_width())
                        .hint_text("Ambient Weather API key"),
                );
                ui.label("Application key");
                let app_resp = ui.add(
                    egui::TextEdit::singleline(&mut app.state.ambient_app_key)
                        .password(!app.state.ambient_show_keys)
                        .desired_width(ui.available_width())
                        .hint_text("Ambient Weather application key"),
                );
                if api_resp.lost_focus() || app_resp.lost_focus() {
                    app.state.persist_ambient();
                    app.fetch_ambient_now();
                }
                ui.horizontal(|ui| {
                    if ui
                        .small_button(if app.state.ambient_show_keys {
                            "Hide keys"
                        } else {
                            "Show keys"
                        })
                        .clicked()
                    {
                        app.state.ambient_show_keys = !app.state.ambient_show_keys;
                    }
                });
                if ui
                    .checkbox(
                        &mut app.state.ambient_remember_keys,
                        "Remember the keys on disk (plain text, 0600)",
                    )
                    .changed()
                {
                    app.state.persist_ambient();
                }
                ui.add_space(6.0);
                if app.state.ambient_pending {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Loading my station…");
                    });
                } else if let Some(e) = &app.state.ambient_error {
                    ui.label(
                        egui::RichText::new(e)
                            .small()
                            .color(egui::Color32::from_rgb(255, 120, 120)),
                    );
                } else if !app.state.ambient_stations.is_empty() {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} station{} loaded — your station refreshes automatically",
                            app.state.ambient_stations.len(),
                            if app.state.ambient_stations.len() == 1 {
                                ""
                            } else {
                                "s"
                            }
                        ))
                        .weak()
                        .small(),
                    );
                }
            });
    });
    if close_requested {
        open = false;
    }
    app.state.ambient_settings_open = open;
}
