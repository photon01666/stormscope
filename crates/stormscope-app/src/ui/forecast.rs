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
    ctx.request_repaint_after(std::time::Duration::from_secs(30));
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
        window
            .fixed_rect(area)
            .movable(false)
            .resizable(false)
            .constrain_to(area)
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
                            let reserve =
                                ui.spacing().indent + ui.spacing().button_padding.x + 6.0 * scale;
                            let button_font = ui
                                .style()
                                .text_styles
                                .get(&egui::TextStyle::Button)
                                .cloned()
                                .unwrap_or(egui::FontId::proportional(14.0));
                            let fits = ui.ctx().fonts_mut(|f| {
                                f.layout_no_wrap(
                                    title_full.clone(),
                                    button_font,
                                    egui::Color32::WHITE,
                                )
                                .size()
                                .x
                            }) <= (ui.available_width() - reserve).max(0.0);
                            let title = if fits { title_full } else { title_short };
                            egui::CollapsingHeader::new(title)
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
    // Target changed: drop what is on screen rather than showing the old
    // point's numbers under the new point's label.
    if let Some((plat, plon)) = app.state.forecast_requested {
        if (plat - lat).abs() > 1e-6 || (plon - lon).abs() > 1e-6 {
            app.state.forecast = None;
            app.state.observation = None;
        }
    }
    log::debug!(
        "forecast refresh: {lat:.4}, {lon:.4} (loaded age {:?}s)",
        age.map(|a| a as i64)
    );
    app.state.forecast_requested = Some((lat, lon));
    app.state.forecast_site = match picked {
        Some(_) => "pinned".to_string(),
        None => app.state.site_id.clone(),
    };
    let _ = app.tx.send(crate::app::DataCmd::LoadForecast(lat, lon));
}

/// One line of "72°F · Light Rain · dew 61°F · …" for the current conditions,
/// converted to the US units the rest of the app uses.
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

/// Polygon of the lit portion of a moon disc. The terminator is an ellipse
/// whose horizontal radius shrinks to zero at quarter moons; the whole shape
/// is mirrored for the waning half of the cycle.
fn lit_moon_points(c: egui::Pos2, r: f32, phase: f64) -> Vec<egui::Pos2> {
    const N: usize = 32;
    let (frac, mirror) = if phase < 0.5 {
        (phase, false)
    } else {
        (phase - 0.5, true)
    };
    let rx = (2.0 * std::f64::consts::PI * frac).cos();
    let mut points = Vec::with_capacity(2 * N + 2);
    for i in 0..=N {
        let a = -std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * (i as f64 / N as f64);
        points.push(egui::pos2(
            c.x + r * a.cos() as f32,
            c.y + r * a.sin() as f32,
        ));
    }
    for i in 0..=N {
        let a = std::f64::consts::FRAC_PI_2 - std::f64::consts::PI * (i as f64 / N as f64);
        let x = rx * a.cos();
        points.push(egui::pos2(c.x + r * x as f32, c.y + r * a.sin() as f32));
    }
    if mirror {
        for p in &mut points {
            p.x = 2.0 * c.x - p.x;
        }
    }
    points
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
    painter.circle_filled(c, r, egui::Color32::from_gray(40));
    let lit = lit_moon_points(c, r, phase);
    painter.add(egui::Shape::Path(egui::epaint::PathShape::closed_line(
        lit,
        egui::epaint::PathStroke::new(0.0, egui::Color32::from_gray(230)),
    )));
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

    let mut parts: Vec<String> = Vec::new();
    match f(o.temperature_c) {
        Some(t) => {
            let shown = ui.animate_value_with_time(egui::Id::new("cc-temp"), t, 0.6);
            parts.push(format!("Air {shown:.0}°F"));
        }
        None => parts.push("Air —°F".into()),
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
        let shown = ui.animate_value_with_time(egui::Id::new("cc-wind"), mph, 0.6);
        parts.push(format!("wind{dir} {shown:.0} mph{gust}"));
    }
    if let Some(p) = o.pressure_pa {
        let shown = ui.animate_value_with_time(egui::Id::new("cc-alt"), p / 3386.389, 0.6);
        parts.push(format!("altimeter {shown:.2} inHg"));
    }
    if let Some(slp) = o.sea_level_pressure_pa {
        parts.push(format!("sea-level {:.2} inHg", slp / 3386.389));
    }
    if let Some(v) = o.visibility_m {
        parts.push(format!("vis {:.1} mi", v / 1609.344));
    }
    if let Some(mm) = o.precip_last_hour_mm {
        if mm > 0.0 {
            parts.push(format!("{:.2} in/hr", mm / 25.4));
        }
    }
    if let Some(mm) = o.precip_3h_mm {
        if mm > 0.0 {
            parts.push(format!("{:.2} in/3h", mm / 25.4));
        }
    }
    if let Some(mm) = o.precip_6h_mm {
        if mm > 0.0 {
            parts.push(format!("{:.2} in/6h", mm / 25.4));
        }
    }
    if !o.sky.is_empty() {
        parts.push(o.sky.clone());
    }
    if !o.cloud_bases_ft.is_empty() {
        let bases: Vec<String> = o
            .cloud_bases_ft
            .iter()
            .map(|b| format!("{b:.0} ft"))
            .collect();
        parts.push(format!("bases {}", bases.join(", ")));
    }
    if let Some(pct) = o.cloud_cover_pct {
        parts.push(format!("cover {pct:.0}%"));
    }
    if let Some(hi) = f(o.heat_index_c) {
        parts.push(format!("heat index {hi:.0}°F"));
    }
    if let Some(wc) = f(o.wind_chill_c) {
        parts.push(format!("wind chill {wc:.0}°F"));
    }
    if let Some(uv) = uv_index {
        let shown = ui.animate_value_with_time(egui::Id::new("cc-uv"), uv, 0.6);
        parts.push(format!("UV {:.1} ({})", shown, uv_word(shown)));
    }
    ui.label(egui::RichText::new(parts.join(" · ")));

    // Moon phase: a real phase glyph with a slow glow, next to its name.
    ui.horizontal(|ui| {
        moon_ui(ui, &ctx);
        ui.label(
            egui::RichText::new(
                "Moon phase updates from the current time; UV index from Open-Meteo.",
            )
            .weak()
            .small(),
        );
    });

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
                "observed {} ({age_text}) · source api.weather.gov · auto-refreshes every 15 min",
                crate::timefmt::fmt_central(&ts)
            ))
            .weak()
            .small(),
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
