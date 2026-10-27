//! Synoptic (Mesowest) station popup.
//!
//! Formats the raw API observation the way the NWS forecast window does:
//! a large temperature headline with an animated condition glyph, then a
//! label/value table in the US units the rest of the app uses (°F, mph,
//! inHg, in). On Android the window fills the map pane; on desktop it is a
//! fixed-size, non-resizable dialog.

use crate::app::StormApp;
use crate::ui::forecast::{WeatherIcon, draw_weather_icon};
use radar_data::synoptic::SynopticStation;

pub fn synoptic_window(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    if !app.state.synoptic_open {
        return;
    }
    let fill = cfg!(target_os = "android");
    let scale = super::big_text_scale(app);
    let size = super::popup_size(ctx, scale, 560.0, 620.0);
    let mut open = app.state.synoptic_open;
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut close_requested = false;

    let mut window = egui::Window::new("Station observation")
        .id(egui::Id::new("stormscope.synoptic"))
        .default_pos(super::centered_popup_pos(ctx, size));
    #[cfg(target_os = "android")]
    if fill {
        window = window.title_bar(false);
    }
    window = if fill {
        // Phone: full pane width, height driven by the content but capped at
        // the pane. A short station sits as a centred card; a station with
        // many variables grows to fill the pane and scrolls.
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
        // Fixed size, like the other desktop popups: the table always has
        // the same footprint and scrolls if a station reports many variables.
        window.fixed_size(size).resizable(false)
    };
    let window = window.open(&mut open);
    window.show(ctx, |ui| {
        #[cfg(target_os = "android")]
        if fill {
            close_requested |= super::phone_header(ui, "Station observation");
        }
        super::apply_text_scale(ui, scale);

        let station = app.state.synoptic_selected.as_ref().and_then(|stid| {
            app.state
                .synoptic_stations
                .iter()
                .find(|s| &s.stid == stid)
                .cloned()
        });

        let Some(station) = station else {
            ui.label("No station selected.");
            return;
        };

        let width = if fill {
            ui.available_width()
        } else {
            (size.x - 34.0).max(300.0)
        };
        ui.set_width(width);

        egui::ScrollArea::vertical()
            .id_salt("stormscope.synoptic.scroll")
            .auto_shrink([false, fill])
            .show(ui, |ui| {
                header_ui(ui, &station);
                ui.separator();
                let rows = obs_rows(&station);
                if rows.is_empty() {
                    ui.label(egui::RichText::new("No observation data available.").weak());
                } else {
                    egui::Grid::new("stormscope.synoptic.grid")
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

                if app.state.synoptic_pending {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Loading…");
                    });
                }
                if let Some(e) = &app.state.synoptic_error {
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
    app.state.synoptic_open = open;
}

/// Temperature headline with an animated condition glyph, then the station's
/// identity line.
fn header_ui(ui: &mut egui::Ui, st: &SynopticStation) {
    let ctx = ui.ctx().clone();
    let temp_c = temp_c(st);
    let kind = icon_kind(st);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(52.0, 52.0), egui::Sense::hover());
        draw_weather_icon(&ctx, ui.painter(), rect, kind);
        ui.vertical(|ui| {
            match temp_c {
                Some(c) => {
                    let f = (c * 9.0 / 5.0 + 32.0) as f32;
                    let shown = ui.animate_value_with_time(
                        egui::Id::new(("stormscope.synoptic.temp", st.stid.as_str())),
                        f,
                        0.6,
                    );
                    ui.label(
                        egui::RichText::new(format!("{shown:.0}°F"))
                            .size(34.0)
                            .strong(),
                    );
                }
                None => {
                    ui.label(egui::RichText::new("—°F").size(34.0).strong().weak());
                }
            }
            ui.label(egui::RichText::new(condition_word(st)).size(17.0));
        });
    });
    ui.add_space(2.0);
    ui.label(egui::RichText::new(st.label()).strong().size(18.0));
    let mut meta = Vec::new();
    if !st.stid.is_empty() {
        meta.push(st.stid.clone());
    }
    if !st.network.is_empty() {
        meta.push(st.network.clone());
    }
    if let Some(e) = st.elevation_m {
        meta.push(format!("{:.0} ft", e * 3.280_84));
    }
    if !meta.is_empty() {
        // A softly pulsing "live" dot marks fresh data, matching the forecast.
        let (dot, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
        let pulse = 0.5 + 0.5 * (ctx.input(|i| i.time) * 2.0).sin();
        ui.painter().circle_filled(
            dot.center(),
            4.5,
            egui::Color32::from_rgba_unmultiplied(90, 220, 120, (110.0 + 110.0 * pulse) as u8),
        );
        ui.label(egui::RichText::new(meta.join(" · ")).weak().small());
    }
}

fn condition_word(st: &SynopticStation) -> &'static str {
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

/// Pick a condition glyph from the observation: precipitation wins, then
/// cloud cover, then fog (very humid with light wind).
fn icon_kind(st: &SynopticStation) -> WeatherIcon {
    let precip = value_of(st, "precip_accum_1_hour")
        .or_else(|| value_of(st, "precip_accum_since_local_midnight"))
        .or_else(|| value_of(st, "precip_accum_24_hour"))
        .map(|(v, _)| v)
        .unwrap_or(0.0);
    let temp_c = temp_c(st);
    let rh = value_of(st, "relative_humidity").map(|(v, _)| v);
    let cloud = value_of(st, "cloud_cover").map(|(v, _)| v);
    let wind = value_of(st, "wind_speed").map(|(v, _)| v).unwrap_or(0.0);
    let daytime = is_daytime(st);

    if precip > 0.0 {
        if temp_c.is_some_and(|t| t <= 0.5) {
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
    if let Some(c) = cloud {
        if c >= 80.0 {
            return WeatherIcon::Cloudy;
        }
        if c >= 35.0 {
            return if daytime {
                WeatherIcon::PartlyDay
            } else {
                WeatherIcon::PartlyNight
            };
        }
        return if daytime {
            WeatherIcon::Sunny
        } else {
            WeatherIcon::ClearNight
        };
    }
    if daytime {
        WeatherIcon::Sunny
    } else {
        WeatherIcon::ClearNight
    }
}

/// Approximate local daytime from UTC and the station's longitude.
fn is_daytime(st: &SynopticStation) -> bool {
    let utc_hour = chrono::Utc::now()
        .format("%H")
        .to_string()
        .parse::<f64>()
        .unwrap_or(12.0);
    let lon = st.coords.as_ref().map(|c| c.lon).unwrap_or(-97.0);
    let local = (utc_hour + lon / 15.0).rem_euclid(24.0);
    (6.5..19.5).contains(&local)
}

/// The numeric value of one observation variable (base name), with its unit.
pub(super) fn value_of(st: &SynopticStation, base: &str) -> Option<(f64, String)> {
    let obj = st.obs.as_object()?;
    for (k, v) in obj {
        if base_var(k) != base {
            continue;
        }
        let raw = v.get("value")?;
        let n = raw.as_f64()?;
        let unit = st
            .units
            .get(base)
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .to_string();
        return Some((n, unit));
    }
    None
}

/// Station air temperature in Celsius (from whatever unit it reports).
pub(super) fn temp_c(st: &SynopticStation) -> Option<f64> {
    value_of(st, "air_temp").and_then(|(v, unit)| to_celsius(v, &unit))
}

fn to_celsius(v: f64, unit: &str) -> Option<f64> {
    match unit {
        "Celsius" | "degrees C" => Some(v),
        "Fahrenheit" => Some((v - 32.0) * 5.0 / 9.0),
        _ => None,
    }
}

/// Ordered, US-unit display rows for a station's observations.
pub(super) fn obs_rows(st: &SynopticStation) -> Vec<(String, String)> {
    let Some(obj) = st.obs.as_object() else {
        return Vec::new();
    };
    let mut present: Vec<&str> = Vec::new();
    for k in obj.keys() {
        let base = base_var(k);
        if matches!(
            base,
            "date_time" | "weather_cond_code" | "wind_cardinal_direction"
        ) {
            continue;
        }
        if !present.contains(&base) {
            present.push(base);
        }
    }
    // Preferred order: the values a spotter scans first.
    const ORDER: &[&str] = &[
        "air_temp",
        "dew_point_temperature",
        "relative_humidity",
        "wind_speed",
        "wind_gust",
        "wind_direction",
        "altimeter",
        "sea_level_pressure",
        "pressure",
        "precip_accum_1_hour",
        "precip_accum_3_hour",
        "precip_accum_6_hour",
        "precip_accum_24_hour",
        "precip_accum_since_local_midnight",
        "snow_depth",
        "cloud_cover",
        "visibility",
        "solar_radiation",
        "wet_bulb_temperature",
        "heat_index",
        "wind_chill",
    ];
    present.sort_by_key(|b| ORDER.iter().position(|o| o == b).unwrap_or(ORDER.len()));

    let mut rows = Vec::with_capacity(present.len());
    for base in present {
        let Some((raw, unit)) = value_of(st, base) else {
            continue;
        };
        let (value, unit) = convert_units(base, raw, &unit);
        let value = if base == "wind_direction" {
            format!("{} ({:.0}°)", super::map::compass16(value), value)
        } else if unit.is_empty() {
            format_number(value, decimals_for(base))
        } else {
            format!("{} {unit}", format_number(value, decimals_for(base)))
        };
        rows.push((label_for(base), value));
    }
    rows
}

fn decimals_for(base: &str) -> usize {
    match base {
        "relative_humidity" | "cloud_cover" | "solar_radiation" => 0,
        "air_temp"
        | "dew_point_temperature"
        | "wet_bulb_temperature"
        | "heat_index"
        | "wind_chill"
        | "wind_speed"
        | "wind_gust" => 1,
        "altimeter" | "sea_level_pressure" | "pressure" => 2,
        "precip_accum_1_hour"
        | "precip_accum_3_hour"
        | "precip_accum_6_hour"
        | "precip_accum_24_hour"
        | "precip_accum_since_local_midnight"
        | "snow_depth" => 2,
        "visibility" => 1,
        _ => 1,
    }
}

fn format_number(v: f64, decimals: usize) -> String {
    format!("{v:.decimals$}")
}

/// Convert a Synoptic value to the app's US units.
fn convert_units(base: &str, v: f64, unit: &str) -> (f64, String) {
    match unit {
        "Celsius" | "degrees C" => (v * 9.0 / 5.0 + 32.0, "°F".into()),
        "Fahrenheit" => (v, "°F".into()),
        "Pascals" => (v / 3386.389, "inHg".into()),
        "hectopascals" | "hPa" => (v * 0.029_529_983, "inHg".into()),
        "m/s" => (v * 2.236_936, "mph".into()),
        "km/h" => (v * 0.621_371, "mph".into()),
        "knots" => (v * 1.150_779, "mph".into()),
        "mph" => (v, "mph".into()),
        "mm" => (v / 25.4, "in".into()),
        "cm" => (v / 2.54, "in".into()),
        "inches" => (v, "in".into()),
        "meters" if base == "visibility" => (v / 1609.344, "mi".into()),
        "kilometers" if base == "visibility" => (v * 0.621_371, "mi".into()),
        "percent" | "%" => (v, "%".into()),
        "W/m^2" | "watts/m^2" => (v, "W/m²".into()),
        _ => (v, unit.to_string()),
    }
}

fn label_for(base: &str) -> String {
    let known = match base {
        "air_temp" => "Temperature",
        "dew_point_temperature" => "Dew point",
        "relative_humidity" => "Humidity",
        "wind_speed" => "Wind speed",
        "wind_gust" => "Wind gust",
        "wind_direction" => "Wind direction",
        "altimeter" => "Altimeter",
        "sea_level_pressure" => "Sea-level pressure",
        "pressure" => "Pressure",
        "precip_accum_1_hour" => "Precip (1 h)",
        "precip_accum_3_hour" => "Precip (3 h)",
        "precip_accum_6_hour" => "Precip (6 h)",
        "precip_accum_24_hour" => "Precip (24 h)",
        "precip_accum_since_local_midnight" => "Precip (since midnight)",
        "snow_depth" => "Snow depth",
        "cloud_cover" => "Cloud cover",
        "visibility" => "Visibility",
        "solar_radiation" => "Solar radiation",
        "wet_bulb_temperature" => "Wet bulb",
        "heat_index" => "Heat index",
        "wind_chill" => "Wind chill",
        _ => "",
    };
    if known.is_empty() {
        humanize(base)
    } else {
        known.to_string()
    }
}

/// `air_temp` → "Air temp", for variables without a hand-written label.
fn humanize(k: &str) -> String {
    let spaced = k.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Strip Synoptic's `_value_N` / `_set_N` suffix so `air_temp_value_1`
/// becomes `air_temp` (the key used by the station's `UNITS` map).
fn base_var(k: &str) -> &str {
    for suffix in ["_value_", "_set_"] {
        if let Some(i) = k.find(suffix) {
            return &k[..i];
        }
    }
    k
}
