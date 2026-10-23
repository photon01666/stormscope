//! Rain & snow totals: the window *and* the map overlay.
//!
//! Data is `radar_data::ForecastSource::station_totals` — NWS observation
//! *series* for stations inside the selected station's viewing area, giving
//! both the rolling buckets each station reports and an hourly-summed
//! accumulation over the loaded window (how an event total is reconstructed
//! from hourly observations without double-counting denser cadences).
//!
//! The overlay plots each station at its own location with its strongest
//! number; the window holds the full table. Both follow the layer
//! conventions: one-second hover dwell, click-through, sidebar toggle.

use crate::app::StormApp;
use radar_data::StationTotals;

/// Station marker color: a measurement blue that reads apart from the
/// report dots' event colors.
fn totals_color() -> egui::Color32 {
    egui::Color32::from_rgb(110, 205, 255)
}

/// How close (view px) the pointer must be to a station dot for hover/click.
const TOT_HIT_PX: f64 = 14.0;

/// The window (no-op while closed).
///
/// The request gate runs first and independently: the map overlay needs the
/// data even while the window stays shut.
pub fn totals_window(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    if app.state.show_totals || app.state.totals_open {
        // Keep the staleness timer alive while nothing else animates.
        ctx.request_repaint_after(std::time::Duration::from_secs(30));
        request_totals_if_stale(app);
    }
    if !app.state.totals_open {
        return;
    }
    // The window fills the pane; the floating map-controls popup would sit
    // on top of it (windows paint in call order) and hide the table.
    app.state.map_controls_open = false;

    let fill = cfg!(target_os = "android");
    let scale = super::big_text_scale(app);
    let screen = ctx.content_rect().size();
    let size = super::popup_size(ctx, scale, 560.0, 500.0);
    let max_h = super::popup_max_height(ctx, scale);
    // A phone is narrower than the 420 pt desktop minimum; clamping keeps the
    // window (and its table) on screen instead of running off the right edge.
    let min_size = egui::vec2(
        (420.0 * scale).min((screen.x - 24.0).max(200.0)),
        (220.0 * scale).min((screen.y - 90.0).max(160.0)),
    );
    let mut open = app.state.totals_open;
    let site_id = app.state.site_id.clone();

    let mut window = egui::Window::new("Rain & snow totals")
        .id(egui::Id::new("stormscope.totals"))
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
        // Same treatment as the AI and forecast windows: fill the map area
        // edge to edge, no dragging or resizing, with the phone type scale.
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
            close_requested |= super::phone_header(ui, "Rain & snow totals");
        }
        super::apply_text_scale(ui, scale);
        ui.label(
            egui::RichText::new(format!("NWS station observations around {site_id}"))
                .weak()
                .small(),
        );

        if app.state.totals.is_empty() {
            let msg = if app.state.totals_fetched_at.is_none() {
                "loading station totals…"
            } else {
                "no observation series available in this station's area"
            };
            ui.label(egui::RichText::new(msg).weak());
        } else {
            let scroll_h = (ui.available_height() - 34.0 * scale).max(120.0);
            egui::ScrollArea::vertical()
                .max_height(scroll_h)
                .auto_shrink([false, false])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                .show(ui, |ui| {
                    for t in &app.state.totals {
                        station_header(ui, t);
                        station_rows(ui, t);
                        ui.add_space(4.0 * scale);
                        ui.separator();
                    }
                });
        }
        ui.label(
            egui::RichText::new("source: api.weather.gov — NWS station observations (public data)")
                .weak()
                .small(),
        );
        if !fill {
            super::fill_popup_height(ui);
        }
    });
    app.state.totals_open = open && !close_requested;
}

/// Station name/id and when its series was observed.
fn station_header(ui: &mut egui::Ui, t: &StationTotals) {
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(&t.station_name).strong());
        ui.label(
            egui::RichText::new(if t.station_name == t.station_id {
                String::new()
            } else {
                t.station_id.clone()
            })
            .weak()
            .small(),
        );
    });
    ui.label(
        egui::RichText::new(match t.last_obs {
            Some(ts) => format!(
                "observed {} · {} observations loaded",
                crate::timefmt::fmt_central(&ts),
                t.obs_count
            ),
            None => "no observations loaded".to_string(),
        })
        .weak()
        .small(),
    );
}

