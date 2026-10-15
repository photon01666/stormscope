//! Alerts browser: a popup listing active NWS alerts.
//!
//! Two views: every active alert nationwide, or only those whose polygon sits
//! near the selected radar site (the same radius the map's alert fetch uses).
//! Clicking a row centers the map on that alert.

use crate::app::StormApp;
use crate::state::{AlertsTab, STATION_ALERT_RADIUS_KM};
use radar_core::warnings::Warning;

/// How many rows to render at most (a nationwide list can run to hundreds;
/// the filter box narrows it down).
const MAX_ROWS: usize = 200;

/// Map/popup color for an alert by event type (matches the polygon colors).
pub(super) fn warning_color(w: &Warning) -> egui::Color32 {
    if w.is_tornado() {
        egui::Color32::from_rgb(255, 60, 120)
    } else if w.is_severe_thunderstorm() {
        egui::Color32::from_rgb(255, 220, 60)
    } else if w.is_flash_flood() {
        egui::Color32::from_rgb(80, 255, 120)
    } else {
        egui::Color32::from_white_alpha(140)
    }
}

/// Alerts whose polygon lies within `radius_km` of (site_lat, site_lon).
/// Zone-only alerts have no geometry to place, so they are excluded.
pub(super) fn station_alerts<'a>(
    alerts: &'a [Warning],
    site_lat: f64,
    site_lon: f64,
    radius_km: f64,
) -> Vec<&'a Warning> {
    alerts
        .iter()
        .filter(|w| {
            w.centroid().is_some_and(|(lat, lon)| {
                radar_render::surface_distance_m(site_lon, site_lat, lon, lat) <= radius_km * 1000.0
            })
        })
        .collect()
}

/// Distance from the site to an alert's centroid, km.
fn distance_km(w: &Warning, site_lat: f64, site_lon: f64) -> Option<f64> {
    w.centroid()
        .map(|(lat, lon)| radar_render::surface_distance_m(site_lon, site_lat, lon, lat) / 1000.0)
}

/// Case-insensitive match over the fields a person would search by.
fn matches_filter(w: &Warning, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let needle = needle.to_lowercase();
    [&w.event, &w.area, &w.office, &w.headline, &w.severity]
        .iter()
        .any(|f| f.to_lowercase().contains(&needle))
}

/// Priority for the watch/warning families the app flashes for, most severe
/// first. `None` means "do not flash": that covers event names outside the
/// storm families the map itself colors (river `Flood Warning`, marine and
/// wind warnings, freeze/red-flag products, advisories, statements) as well
/// as anything that is neither a watch nor a warning.
///
/// Deliberately event-name based rather than severity/urgency based: NWS
/// watches and warnings are the two families people act on, and their names
/// say which they are.
fn significant_priority(w: &Warning) -> Option<u8> {
    let is_watch = w.event.contains("Watch");
    let is_warning = w.event.contains("Warning");
    if !is_watch && !is_warning {
        return None;
    }
    // The three storm families the radar overlay colors. A plain river
    // "Flood Warning" is not one of them: it is not radar-driven, and
    // flashing for it looked like a bug (the badge pulsing white).
    let family = if w.is_tornado() {
        3
    } else if w.is_severe_thunderstorm() {
        2
    } else if w.is_flash_flood() {
        1
    } else {
        return None;
    };
    // Total order, no ties: warnings always outrank watches.
    let class = if is_warning { 4 } else { 2 };
    Some(class * 2 + family)
}

/// Color the top-of-map alerts badge flashes for a watch/warning: the family
/// color (matching the map polygons), dimmed for watches so a watch reads as
/// less urgent than the corresponding warning. Only the three storm families
/// reach this (see `significant_priority`); the neutral fallback exists so
/// the helper stays total.
pub(super) fn pulse_color(w: &Warning) -> egui::Color32 {
    let base = if w.is_tornado() {
        egui::Color32::from_rgb(255, 60, 120)
    } else if w.is_severe_thunderstorm() {
        egui::Color32::from_rgb(255, 220, 60)
    } else if w.is_flash_flood() {
        egui::Color32::from_rgb(80, 255, 120)
    } else {
        egui::Color32::from_rgb(255, 255, 255)
    };
    if w.event.contains("Watch") {
        base.gamma_multiply(0.72)
    } else {
        base
    }
}

