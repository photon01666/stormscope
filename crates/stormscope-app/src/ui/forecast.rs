//! NWS forecast window: the selected station's area by default, or a point
//! picked on the map.
//!
//! Forecasts come from `api.weather.gov`'s gridpoint endpoints (see
//! `radar_data::ForecastSource`); this module is only the presentation and
//! the station-vs-point bookkeeping.

use crate::app::StormApp;

/// How long the forecast and current conditions stay fresh before the window
/// refetches them. The NWS API is open data with unpublished "generous"
/// limits and asks clients to be reasonable; four small requests every 15
/// minutes while the window is open is far inside that. Upstream, gridpoint
/// forecasts update several times an hour and ASOS observations report about
/// hourly, so this cadence keeps up without hammering.
pub(super) const FORECAST_REFRESH_SECS: f64 = 15.0 * 60.0;

/// Should the data on screen be refetched? Either the target moved (station
/// switch or a new picked point) or what is loaded has aged past the cadence.
pub(super) fn forecast_needs_refresh(
    loaded_age_secs: Option<f64>,
    target_matches: bool,
    has_data: bool,
) -> bool {
    if !has_data || !target_matches {
        return true;
    }
    loaded_age_secs.is_none_or(|age| age >= FORECAST_REFRESH_SECS)
}

