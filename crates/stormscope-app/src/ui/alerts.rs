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

    let mut center_on: Option<(f64, f64)> = None;
    egui::Window::new("Alerts")
        .open(open)
        .default_size([600.0, 480.0])
        .min_width(420.0)
        .resizable(true)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(tab, AlertsTab::All, format!("All US ({})", all.len()));
                ui.selectable_value(tab, AlertsTab::Station, format!("{site_id} only ({})", station.len()));
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
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for w in rows.iter().take(MAX_ROWS) {
                        let dist = distance_km(w, site_lat, site_lon);
                        if alert_row(ui, w, warning_color(w), dist, site_id) {
                            center_on = w.centroid();
                        }
                    }
                });
        });

    if let Some((lat, lon)) = center_on {
        // Pan only — keep the user's zoom, they are hunting one warning.
        app.state.view.center_lat = lat;
        app.state.view.center_lon = lon;
    }
}

/// One clickable alert row. Returns true when clicked (caller centers the map).
fn alert_row(
    ui: &mut egui::Ui,
    w: &Warning,
    color: egui::Color32,
    distance_km: Option<f64>,
    site_id: &str,
) -> bool {
    let row_id = egui::Id::new(("stormscope.alert.row", &w.id));
    // Hover state comes from last frame's response, so the highlight can be
    // painted *behind* this frame's content.
    let hovered = ui.ctx().read_response(row_id).is_some_and(|r| r.hovered());
    let fill = if hovered {
        ui.visuals().widgets.hovered.bg_fill
    } else {
        ui.visuals().faint_bg_color
    };

    let has_polygon = w.polygon.is_some();
    let inner = egui::Frame::NONE
        .fill(fill)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(6, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(color, "●");
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

    let response = ui.interact(inner.response.rect, row_id, egui::Sense::click());
    if has_polygon {
        response
            .on_hover_text("Click to center the map on this alert")
            .clicked()
    } else {
        false
    }
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