/// Station to switch to when the user jumps to an alert, if any.
///
/// An alert beyond the current station's radius has no radar and no polygon
/// on the map (frames and the near-site alert set are per station), so
/// "take me there" means "load the station that actually covers it". `None`
/// when the current station is already the right one — including the case of
/// an alert beyond every station's radius, where switching would gain
/// nothing.
pub(super) fn station_for_alert<'a>(
    alert: &Warning,
    current_site: &str,
    radius_km: f64,
) -> Option<&'static radar_core::sites::RadarSite> {
    let (lat, lon) = alert.centroid()?;
    let current = radar_core::sites::site(current_site)?;
    let dist_current =
        radar_core::sites::haversine_km(lat, lon, current.latitude, current.longitude);
    if dist_current <= radius_km {
        return None; // already covered by the selected station
    }
    // Only switch to a station that would actually cover the alert: one
    // beyond every station's radius (offshore, or outside the network) has no
    // radar to show, so a reload of "the nearest site" would gain nothing.
    let nearest = radar_core::sites::nearest(lat, lon)?;
    let dist_nearest =
        radar_core::sites::haversine_km(lat, lon, nearest.latitude, nearest.longitude);
    (nearest.id != current.id && dist_nearest <= radius_km).then_some(nearest)
}

/// The most significant watch-or-greater alert near (lat, lon), if any — what
/// the top-of-map badge flashes for.
pub(super) fn significant_near<'a>(
    alerts: &'a [Warning],
    lat: f64,
    lon: f64,
    radius_km: f64,
) -> Option<&'a Warning> {
    alerts
        .iter()
        .filter(|w| {
            w.centroid().is_some_and(|(alat, alon)| {
                radar_render::surface_distance_m(lon, lat, alon, alat) <= radius_km * 1000.0
            })
        })
        .filter_map(|w| significant_priority(w).map(|p| (p, w)))
        .max_by_key(|(p, _)| *p)
        .map(|(_, w)| w)
}