/// The window (no-op while closed).
pub fn forecast_window(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    if !app.state.forecast_open {
        return;
    }
    // Refetch when the station changed, when the point was repicked, or when
    // what is on screen has aged past the cadence — and keep a slow repaint
    // coming so that timer can fire while nothing else animates.
    // Fast repaint: the row icons (rain, sun rays, drifting clouds) and the
    // moon glow are time-animated, and egui only redraws on request.
    ctx.request_repaint_after(std::time::Duration::from_millis(150));
    request_forecast_if_stale(app);

    let fill = cfg!(target_os = "android");
    let scale = super::big_text_scale(app);
    let screen = ctx.content_rect().size();
    let size = super::popup_size(ctx, scale, 580.0, 520.0);
    let max_h = super::popup_max_height(ctx, scale);
    // A phone is narrower than the 420 pt desktop minimum: clamping keeps the
    // window (and the text inside it) on screen instead of running off the
    // right edge.
    let min_size = egui::vec2(
        (420.0 * scale).min((screen.x - 24.0).max(200.0)),
        (220.0 * scale).min((screen.y - 90.0).max(160.0)),
    );
    let site_id = app.state.site_id.clone();
    let mut open = app.state.forecast_open;
    let mut refetch = false;
    let mut pick_from_map = false;
    let mut clear_point = false;

    let mut window = egui::Window::new("NWS forecast")
        .id(egui::Id::new("stormscope.forecast"))
        // Centered *by its top-left corner*, not by a pivot: egui's pivot
        // is for anchored areas, and with a center pivot every size change
        // also shifts the stored position — which fights the resize drag.
        .default_pos(super::centered_popup_pos(ctx, size))
        .max_width((screen.x - 16.0).max(200.0))
        .min_size(min_size);
    #[cfg(target_os = "android")]
    if fill {
        window = window.title_bar(false);
    }
    window = if fill {
        // Same treatment as the AI window: fill the map area edge to edge,
        // no dragging or resizing, with the roomier phone type scale.
        // A few points of inset so the phone header isn't flush with (and
        // read as cut by) the pane's top edge.
        let inset = area.shrink2(egui::vec2(4.0, 8.0));
        window
            .fixed_rect(inset)
            .movable(false)
            .resizable(false)
            .constrain_to(inset)
    } else {
        window.default_size(size).max_height(max_h).resizable(true)
    };
    let window = window.open(&mut open);
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut close_requested = false;
    window.show(ctx, |ui| {
        #[cfg(target_os = "android")]
        if fill {
            close_requested |= super::phone_header(ui, "NWS forecast");
        }
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
                let source = if app.state.forecast_point_gps {
                    "GPS"
                } else {
                    "pinned at"
                };
                ui.label(
                    egui::RichText::new(format!("· {source} {lat:.3}, {lon:.3}"))
                        .weak()
                        .small(),
                );
            }
            if ui
                .small_button("Refresh now")
                .on_hover_text("Fetch the latest forecast and observation")
                .clicked()
            {
                app.state.forecast_requested = None;
                app.state.forecast_fetched_at = None;
                refetch = true;
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
            Some(o) => current_conditions_ui(ui, o, app.state.uv_index),
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
                                egui::RichText::new("The API returned no periods for this point.")
                                    .weak(),
                            );
                            return;
                        }
                        for (i, p) in fc.periods.iter().enumerate() {
                            let icon = weather_icon_kind(&p.short, p.is_daytime);
                            ui.horizontal(|ui| {
                                let (icon_rect, _) = ui.allocate_exact_size(
                                    egui::vec2(34.0, 34.0),
                                    egui::Sense::hover(),
                                );
                                draw_weather_icon(ui.ctx(), ui.painter(), icon_rect, icon);
                                let temp = if p.temperature.is_finite() {
                                    let shown = ui.animate_value_with_time(
                                        egui::Id::new(("fc-temp", i)),
                                        p.temperature,
                                        0.5,
                                    );
                                    format!("{shown:.0}°{}", p.unit)
                                } else {
                                    "—".to_string()
                                };
                                let subtitle = if p.wind.is_empty() {
                                    p.short.clone()
                                } else if p.short.is_empty() {
                                    p.wind.clone()
                                } else {
                                    format!("{} · {}", p.short, p.wind)
                                };
                                let title_short = format!("{} — {temp}", p.name);
                                let title_full = if subtitle.is_empty() {
                                    title_short.clone()
                                } else {
                                    format!("{title_short} · {subtitle}")
                                };
                                // CollapsingHeader draws its title on one line
                                // with TextWrapMode::Extend: it does not wrap,
                                // it just runs past the window and gets clipped
                                // ("…Showers · S 5 mj" on a phone). Keep the
                                // full line only when it actually fits; otherwise
                                // use the short title and move the tail into the
                                // body, which does wrap.
                                // Room for the collapse arrow plus the
                                // scrollbar: measuring only the arrow let
                                // long titles run under the window edge on a
                                // phone ("S 5 mj").
                                let reserve = ui.spacing().indent
                                    + ui.spacing().button_padding.x
                                    + 26.0 * scale;
                                let mut button_font = ui
                                    .style()
                                    .text_styles
                                    .get(&egui::TextStyle::Button)
                                    .cloned()
                                    .unwrap_or(egui::FontId::proportional(14.0));
                                // Bigger forecast titles: measured and drawn
                                // with this same font so the fit check cannot
                                // disagree with what ends up on screen.
                                button_font.size = (button_font.size * 1.2).min(30.0);
                                let fits = ui.ctx().fonts_mut(|f| {
                                    f.layout_no_wrap(
                                        title_full.clone(),
                                        button_font.clone(),
                                        egui::Color32::WHITE,
                                    )
                                    .size()
                                    .x
                                }) <= (ui.available_width() - reserve).max(0.0);
                                let title = if fits { title_full } else { title_short };
                                egui::CollapsingHeader::new(
                                    egui::RichText::new(title).font(button_font),
                                )
                                .id_salt(("forecast.period", i))
                                .default_open(i == 0)
                                .show(ui, |ui| {
                                    if !fits && !subtitle.is_empty() {
                                        ui.label(egui::RichText::new(&subtitle).weak());
                                        ui.add_space(2.0 * scale);
                                    }
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
                            });
                        }
                    });
            }
        }
        super::fill_popup_height(ui);
    });

    if pick_from_map {
        // Arm the pick and get out of the way: on a phone the window fills
        // the map, and even on desktop the target is easier to hit with the
        // map exposed. The map click reopens it and requests that point.
        app.state.forecast_pick_mode = true;
        app.state.forecast_reopen_after_pick = true;
        open = false;
    }
    if close_requested {
        open = false;
    }
    app.state.forecast_open = open;
    if clear_point {
        app.state.forecast_point = None;
        app.state.forecast_point_gps = false;
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
    // A loaded forecast for this target counts as fresh — unless it has aged
    // past the refresh cadence.
    let age = app
        .state
        .forecast_fetched_at
        .map(|t| t.elapsed().as_secs_f64());
    if !forecast_needs_refresh(
        age,
        picked.is_some() || app.state.forecast_site == app.state.site_id,
        app.state.forecast.is_some(),
    ) {
        return;
    }
    request_forecast_now(app);
}