/// The numbers: rolling buckets, then the event-facing hourly sums.
fn station_rows(ui: &mut egui::Ui, t: &StationTotals) {
    if t.buckets.is_empty() && t.hourly_sums.is_empty() {
        ui.label(
            egui::RichText::new("this station's series carries no rain or snow fields")
                .weak()
                .small(),
        );
    }
    for (label, inches) in &t.buckets {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(format!("{inches:.2} in")).monospace());
            });
        });
    }
    for (label, inches) in &t.hourly_sums {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{label} · {:.1} h window", t.window_hours)).strong(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!("{inches:.2} in"))
                        .monospace()
                        .strong(),
                );
            });
        });
    }
}

/// The station's strongest number — event sum or biggest bucket — plus
/// whether it is a frozen one (label carries snow/ice). This is what the
/// map tag shows; the window has the table.
fn headline(t: &StationTotals) -> Option<(f64, bool)> {
    t.buckets
        .iter()
        .map(|(label, v)| (*v, label.contains("snow") || label.contains("ice")))
        .chain(t.hourly_sums.iter().map(|(_, v)| (*v, false)))
        .reduce(|a, b| if b.0 > a.0 { b } else { a })
}

/// Station dots: diamonds (reports are circles), the strongest value tagged
/// alongside the station id, halo while hovered.
pub(super) fn draw_totals(
    painter: &egui::Painter,
    origin: egui::Pos2,
    view: &radar_render::MapView,
    app: &StormApp,
) {
    if !app.state.show_totals {
        return;
    }
    for t in &app.state.totals {
        let (Some(lat), Some(lon)) = (t.lat, t.lon) else {
            continue;
        };
        let (vx, vy) = view.lonlat_to_view_px(lon, lat);
        let at = egui::pos2(origin.x + vx as f32, origin.y + vy as f32);
        if !painter.clip_rect().contains(at) {
            continue;
        }
        let color = totals_color();
        let lit = app.state.totals_hover.as_deref() == Some(t.station_id.as_str());
        let r = if lit { 7.5 } else { 5.0 };
        if lit {
            painter.circle_filled(at, r + 5.0, color.gamma_multiply(0.22));
        }
        // Diamond: distinct at a glance from the report dots' circles.
        painter.add(egui::Shape::convex_polygon(
            vec![
                at - egui::vec2(0.0, r),
                at + egui::vec2(r, 0.0),
                at + egui::vec2(0.0, r),
                at - egui::vec2(r, 0.0),
            ],
            color,
            egui::Stroke::new(1.4, egui::Color32::from_black_alpha(210)),
        ));
        let label = match headline(t) {
            Some((v, frozen)) => format!(
                "{} {}{v:.2} in",
                t.station_id,
                if frozen { "❄ " } else { "" }
            ),
            None => t.station_id.clone(),
        };
        super::map::draw_tag_label(
            painter,
            at - egui::vec2(0.0, r + 5.0),
            egui::Align2::CENTER_BOTTOM,
            &label,
            egui::FontId::proportional(11.5),
            color,
        );
    }
}

/// Station dot under (view px): nearest within reach wins.
pub(super) fn totals_at<'a>(
    totals: &'a [StationTotals],
    view: &radar_render::MapView,
    cx: f64,
    cy: f64,
) -> Option<&'a StationTotals> {
    let mut best: Option<(&StationTotals, f64)> = None;
    for t in totals {
        let (Some(lat), Some(lon)) = (t.lat, t.lon) else {
            continue;
        };
        let (vx, vy) = view.lonlat_to_view_px(lon, lat);
        let d = (vx - cx).hypot(vy - cy);
        if d <= TOT_HIT_PX && best.map_or(true, |(_, bd)| d < bd) {
            best = Some((t, d));
        }
    }
    best.map(|(t, _)| t)
}

/// Brief tooltip for the station under the pointer: the same table the
/// window shows, then where the full window is.
pub(super) fn totals_hover_tooltip(ui: &egui::Ui, response: &egui::Response, app: &StormApp) {
    let Some(id) = app.state.totals_hover.as_deref() else {
        return;
    };
    if ui.input(|i| i.time) - app.state.totals_hover_at < 1.0 {
        return;
    }
    let Some(t) = app.state.totals.iter().find(|t| t.station_id == id) else {
        return;
    };
    egui::containers::Tooltip::for_widget(response)
        .at_pointer()
        .show(|ui| {
            super::apply_text_scale(ui, app.sidebar_scale.max(1.0));
            station_header(ui, t);
            station_rows(ui, t);
            ui.label(
                egui::RichText::new("click to open the totals window")
                    .weak()
                    .small(),
            );
        });
}