/// Ray-casting point-in-polygon test against an exterior ring. NWS warning
/// polygons are single-ring in practice, so holes are ignored.
pub(super) fn ring_contains(ring: &[(f64, f64)], lon: f64, lat: f64) -> bool {
    if ring.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let (xi, yi) = ring[i];
        let (xj, yj) = ring[j];
        if (yi > lat) != (yj > lat) && lon < (xj - xi) * (lat - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// The warning polygon under a map point, if any. Later entries win: they are
/// drawn last, so they are visually on top.
pub(super) fn warning_at(alerts: &[Warning], lon: f64, lat: f64) -> Option<&Warning> {
    alerts.iter().rev().find(|w| {
        w.polygon
            .as_ref()
            .is_some_and(|rings| rings.first().is_some_and(|r| ring_contains(r, lon, lat)))
    })
}

fn site_of(app: &StormApp) -> (f64, f64) {
    radar_core::sites::site(&app.state.site_id)
        .map(|s| (s.latitude, s.longitude))
        .unwrap_or((35.3331, -97.2778))
}

/// The popup window itself (no-op while closed).
pub fn alerts_popup(ctx: &egui::Context, app: &mut StormApp) {
    if !app.state.alerts_popup_open {
        return;
    }
    let (site_lat, site_lon) = site_of(app);

    // Split borrows: the window mutates open/filter/tab while reading the
    // alert list, and nothing here needs to clone it.
    let state = &mut app.state;
    let open = &mut state.alerts_popup_open;
    let filter = &mut state.alerts_filter;
    let tab = &mut state.alerts_tab;
    let site_id = &state.site_id;
    let all = &state.all_alerts;
    let station: Vec<&Warning> = station_alerts(all, site_lat, site_lon, STATION_ALERT_RADIUS_KM);

    // Match the sidebar's high-res scaling: on a 4K panel the window is
    // otherwise unreadable. The size is clamped to the screen so a long
    // alert list cannot push the window off the bottom edge.
    let scale = app.sidebar_scale.max(1.0);
    let size = super::popup_size(ctx, scale, 620.0, 520.0);

    let mut center_on: Option<(f64, f64)> = None;
    let mut hover: Option<String> = None;
    let mut clicked: Option<String> = None;
    let mut detail: Option<String> = None;
    let mut switch_to: Option<String> = None;
    egui::Window::new("Alerts")
        .open(open)
        .default_size(size)
        .max_height(size.y)
        .min_width(420.0 * scale)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .resizable(true)
        .show(ctx, |ui| {
            super::apply_text_scale(ui, scale);
            ui.horizontal(|ui| {
                ui.selectable_value(tab, AlertsTab::All, format!("All US ({})", all.len()));
                ui.selectable_value(
                    tab,
                    AlertsTab::Station,
                    format!("{site_id} only ({})", station.len()),
                );
            });
            ui.label(
                egui::RichText::new(format!(
                    "source api.weather.gov · refreshed every 5 min · the station tab covers alerts within {} km of the site",
                    STATION_ALERT_RADIUS_KM as i64
                ))
                .weak()
                .small(),
            );
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Filter");
                ui.add(
                    egui::TextEdit::singleline(filter)
                        .hint_text("event, area, office, headline…")
                        .desired_width(f32::INFINITY),
                );
            });
            ui.separator();

            let source: Vec<&Warning> = match *tab {
                AlertsTab::All => all.iter().collect(),
                AlertsTab::Station => station.clone(),
            };
            let rows: Vec<&Warning> = source
                .into_iter()
                .filter(|w| matches_filter(w, filter))
                .collect();

            if rows.is_empty() {
                ui.label(
                    egui::RichText::new(if station.is_empty() && *tab == AlertsTab::Station {
                        "No alerts near this site."
                    } else {
                        "Nothing matches."
                    })
                    .weak(),
                );
                return;
            }
            ui.label(
                egui::RichText::new(if rows.len() > MAX_ROWS {
                    format!(
                        "showing {} of {} — narrow with the filter",
                        MAX_ROWS,
                        rows.len()
                    )
                } else {
                    format!("{} alerts", rows.len())
                })
                .weak()
                .small(),
            );

            egui::ScrollArea::vertical()
                .max_height((size.y - 230.0 * scale).max(120.0))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for w in rows.iter().take(MAX_ROWS) {
                        let dist = distance_km(w, site_lat, site_lon);
                        let (is_hovered, is_clicked, wants_details) = alert_row(
                            ui,
                            w,
                            warning_color(w),
                            dist,
                            site_id,
                            site_lat,
                            site_lon,
                            scale,
                        );
                        if is_hovered {
                            hover = Some(w.id.clone());
                        }
                        if is_clicked {
                            clicked = Some(w.id.clone());
                            center_on = w.centroid();
                            // Far from the current station: "take me there"
                            // only works if the station that covers it is
                            // loaded (radar frames and the near-site alert
                            // set are per station).
                            if let Some(site) = station_for_alert(
                                w,
                                site_id,
                                STATION_ALERT_RADIUS_KM,
                            ) {
                                switch_to = Some(site.id.to_string());
                            }
                        }
                        if wants_details {
                            detail = Some(w.id.clone());
                        }
                    }
                });
        });

    if let Some((lat, lon)) = center_on {
        // Pan only — keep the user's zoom, they are hunting one warning.
        app.state.view.center_lat = lat;
        app.state.view.center_lon = lon;
    }
    app.state.alerts_hover = hover;
    if let Some(id) = clicked {
        // Focus + timestamp: the map flashes this polygon briefly and keeps a
        // steady highlight afterwards, so the clicked row is identifiable.
        app.state.alerts_focus = Some(id);
        app.state.alerts_focus_at = ctx.input(|i| i.time);
    }
    if let Some(id) = detail {
        app.state.alert_detail = Some(id);
    }
    if let Some(site_id) = switch_to {
        // Loading a station takes a few seconds (its frames do not exist
        // yet); zoom in if the view is too far out for a warning-sized area,
        // so the alert is actually visible once the data lands.
        if app.state.view.zoom < 8.5 {
            app.state.view.zoom = 8.5;
        }
        app.select_site(&site_id);
    }
}