/// Unconditional refresh of forecast + current conditions + UV for the
/// current target. The AI calls this before answering when its cached
/// surface observation has gone stale.
pub(crate) fn request_forecast_now(app: &mut StormApp) {
    let picked = app.state.forecast_point;
    let (lat, lon) = match picked {
        Some(p) => p,
        None => match radar_core::sites::site(&app.state.site_id) {
            Some(s) => (s.latitude, s.longitude),
            None => return,
        },
    };
    // Target changed: drop what is on screen rather than showing the old
    // point's numbers under the new point's label.
    if let Some((plat, plon)) = app.state.forecast_requested {
        if (plat - lat).abs() > 1e-6 || (plon - lon).abs() > 1e-6 {
            app.state.forecast = None;
            app.state.observation = None;
        }
    }
    log::debug!("forecast refresh (forced for AI): {lat:.4}, {lon:.4}");
    app.state.forecast_requested = Some((lat, lon));
    app.state.forecast_site = match picked {
        Some(_) => "pinned".to_string(),
        None => app.state.site_id.clone(),
    };
    let _ = app.tx.send(crate::app::DataCmd::LoadForecast(lat, lon));
}

/// One line of "72°F · Light Rain · dew 61°F · …" for the current conditions,
/// converted to the US units the rest of the app uses.
/// Animated weather glyph for a forecast row, keyed off the NWS summary.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum WeatherIcon {
    Sunny,
    ClearNight,
    PartlyDay,
    PartlyNight,
    MostlyDay,
    Cloudy,
    Rain,
    Showers,
    Thunder,
    Snow,
    Fog,
}

fn weather_icon_kind(short: &str, is_daytime: bool) -> WeatherIcon {
    let s = short.to_lowercase();
    if s.contains("thunder") || s.contains("tstm") {
        WeatherIcon::Thunder
    } else if s.contains("snow")
        || s.contains("sleet")
        || s.contains("ice")
        || s.contains("blizzard")
        || s.contains("flurr")
    {
        WeatherIcon::Snow
    } else if s.contains("rain") || s.contains("shower") || s.contains("drizzle") {
        if s.contains("chance") || s.contains("slight") || s.contains("scattered") {
            WeatherIcon::Showers
        } else {
            WeatherIcon::Rain
        }
    } else if s.contains("fog") || s.contains("haze") || s.contains("smoke") || s.contains("mist") {
        WeatherIcon::Fog
    } else if s.contains("mostly cloudy") || s.contains("overcast") || s.contains("cloudy") {
        WeatherIcon::Cloudy
    } else if s.contains("partly cloudy") || s.contains("partly sunny") {
        if is_daytime {
            WeatherIcon::PartlyDay
        } else {
            WeatherIcon::PartlyNight
        }
    } else if s.contains("mostly clear") || s.contains("mostly sunny") {
        if is_daytime {
            WeatherIcon::MostlyDay
        } else {
            WeatherIcon::PartlyNight
        }
    } else if s.contains("sunny") || s.contains("clear") {
        if is_daytime {
            WeatherIcon::Sunny
        } else {
            WeatherIcon::ClearNight
        }
    } else if is_daytime {
        WeatherIcon::Sunny
    } else {
        WeatherIcon::ClearNight
    }
}

const SUN: egui::Color32 = egui::Color32::from_rgb(255, 196, 60);
const MOON: egui::Color32 = egui::Color32::from_rgb(222, 228, 245);
const CLOUD: egui::Color32 = egui::Color32::from_rgb(150, 156, 168);
const RAIN: egui::Color32 = egui::Color32::from_rgb(95, 165, 255);
const SNOW: egui::Color32 = egui::Color32::from_rgb(235, 240, 255);
const BOLT: egui::Color32 = egui::Color32::from_rgb(255, 220, 90);