/// Send a totals request unless one already covers this target — with a
/// 60-second retry window, so a failed fetch retries on the next visit
/// instead of either sticking forever or hammering the API. The target is
/// always the *selected station's* location: totals belong to the viewing
/// area of the site on screen.
pub(super) fn request_totals_if_stale(app: &mut StormApp) {
    let Some(site) = radar_core::sites::site(&app.state.site_id) else {
        return;
    };
    let (lat, lon) = (site.latitude, site.longitude);
    if let Some(req) = app.state.totals_requested {
        let same = (req.0 - lat).abs() < 1e-6 && (req.1 - lon).abs() < 1e-6;
        if same && req.2.elapsed().as_secs_f64() < 60.0 {
            return; // in flight (or recently failed and due a retry)
        }
    }
    let age = app
        .state
        .totals_fetched_at
        .map(|t| t.elapsed().as_secs_f64());
    let target_matches = app
        .state
        .totals_target
        .is_some_and(|(tlat, tlon)| (tlat - lat).abs() < 1e-6 && (tlon - lon).abs() < 1e-6);
    // Same freshness rule the forecast window uses: fetch when nothing has
    // loaded, the target moved, or the loaded data aged past the cadence.
    if !super::forecast::forecast_needs_refresh(
        age,
        target_matches,
        app.state.totals_fetched_at.is_some(),
    ) {
        return;
    }
    app.state.totals_requested = Some((lat, lon, std::time::Instant::now()));
    let _ = app.tx.send(crate::app::DataCmd::LoadTotals(lat, lon));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn totals(id: &str, coords: Option<(f64, f64)>, buckets: Vec<(String, f64)>) -> StationTotals {
        StationTotals {
            station_id: id.into(),
            station_name: id.into(),
            lat: coords.map(|(la, _)| la),
            lon: coords.map(|(_, lo)| lo),
            last_obs: None,
            buckets,
            hourly_sums: Vec::new(),
            window_hours: 0.0,
            obs_count: 0,
        }
    }

    /// The map tag shows the strongest number: a snow depth of 1.2 in beats
    /// 0.3 of rain (and gets the flake), the event sum beats every bucket,
    /// and a field-less station shows just its id.
    #[test]
    fn headline_picks_the_strongest_value() {
        let mut t = totals(
            "KTOP",
            Some((39.0, -96.0)),
            vec![
                ("rain · last hour".into(), 0.10),
                ("snow depth".into(), 1.20),
            ],
        );
        assert_eq!(headline(&t), Some((1.20, true)), "snow wins, flagged");
        t.hourly_sums.push(("rain · hourly sum".into(), 2.50));
        assert_eq!(
            headline(&t),
            Some((2.50, false)),
            "the event sum beats the buckets"
        );
        let empty = totals("KMHK", Some((39.0, -96.0)), Vec::new());
        assert_eq!(headline(&empty), None, "no fields, no number");
    }

    /// Station dots hit-test nearest-within-reach; stations without
    /// coordinates (and beyond reach) are never hit.
    #[test]
    fn station_hit_test_is_nearest_within_range() {
        let view = radar_render::MapView {
            center_lat: 38.0,
            center_lon: -97.0,
            zoom: 7.0,
            width_px: 800.0,
            height_px: 600.0,
        };
        let near = totals("KN", Some((38.0, -97.0)), Vec::new());
        let far = totals("KF", Some((38.0, -96.0)), Vec::new());
        let far2 = totals("KF2", Some((38.5, -96.0)), Vec::new());
        let no_geo = totals("NG", None, Vec::new());
        let all = vec![near, far, far2, no_geo];
        let (cx, cy) = view.lonlat_to_view_px(-97.0, 38.0);
        assert!(totals_at(&all, &view, cx, cy).is_some_and(|t| t.station_id == "KN"));
        assert!(
            totals_at(&all, &view, cx + 40.0, cy).is_none(),
            "past reach"
        );
        let only_no_geo = vec![totals("NG", None, Vec::new())];
        let (ox, oy) = view.lonlat_to_view_px(0.0, 0.0);
        assert!(totals_at(&only_no_geo, &view, ox, oy).is_none());
    }

    /// Freshness follows the same shape as the forecast window: fetch when
    /// nothing is loaded, the target moved, or the data aged out.
    #[test]
    fn totals_refresh_needs_follows_target_and_clock() {
        assert!(super::super::forecast::forecast_needs_refresh(
            None, true, false
        ));
        assert!(!super::super::forecast::forecast_needs_refresh(
            Some(10.0),
            true,
            true
        ));
        assert!(super::super::forecast::forecast_needs_refresh(
            Some(super::super::forecast::FORECAST_REFRESH_SECS + 1.0),
            true,
            true
        ));
        assert!(super::super::forecast::forecast_needs_refresh(
            Some(1.0),
            false,
            true
        ));
    }
}