/// The NOAA detail popup: the full CAP text for one alert. Opened by the
/// browser's "Details" button or by clicking a warning polygon on the map.
pub fn alert_detail_popup(ctx: &egui::Context, app: &mut StormApp) {
    let Some(id) = app.state.alert_detail.clone() else {
        return;
    };
    // Near-site alerts are drawn on the map; nationwide-only alerts (zone
    // alerts, alerts far away) still have text worth reading.
    let found = app
        .state
        .alerts
        .iter()
        .find(|w| w.id == id)
        .or_else(|| app.state.all_alerts.iter().find(|w| w.id == id))
        .cloned();
    let Some(w) = found else {
        app.state.alert_detail = None;
        return;
    };

    let scale = app.sidebar_scale.max(1.0);
    let size = super::popup_size(ctx, scale, 620.0, 500.0);
    let mut open = true;
    egui::Window::new("NOAA alert details")
        .id(egui::Id::new("stormscope.alert.detail"))
        .open(&mut open)
        .default_size(size)
        .max_height(size.y)
        .min_width(420.0 * scale)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .resizable(true)
        .show(ctx, |ui| {
            super::apply_text_scale(ui, scale);
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(
                    warning_color(&w),
                    egui::RichText::new("●").size(12.0 * scale),
                );
                ui.heading(&w.event);
            });
            ui.horizontal_wrapped(|ui| {
                if !w.office.is_empty() {
                    ui.label(egui::RichText::new(format!("issued by NWS {}", w.office)).weak());
                }
                if !w.severity.is_empty() || !w.urgency.is_empty() {
                    ui.label(
                        egui::RichText::new(format!("{} / {}", w.severity, w.urgency)).weak(),
                    );
                }
                ui.label(
                    egui::RichText::new(format!(
                        "in effect until {}",
                        crate::timefmt::fmt_central(&w.expires)
                    ))
                    .weak(),
                );
            });
            if !w.area.is_empty() {
                ui.label(egui::RichText::new(&w.area).weak());
            }
            ui.separator();

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .max_height((size.y - 260.0 * scale).max(120.0))
                .show(ui, |ui| {
                    if !w.instruction.is_empty() {
                        egui::Frame::NONE
                            .fill(ui.visuals().extreme_bg_color)
                            .corner_radius(4.0)
                            .inner_margin(egui::Margin::same(8))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new("NOAA instruction")
                                        .strong()
                                        .small(),
                                );
                                ui.label(egui::RichText::new(&w.instruction));
                            });
                        ui.add_space(6.0);
                    }
                    if w.description.is_empty() {
                        ui.label(egui::RichText::new(&w.headline).weak());
                        ui.label(
                            egui::RichText::new(
                                "No extended NOAA text for this alert; the headline above is all the API returned.",
                            )
                            .weak()
                            .small(),
                        );
                    } else {
                        for para in w.description.split('\n') {
                            if para.trim().is_empty() {
                                ui.add_space(4.0);
                            } else {
                                ui.label(para);
                            }
                        }
                    }
                });
        });

    if !open {
        app.state.alert_detail = None;
    }
}

/// The first sentence of a text blob, truncated for a tooltip.
pub(super) fn first_sentence(text: &str, max_chars: usize) -> String {
    let t = text.trim();
    if t.is_empty() {
        return String::new();
    }
    let end = t.find(['.', '!', '?', '\n']).map_or(t.len(), |i| i + 1);
    let sentence = t[..end].trim();
    if sentence.chars().count() > max_chars {
        let cut: String = sentence.chars().take(max_chars).collect();
        format!("{cut}…")
    } else {
        sentence.to_string()
    }
}