/// Puffy cloud: three lobes over a rounded base, centred in `rect`.
fn draw_cloud(painter: &egui::Painter, rect: egui::Rect, dx: f32, color: egui::Color32) {
    let c = rect.center() + egui::vec2(dx, 1.0);
    let s = rect.width() / 26.0;
    painter.circle_filled(c + egui::vec2(-5.0 * s, 1.0 * s), 5.0 * s, color);
    painter.circle_filled(c + egui::vec2(0.0, -2.0 * s), 6.5 * s, color);
    painter.circle_filled(c + egui::vec2(5.5 * s, 1.5 * s), 4.5 * s, color);
    let base =
        egui::Rect::from_center_size(c + egui::vec2(0.0, 4.0 * s), egui::vec2(18.0 * s, 8.0 * s));
    painter.rect_filled(base, 4.0 * s, color);
}

/// One animated forecast glyph: sun/moon, clouds, rain, snow, bolt or mist.
pub(super) fn draw_weather_icon(
    ctx: &egui::Context,
    painter: &egui::Painter,
    rect: egui::Rect,
    kind: WeatherIcon,
) {
    let t = ctx.input(|i| i.time) as f32;
    let c = rect.center();
    let r = rect.width() / 2.0;
    // Interior offsets below are authored for a 26 pt box; k rescales them
    // when the icon grows.
    let k = r / 13.0;

    match kind {
        WeatherIcon::Sunny | WeatherIcon::ClearNight => {
            if kind == WeatherIcon::Sunny {
                let glow = 0.5 + 0.5 * (t * 1.2).sin();
                painter.circle_filled(
                    c,
                    r * 0.75 + 3.0,
                    egui::Color32::from_rgba_unmultiplied(255, 196, 60, (18.0 + 16.0 * glow) as u8),
                );
                painter.circle_filled(c, r * 0.55, SUN);
                for i in 0..8 {
                    let a = t * 0.5 + i as f32 * std::f32::consts::FRAC_PI_4;
                    let (sa, ca) = a.sin_cos();
                    let p1 = c + egui::vec2(ca * r * 0.7, sa * r * 0.7);
                    let p2 = c + egui::vec2(ca * r * 0.95, sa * r * 0.95);
                    painter.line_segment([p1, p2], egui::Stroke::new(1.6, SUN));
                }
            } else {
                draw_moon(
                    painter,
                    c,
                    r * 0.8,
                    0.72,
                    egui::Color32::from_gray(45),
                    MOON,
                );
            }
        }
        WeatherIcon::PartlyDay | WeatherIcon::PartlyNight | WeatherIcon::MostlyDay => {
            let sun_side = kind == WeatherIcon::PartlyDay || kind == WeatherIcon::MostlyDay;
            let big_cloud = kind == WeatherIcon::MostlyDay;
            if sun_side {
                let sc = c + egui::vec2(-4.0, -5.0);
                let glow = 0.5 + 0.5 * (t * 1.2).sin();
                painter.circle_filled(
                    sc,
                    6.0,
                    egui::Color32::from_rgba_unmultiplied(255, 196, 60, (30.0 + 30.0 * glow) as u8),
                );
                painter.circle_filled(sc, 4.2, SUN);
                for i in 0..6 {
                    let a = t * 0.4 + i as f32 * std::f32::consts::FRAC_PI_3;
                    let (sa, ca) = a.sin_cos();
                    painter.line_segment(
                        [
                            sc + egui::vec2(ca * 5.4, sa * 5.4),
                            sc + egui::vec2(ca * 7.0, sa * 7.0),
                        ],
                        egui::Stroke::new(1.3, SUN),
                    );
                }
            } else {
                let mc = c + egui::vec2(-5.0 * k, -5.0 * k);
                draw_moon(
                    painter,
                    mc,
                    5.0 * k,
                    0.72,
                    egui::Color32::from_gray(45),
                    MOON,
                );
            }
            let drift = (t * 0.6).sin() * 1.5;
            draw_cloud(
                painter,
                egui::Rect::from_center_size(
                    c + egui::vec2(3.0, 3.0),
                    egui::vec2(
                        rect.width() * if big_cloud { 1.1 } else { 1.0 },
                        rect.height(),
                    ),
                ),
                drift,
                CLOUD,
            );
        }
        WeatherIcon::Cloudy => {
            let drift = (t * 0.4).sin() * 2.0;
            draw_cloud(painter, rect, drift - 2.0, egui::Color32::from_gray(105));
            draw_cloud(painter, rect, drift + 2.5, CLOUD);
        }
        WeatherIcon::Rain | WeatherIcon::Showers | WeatherIcon::Thunder => {
            let drift = (t * 0.5).sin() * 1.5;
            draw_cloud(painter, rect, drift, CLOUD);
            if kind == WeatherIcon::Thunder {
                let flick = 0.4 + 0.6 * ((t * 6.0).sin() * 0.5 + 0.5);
                let bolt = [
                    c + egui::vec2(-1.0 * k, 4.0 * k),
                    c + egui::vec2(2.5 * k, 4.0 * k),
                    c + egui::vec2(0.0, 9.5 * k),
                    c + egui::vec2(3.0 * k, 9.5 * k),
                    c + egui::vec2(-2.5 * k, 16.0 * k),
                    c + egui::vec2(-0.5 * k, 10.5 * k),
                    c + egui::vec2(-3.5 * k, 10.5 * k),
                ];
                painter.add(egui::Shape::convex_polygon(
                    bolt.to_vec(),
                    egui::Color32::from_rgba_unmultiplied(
                        BOLT.r(),
                        BOLT.g(),
                        BOLT.b(),
                        (255.0 * flick) as u8,
                    ),
                    egui::Stroke::NONE,
                ));
            }
            let drops = if kind == WeatherIcon::Showers { 2 } else { 3 };
            for i in 0..drops {
                let y = (4.0 + ((t * 34.0 + i as f32 * 14.0) % 14.0)) * k;
                let x = c.x - 5.0 * k + i as f32 * 5.0 * k;
                painter.line_segment(
                    [
                        egui::pos2(x, c.y + y),
                        egui::pos2(x - 1.5 * k, c.y + y + 3.5 * k),
                    ],
                    egui::Stroke::new(1.8, RAIN),
                );
            }
        }
        WeatherIcon::Snow => {
            let drift = (t * 0.4).sin() * 1.5;
            draw_cloud(painter, rect, drift, CLOUD);
            for i in 0..3 {
                let y = (4.0 + ((t * 12.0 + i as f32 * 13.0) % 13.0)) * k;
                let x = (c.x - 5.0 * k + i as f32 * 5.0 * k) + (t * 1.5 + i as f32).sin() * 1.5 * k;
                let p = egui::pos2(x, c.y + y);
                painter.line_segment(
                    [p + egui::vec2(-2.2 * k, 0.0), p + egui::vec2(2.2 * k, 0.0)],
                    egui::Stroke::new(1.4, SNOW),
                );
                painter.line_segment(
                    [p + egui::vec2(0.0, -2.2 * k), p + egui::vec2(0.0, 2.2 * k)],
                    egui::Stroke::new(1.4, SNOW),
                );
            }
        }
        WeatherIcon::Fog => {
            let drift = (t * 0.3).sin() * 1.2;
            draw_cloud(painter, rect, drift, CLOUD);
            for i in 0..3 {
                let slide = (((t * 9.0 + i as f32 * 5.0) % 12.0) - 6.0) * k;
                let y = c.y + (5.0 + i as f32 * 3.5) * k;
                painter.line_segment(
                    [
                        egui::pos2(c.x - 7.0 * k + slide, y),
                        egui::pos2(c.x + 7.0 * k + slide, y),
                    ],
                    egui::Stroke::new(1.8, egui::Color32::from_gray(150)),
                );
            }
        }
    }
}

fn uv_word(uv: f32) -> &'static str {
    match uv {
        0.0..=2.0 => "Low",
        3.0..=5.0 => "Moderate",
        6.0..=7.0 => "High",
        8.0..=10.0 => "Very high",
        _ => "Extreme",
    }
}

/// Lunar phase from the current instant: 0.0 = new, 0.5 = full.
fn moon_phase(now: chrono::DateTime<chrono::Utc>) -> f64 {
    let jd = now.timestamp() as f64 / 86_400.0 + 2_440_587.5;
    ((jd - 2_451_550.097_66) / 29.530_588_853).rem_euclid(1.0)
}

fn moon_illumination(phase: f64) -> f64 {
    (1.0 - (2.0 * std::f64::consts::PI * phase).cos()) / 2.0
}

fn moon_name(phase: f64) -> &'static str {
    match (phase * 8.0).round() as u32 % 8 {
        0 => "New moon",
        1 => "Waxing crescent",
        2 => "First quarter",
        3 => "Waxing gibbous",
        4 => "Full moon",
        5 => "Waning gibbous",
        6 => "Last quarter",
        _ => "Waning crescent",
    }
}

/// Phase-correct moon built from a dark base, a clipped lit half and the
/// terminator ellipse — all primitives epaint fills reliably. (A filled lune
/// polygon renders as an invisible stroke, which made the old glyph a dark
/// blob with stray dots.)
///
/// `phase`: 0.0 = new, 0.25 = first quarter, 0.5 = full, 0.75 = last quarter.
fn draw_moon(
    painter: &egui::Painter,
    c: egui::Pos2,
    r: f32,
    phase: f64,
    dark: egui::Color32,
    lit: egui::Color32,
) {
    let rx = r * (2.0 * std::f64::consts::PI * phase).cos() as f32;
    let pad = 2.0;
    let waxing = phase < 0.5;
    // The lit half first, then the terminator lobe on the appropriate side.
    let (lit_clip, other_clip) = if waxing {
        (
            egui::Rect::from_min_max(
                egui::pos2(c.x, c.y - r - pad),
                egui::pos2(c.x + r + pad, c.y + r + pad),
            ),
            egui::Rect::from_min_max(
                egui::pos2(c.x - r - pad, c.y - r - pad),
                egui::pos2(c.x, c.y + r + pad),
            ),
        )
    } else {
        (
            egui::Rect::from_min_max(
                egui::pos2(c.x - r - pad, c.y - r - pad),
                egui::pos2(c.x, c.y + r + pad),
            ),
            egui::Rect::from_min_max(
                egui::pos2(c.x, c.y - r - pad),
                egui::pos2(c.x + r + pad, c.y + r + pad),
            ),
        )
    };

    painter.circle_filled(c, r, dark);
    painter.with_clip_rect(lit_clip).circle_filled(c, r, lit);
    if rx >= 0.0 {
        // Crescent/quarter: the terminator bulges into the lit half; carve it
        // back out with the dark side of the terminator ellipse.
        painter.with_clip_rect(lit_clip).add(egui::Shape::Ellipse(
            egui::epaint::EllipseShape::filled(c, egui::vec2(rx, r), dark),
        ));
    } else {
        // Gibbous: the lit region spills past center over the dark half.
        painter.with_clip_rect(other_clip).add(egui::Shape::Ellipse(
            egui::epaint::EllipseShape::filled(c, egui::vec2(-rx, r), lit),
        ));
    }

    // Faint craters, only when the disc is mostly lit and on the lit side.
    if moon_illumination(phase) > 0.85 {
        let s = if waxing { 1.0 } else { -1.0 };
        for (dx, dy, cr) in [(0.25, -0.2, 0.13), (0.45, 0.3, 0.09), (0.1, 0.4, 0.07)] {
            painter.circle_filled(
                c + egui::vec2(s * dx * r, dy * r),
                cr * r,
                egui::Color32::from_gray(200),
            );
        }
    }
}