/// Tooltip contents: what this alert *is*, in a glance — event, who issued
/// it, how long it runs, where, and the opening line of the NOAA text.
pub(super) fn brief_alert_ui(ui: &mut egui::Ui, w: &Warning, site_lat: f64, site_lon: f64) {
    ui.horizontal(|ui| {
        ui.colored_label(warning_color(w), "●");
        ui.strong(&w.event);
    });
    let mut meta = Vec::new();
    if !w.office.is_empty() {
        meta.push(format!("NWS {}", w.office));
    }
    if !w.severity.is_empty() {
        meta.push(w.severity.clone());
    }
    meta.push(format!("until {}", crate::timefmt::fmt_central(&w.expires)));
    if let Some(d) = distance_km(w, site_lat, site_lon) {
        meta.push(format!("{d:.0} km away"));
    }
    ui.label(egui::RichText::new(meta.join(" · ")).weak().small());
    if !w.area.is_empty() {
        ui.label(egui::RichText::new(&w.area).weak().small());
    }
    let gist = first_sentence(&w.description, 150);
    if !gist.is_empty() {
        ui.label(egui::RichText::new(gist).small());
    } else if !w.headline.is_empty() {
        ui.label(egui::RichText::new(&w.headline).small());
    }
}

/// The clickable parts of an alert row: everything except the rectangle of
/// the Details button (left strip and area underneath it).
pub(super) fn row_regions(row: egui::Rect, button: egui::Rect) -> (egui::Rect, egui::Rect) {
    let left = egui::Rect::from_min_max(row.min, egui::pos2(button.min.x - 2.0, row.max.y));
    let below = egui::Rect::from_min_max(egui::pos2(row.min.x, button.max.y + 1.0), row.max);
    (left, below)
}

/// One clickable alert row. Returns `(hovered, clicked, details)` — the
/// caller mirrors hover into the map glow, jumps the map on click, and opens
/// the NOAA detail popup for `details`.
fn alert_row(
    ui: &mut egui::Ui,
    w: &Warning,
    color: egui::Color32,
    distance_km: Option<f64>,
    site_id: &str,
    site_latitude: f64,
    site_longitude: f64,
    scale: f32,
) -> (bool, bool, bool) {
    let mut details = false;
    let row_id = egui::Id::new(("stormscope.alert.row", &w.id));
    let row_left_id = row_id.with("left");
    let row_below_id = row_id.with("below");
    // Hover state comes from last frame's responses, so the highlight can be
    // painted *behind* this frame's content.
    let hovered = ui
        .ctx()
        .read_response(row_left_id)
        .is_some_and(|r| r.hovered())
        || ui
            .ctx()
            .read_response(row_below_id)
            .is_some_and(|r| r.hovered());
    let fill = if hovered {
        ui.visuals().widgets.hovered.bg_fill
    } else {
        ui.visuals().faint_bg_color
    };

    let has_polygon = w.polygon.is_some();
    let mut details_rect: Option<egui::Rect> = None;
    let inner = egui::Frame::NONE
        .fill(fill)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(6, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // The severity dot doubles as a map-color legend; on a 4K
                // panel it needs to keep pace with the scaled text.
                ui.colored_label(color, egui::RichText::new("●").size(11.0 * scale));
                ui.strong(&w.event);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "until {}",
                            crate::timefmt::fmt_central(&w.expires)
                        ))
                        .weak()
                        .small(),
                    );
                    let btn = ui
                        .small_button("Details")
                        .on_hover_text("Show the full NOAA text for this alert");
                    if btn.clicked() {
                        details = true;
                    }
                    details_rect = Some(btn.rect);
                });
            });
            ui.horizontal_wrapped(|ui| {
                if !w.office.is_empty() {
                    ui.label(
                        egui::RichText::new(format!("WFO {}", w.office))
                            .weak()
                            .small(),
                    );
                }
                if !w.severity.is_empty() {
                    ui.label(egui::RichText::new(&w.severity).weak().small());
                }
                if let Some(d) = distance_km {
                    ui.label(
                        egui::RichText::new(format!("· {d:.0} km from {site_id}"))
                            .weak()
                            .small(),
                    );
                }
                if !has_polygon {
                    ui.label(
                        egui::RichText::new("· zone alert (no polygon)")
                            .weak()
                            .small(),
                    );
                }
            });
            if !w.area.is_empty() {
                ui.label(egui::RichText::new(&w.area).weak().small());
            }
            if !w.headline.is_empty() && !w.headline.eq_ignore_ascii_case(&w.event) {
                ui.label(egui::RichText::new(&w.headline).small());
            }
        });
    ui.add_space(2.0);

    // Clickable row area: everything except the Details button. Registering
    // the whole frame instead would sit above the button in egui's hit order
    // and swallow its clicks (the button "did nothing").
    let row = inner.response.rect;
    let btn = details_rect.unwrap_or(egui::Rect::NOTHING);
    let (left, below) = row_regions(row, btn);
    let mut hovered = false;
    let mut raw_clicked = false;
    for (region, id) in [(left, row_left_id), (below, row_below_id)] {
        if region.width() <= 0.0 || region.height() <= 0.0 {
            continue;
        }
        let r = ui.interact(region, id, egui::Sense::click());
        hovered |= r.hovered();
        raw_clicked |= r.clicked();
        if has_polygon {
            r.on_hover_ui(|ui| {
                // Tooltip layers do not inherit per-panel scaling.
                super::apply_text_scale(ui, scale);
                brief_alert_ui(ui, w, site_latitude, site_longitude);
                ui.label(
                    egui::RichText::new("click to center the map on this alert")
                        .weak()
                        .small(),
                );
            });
        }
    }
    // A click on the Details button must not also count as a row click.
    (hovered, raw_clicked && !details, details)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn warn(id: &str, event: &str, lat: f64, lon: f64, with_polygon: bool) -> Warning {
        let polygon = with_polygon.then(|| {
            vec![vec![
                (lon - 0.1, lat - 0.1),
                (lon + 0.1, lat - 0.1),
                (lon + 0.1, lat + 0.1),
                (lon - 0.1, lat + 0.1),
            ]]
        });
        Warning {
            id: id.into(),
            event: event.into(),
            severity: "Severe".into(),
            urgency: "Expected".into(),
            expires: Utc::now(),
            office: "OUN".into(),
            headline: format!("{event} for somewhere"),
            description: format!("At 7:00 PM, a {event} was located near Newcastle."),
            instruction: "TAKE COVER NOW.".into(),
            area: "Cleveland, OK".into(),
            polygon,
        }
    }

    /// The station tab is a geographic filter: only alerts whose polygon
    /// centroid is within the radius of the site.
    #[test]
    fn station_filter_keeps_nearby_polygons_only() {
        let site = (35.3331, -97.2778); // KTLX
        let alerts = vec![
            warn("near", "Tornado Warning", 35.4, -97.3, true),
            warn("edge", "Severe Thunderstorm Warning", 32.0, -97.3, true), // ~370 km
            warn("far", "Flash Flood Warning", 40.0, -97.3, true),          // ~520 km
            warn("zone", "Special Weather Statement", 35.4, -97.3, false),  // no polygon
        ];
        let got = station_alerts(&alerts, site.0, site.1, STATION_ALERT_RADIUS_KM);
        let ids: Vec<&str> = got.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["near", "edge"],
            "zone-only alerts cannot be placed"
        );
    }

    /// The filter box matches event, area, office and headline,
    /// case-insensitively.
    #[test]
    fn filter_matches_the_useful_fields() {
        let w = warn("a", "Tornado Warning", 35.0, -97.0, true);
        assert!(matches_filter(&w, ""));
        assert!(matches_filter(&w, "tornado"));
        assert!(matches_filter(&w, "CLEVELAND"));
        assert!(matches_filter(&w, "oun"));
        assert!(matches_filter(&w, "somewhere"));
        assert!(!matches_filter(&w, "flood"));
    }

    /// Jumping to an alert from another station's area must load the station
    /// that covers it, or the map shows neither the alert nor radar data.
    #[test]
    fn jumping_to_a_far_alert_picks_the_covering_station() {
        // A Denver alert is ~700 km from KTWX (outside its radius); the
        // station that watches it is the Front Range one.
        let denver = (39.7, -104.99);
        let d = warn("d", "Severe Thunderstorm Warning", denver.0, denver.1, true);
        let site = station_for_alert(&d, "KTWX", STATION_ALERT_RADIUS_KM)
            .expect("far alert must switch stations");
        assert_ne!(site.id, "KTWX");
        assert_eq!(
            site.id,
            radar_core::sites::nearest(denver.0, denver.1).unwrap().id,
            "switch to the nearest site"
        );
        // That station covers the alert (otherwise the switch is pointless).
        assert!(
            radar_core::sites::haversine_km(denver.0, denver.1, site.latitude, site.longitude)
                <= STATION_ALERT_RADIUS_KM
        );

        // Kansas City is ~130 km from Topeka: same coverage, no switch.
        let kc = warn("kc", "Severe Thunderstorm Warning", 39.1, -94.58, true);
        assert!(station_for_alert(&kc, "KTWX", STATION_ALERT_RADIUS_KM).is_none());

        // An alert already covered by the selected station: no switch. (Near
        // Topeka itself — Oklahoma City, at ~400 km, is right at the edge.)
        let local = warn("local", "Tornado Warning", 39.2, -96.0, true);
        assert!(station_for_alert(&local, "KTWX", STATION_ALERT_RADIUS_KM).is_none());

        // Far outside the network (mid-Pacific): no station covers it, so a
        // reload would gain nothing — pan only.
        let ocean = warn("ocean", "Marine Warning", 20.0, -150.0, true);
        assert!(station_for_alert(&ocean, "KTWX", STATION_ALERT_RADIUS_KM).is_none());

        // Zone alerts cannot be located at all.
        let zone = warn("zone", "Heat Advisory", 39.1, -94.58, false);
        assert!(station_for_alert(&zone, "KTWX", STATION_ALERT_RADIUS_KM).is_none());
    }

    /// The brief tooltip text is the opening sentence, trimmed to fit.
    #[test]
    fn brief_description_uses_the_opening_sentence() {
        assert_eq!(
            first_sentence(
                "At 7:00 PM, a tornado was located near Newcastle. Moving NE.",
                150
            ),
            "At 7:00 PM, a tornado was located near Newcastle."
        );
        // No terminator: the whole (trimmed) text.
        assert_eq!(first_sentence("  Short notice  ", 150), "Short notice");
        // Long sentences are cut on a char boundary with an ellipsis.
        let long = "A".repeat(400);
        let cut = first_sentence(&long, 150);
        assert_eq!(cut.chars().count(), 151); // 150 + the ellipsis
        assert!(cut.ends_with('…'));
        // Empty text stays empty (the caller falls back to the headline).
        assert!(first_sentence("   ", 150).is_empty());
        // Multi-byte safety: cutting "°C" style text must not panic.
        let utf8 = format!("{}°°°", "x".repeat(148));
        assert!(first_sentence(&utf8, 150).chars().count() <= 151);
    }

    /// The row's clickable regions must cover the row except the Details
    /// button (that is what makes the button's own clicks land).
    #[test]
    fn row_regions_exclude_the_details_button() {
        let row = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(400.0, 60.0));
        let btn = egui::Rect::from_min_size(egui::pos2(340.0, 24.0), egui::vec2(64.0, 20.0));
        let (left, below) = row_regions(row, btn);
        assert!(
            !left.intersects(btn),
            "left strip must not cover the button"
        );
        assert!(
            !below.intersects(btn),
            "below strip must not cover the button"
        );
        // A click on the far side of the row still hits a region.
        assert!(left.contains(egui::pos2(200.0, 40.0)));
        assert!(below.contains(egui::pos2(360.0, 70.0)));
    }

    /// Map-click hit testing: a click inside a polygon finds its alert, the
    /// topmost (last drawn) one wins, and zone-only alerts are never hit.
    #[test]
    fn warning_hit_testing_on_the_map() {
        let a = warn("a", "Tornado Warning", 35.4, -97.3, true); // +/-0.1 deg box
        let b = warn("b", "Flash Flood Warning", 35.4, -97.3, true);
        assert_eq!(warning_at(&[a.clone()], -97.3, 35.4).unwrap().id, "a");
        assert!(
            warning_at(&[a.clone()], -97.0, 35.6).is_none(),
            "outside the box"
        );
        assert!(
            warning_at(&[a.clone(), b], -97.3, 35.4).unwrap().id == "b",
            "last drawn polygon is on top"
        );
        let zone = warn("z", "Heat Advisory", 35.4, -97.3, false);
        assert!(
            warning_at(&[zone], -97.3, 35.4).is_none(),
            "no polygon, no hit"
        );
    }

    /// Alerts badge: only watches/warnings count, warnings outrank watches,
    /// and advisories never flash anything.
    #[test]
    fn significant_alerts_rank_warnings_over_watches() {
        let p = |event: &str| significant_priority(&warn("x", event, 35.4, -97.3, true));
        assert!(p("Tornado Warning").unwrap() > p("Severe Thunderstorm Warning").unwrap());
        assert!(
            p("Severe Thunderstorm Warning").unwrap() > p("Tornado Watch").unwrap(),
            "an active warning outranks any watch"
        );
        assert!(p("Tornado Watch").unwrap() > p("Flash Flood Watch").unwrap());
        assert!(p("Wind Advisory").is_none());
        assert!(p("Special Weather Statement").is_none());
        // Not storm families: no flash, so the badge never pulses white.
        assert!(
            p("Flood Warning").is_none(),
            "river flooding is not a storm warning"
        );
        assert!(p("Flood Watch").is_none());
        assert!(p("High Wind Warning").is_none());
        assert!(p("Freeze Warning").is_none());
        assert!(p("Special Marine Warning").is_none());
        assert!(p("Red Flag Warning").is_none());
        // The storm families do flash, watch or warning.
        assert!(p("Flash Flood Warning").is_some());
        assert!(p("Flash Flood Watch").is_some());
        assert!(p("Tornado Watch").is_some());
        assert!(
            p("Tornado Emergency").is_none(),
            "no Watch/Warning in the name"
        );

        // The nearest significant alert wins; advisories and far alerts do not.
        let site = (35.3331, -97.2778);
        let alerts = vec![
            warn("adv", "Wind Advisory", 35.4, -97.3, true),
            warn("watch", "Tornado Watch", 35.4, -97.3, true),
            warn("warn", "Severe Thunderstorm Warning", 35.5, -97.4, true),
            warn("far", "Tornado Warning", 45.0, -97.3, true),
        ];
        let got = significant_near(&alerts, site.0, site.1, STATION_ALERT_RADIUS_KM).unwrap();
        assert_eq!(got.id, "warn", "most significant near-station alert");
        // With no significant alert nearby: nothing to flash.
        assert!(significant_near(&alerts[..1], site.0, site.1, STATION_ALERT_RADIUS_KM).is_none());
        assert!(significant_near(&alerts[3..], site.0, site.1, 100.0).is_none());
    }

    /// Watches flash in the family color but dimmed, so they read as less
    /// urgent than the matching warning.
    #[test]
    fn watch_pulse_is_dimmed_family_color() {
        let w = warn("w", "Severe Thunderstorm Warning", 35.4, -97.3, true);
        let watch = warn("v", "Severe Thunderstorm Watch", 35.4, -97.3, true);
        assert_eq!(pulse_color(&w), egui::Color32::from_rgb(255, 220, 60));
        assert_ne!(pulse_color(&watch), pulse_color(&w));
        assert!(pulse_color(&watch).r() < pulse_color(&w).r());
    }

    /// Alert colors follow the event family, matching the map polygons.
    #[test]
    fn alert_colors_follow_event_family() {
        assert_eq!(
            warning_color(&warn("t", "Tornado Warning", 35.0, -97.0, true)),
            egui::Color32::from_rgb(255, 60, 120)
        );
        assert_eq!(
            warning_color(&warn("s", "Severe Thunderstorm Warning", 35.0, -97.0, true)),
            egui::Color32::from_rgb(255, 220, 60)
        );
        assert_eq!(
            warning_color(&warn("f", "Flash Flood Warning", 35.0, -97.0, true)),
            egui::Color32::from_rgb(80, 255, 120)
        );
    }
}