/// Animated moon-phase glyph: the lit portion is computed from the real
/// phase, and a slow glow pulse keeps it feeling live.
fn moon_ui(ui: &mut egui::Ui, ctx: &egui::Context) {
    let phase = moon_phase(chrono::Utc::now());
    let (rect, _) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::hover());
    let c = rect.center();
    let r = rect.width() / 2.0 - 2.0;
    let painter = ui.painter();
    let pulse = 0.5 + 0.5 * (ctx.input(|i| i.time) * 0.8).sin();
    painter.circle_filled(
        c,
        r + 5.0,
        egui::Color32::from_rgba_unmultiplied(200, 205, 255, (8.0 + 24.0 * pulse) as u8),
    );
    draw_moon(
        painter,
        c,
        r,
        phase,
        egui::Color32::from_gray(40),
        egui::Color32::from_gray(230),
    );
    // A thin rim keeps the disc crisp against the window fill.
    painter.circle_stroke(c, r, egui::Stroke::new(1.0, egui::Color32::from_gray(110)));
    let illum = moon_illumination(phase) * 100.0;
    ui.label(egui::RichText::new(format!("{} · {:.0}% lit", moon_name(phase), illum)).small());
}

fn current_conditions_ui(
    ui: &mut egui::Ui,
    o: &radar_core::forecast::Observation,
    uv_index: Option<f32>,
) {
    fn f(c: Option<f32>) -> Option<f32> {
        c.map(|v| v * 9.0 / 5.0 + 32.0)
    }
    let ctx = ui.ctx().clone();
    let station = if o.station_name.is_empty() {
        o.station_id.clone()
    } else {
        format!("{} · {}", o.station_id, o.station_name)
    };
    ui.horizontal_wrapped(|ui| {
        ui.strong("Current conditions");
        // A softly pulsing "live" dot marks fresh data.
        let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
        let pulse = 0.5 + 0.5 * (ctx.input(|i| i.time) * 2.0).sin();
        ui.painter().circle_filled(
            rect.center(),
            4.5,
            egui::Color32::from_rgba_unmultiplied(90, 220, 120, (110.0 + 110.0 * pulse) as u8),
        );
        ui.label(egui::RichText::new(station).weak().small());
    });

    // Large headline: temperature and the NWS condition word. The details are
    // a label/value list below so the eye can scan them instead of parsing one
    // long dot-separated line.
    let temp =
        f(o.temperature_c).map(|t| ui.animate_value_with_time(egui::Id::new("cc-temp"), t, 0.6));
    ui.horizontal_wrapped(|ui| {
        match temp {
            Some(t) => {
                ui.label(egui::RichText::new(format!("{t:.0}°F")).size(34.0).strong());
            }
            None => {
                ui.label(egui::RichText::new("—°F").size(34.0).strong().weak());
            }
        }
        if !o.text.is_empty() {
            ui.label(egui::RichText::new(&o.text).size(20.0));
        }
    });
    ui.add_space(2.0);

    let wind_shown = o
        .wind_speed_kmh
        .map(|sp| ui.animate_value_with_time(egui::Id::new("cc-wind"), sp * 0.621371, 0.6));
    let alt_shown = o
        .pressure_pa
        .map(|p| ui.animate_value_with_time(egui::Id::new("cc-alt"), p / 3386.389, 0.6));
    let uv_shown = uv_index.map(|uv| ui.animate_value_with_time(egui::Id::new("cc-uv"), uv, 0.6));

    const DETAIL: f32 = 18.0;
    const LABEL: f32 = 15.0;
    egui::Grid::new("stormscope.forecast.current-conditions")
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            let mut row = |label: &str, value: String| {
                ui.label(egui::RichText::new(label).weak().size(LABEL));
                ui.label(egui::RichText::new(value).strong().size(DETAIL));
                ui.end_row();
            };
            if let Some(d) = f(o.dewpoint_c) {
                row("Dew point", format!("{d:.0}°F"));
            }
            if let Some(h) = o.humidity_pct {
                row("Humidity", format!("{h:.0}%"));
            }
            if let (Some(_), Some(shown)) = (o.wind_speed_kmh, wind_shown) {
                let dir = o
                    .wind_direction_deg
                    .map(|d| format!("{} ", super::map::compass16(d as f64)))
                    .unwrap_or_default();
                let gust = o
                    .wind_gust_kmh
                    .map(|g| format!(" · gust {:.0}", g * 0.621371))
                    .unwrap_or_default();
                row("Wind", format!("{dir}{shown:.0} mph{gust}"));
            }
            if let (Some(_), Some(shown)) = (o.pressure_pa, alt_shown) {
                row("Altimeter", format!("{shown:.2} inHg"));
            }
            if let Some(slp) = o.sea_level_pressure_pa {
                row("Sea-level", format!("{:.2} inHg", slp / 3386.389));
            }
            if let Some(v) = o.visibility_m {
                row("Visibility", format!("{:.1} mi", v / 1609.344));
            }
            let mut precip: Vec<String> = Vec::new();
            if let Some(mm) = o.precip_last_hour_mm
                && mm > 0.0
            {
                precip.push(format!("{:.2} in/hr", mm / 25.4));
            }
            if let Some(mm) = o.precip_3h_mm
                && mm > 0.0
            {
                precip.push(format!("{:.2} in/3h", mm / 25.4));
            }
            if let Some(mm) = o.precip_6h_mm
                && mm > 0.0
            {
                precip.push(format!("{:.2} in/6h", mm / 25.4));
            }
            if !precip.is_empty() {
                row("Precipitation", precip.join(" · "));
            }
            let mut sky: Vec<String> = Vec::new();
            if !o.sky.is_empty() {
                sky.push(o.sky.clone());
            }
            if !o.cloud_bases_ft.is_empty() {
                let bases: Vec<String> = o
                    .cloud_bases_ft
                    .iter()
                    .map(|b| format!("{b:.0} ft"))
                    .collect();
                sky.push(format!("bases {}", bases.join(", ")));
            }
            if let Some(pct) = o.cloud_cover_pct {
                sky.push(format!("cover {pct:.0}%"));
            }
            if !sky.is_empty() {
                row("Sky", sky.join(" · "));
            }
            if let Some(hi) = f(o.heat_index_c) {
                row("Heat index", format!("{hi:.0}°F"));
            }
            if let Some(wc) = f(o.wind_chill_c) {
                row("Wind chill", format!("{wc:.0}°F"));
            }
            if let (Some(_), Some(shown)) = (uv_index, uv_shown) {
                row("UV index", format!("{:.1} ({})", shown, uv_word(shown)));
            }
        });

    // Moon phase: a real phase glyph with a slow glow, next to its name.
    ui.horizontal(|ui| {
        moon_ui(ui, &ctx);
    });
    ui.add(
        egui::Label::new(
            egui::RichText::new("Moon phase from the device clock · UV index from Open-Meteo")
                .weak()
                .small(),
        )
        .wrap(),
    );

    if let Some(ts) = o.timestamp {
        let age = chrono::Utc::now() - ts;
        let mins = age.num_minutes().max(0);
        let age_text = if mins < 60 {
            format!("{mins} min ago")
        } else {
            format!("{} h {} min ago", mins / 60, mins % 60)
        };
        ui.add(
            egui::Label::new(
                egui::RichText::new(format!(
                    "observed {} ({age_text}) · source api.weather.gov · auto-refreshes every 15 min",
                    crate::timefmt::fmt_central(&ts)
                ))
                .weak()
                .small(),
            )
            .wrap(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moon_phase_tracks_new_and_full_moons() {
        // 2000-01-06 18:14 UTC was a new moon; 2000-01-21 was full.
        let new = chrono::DateTime::parse_from_rfc3339("2000-01-06T18:14:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let full = chrono::DateTime::parse_from_rfc3339("2000-01-21T04:40:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let np = moon_phase(new);
        let fp = moon_phase(full);
        assert!(np < 0.03 || np > 0.97, "new moon phase {np}");
        assert!((fp - 0.5).abs() < 0.05, "full moon phase {fp}");
        assert_eq!(moon_name(np), "New moon");
        assert_eq!(moon_name(fp), "Full moon");
    }

    /// The cadence rule: refetch when the target moved, when nothing is
    /// loaded, or when the data aged past 15 minutes — not otherwise (the
    /// window asks on every frame).
    #[test]
    fn refresh_cadence_follows_the_target_and_the_clock() {
        assert!(forecast_needs_refresh(None, true, false), "nothing loaded");
        assert!(
            forecast_needs_refresh(Some(1.0), false, true),
            "target moved"
        );
        assert!(!forecast_needs_refresh(Some(60.0), true, true), "fresh");
        assert!(!forecast_needs_refresh(Some(14.0 * 60.0), true, true));
        assert!(
            forecast_needs_refresh(Some(FORECAST_REFRESH_SECS), true, true),
            "cadence elapsed"
        );
        assert!(forecast_needs_refresh(Some(40.0 * 60.0), true, true));
        assert!(
            forecast_needs_refresh(None, true, true),
            "no stamp counts as stale"
        );
    }
}
