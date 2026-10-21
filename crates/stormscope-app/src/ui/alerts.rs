//! Alerts browser: a popup listing active NWS alerts.
//!
//! Two views: every active alert nationwide, or only those whose polygon sits
//! near the selected radar site (the same radius the map's alert fetch uses).
//! Clicking a row centers the map on that alert.

use crate::app::StormApp;
use crate::state::{AlertsTab, Mode, STATION_ALERT_RADIUS_KM};
use radar_core::warnings::Warning;
use radar_data::warnings_archive::HistoricWarning;

/// How many rows to render at most (a nationwide list can run to hundreds;
/// the filter box narrows it down).
const MAX_ROWS: usize = 200;

/// Mile distance + 16-wind bearing from the selected station to a warning's
/// polygon: "42.1 mi SSW of KTWX" — the units the 3D pane and RadarScope
/// use, and the direction is what makes the row's location legible.
fn station_relative(w: &Warning, site_lat: f64, site_lon: f64, site_id: &str) -> Option<String> {
    let (lat, lon) = w.centroid()?;
    let km = radar_core::sites::haversine_km(lat, lon, site_lat, site_lon);
    let brg = radar_render::initial_bearing_deg(site_lon, site_lat, lon, lat);
    Some(format!(
        "{} {} of {site_id}",
        super::map::format_miles(km * 0.621_371),
        super::map::compass16(brg)
    ))
}

/// Does a historical warning *belong* to this station — i.e. is this site
/// the radar nearest the warning's polygon?
///
/// A plain radius was wrong: Tulsa's severe warnings (215 mi south of
/// Topeka, well inside a 400 km circle) are KINX's business, not KTWX's.
/// Ownership by nearest radar is also exactly the rule the radar jump uses,
/// so every row in this tab loads the radar it was listed under.
/// County-based events (no polygon) carry no coordinates, so they never
/// appear in a station-restricted view.
fn historic_belongs_to(w: &HistoricWarning, site_id: &str) -> bool {
    let Some(ring) = w.polygon.as_ref().and_then(|r| r.first()) else {
        return false;
    };
    if ring.is_empty() {
        return false;
    }
    let n = ring.len() as f64;
    let (mut slon, mut slat) = (0.0f64, 0.0f64);
    for (lon, lat) in ring {
        slon += lon;
        slat += lat;
    }
    let (lat, lon) = (slat / n, slon / n);
    radar_core::sites::SITES
        .iter()
        .min_by(|a, b| {
            let da = radar_core::sites::haversine_km(lat, lon, a.latitude, a.longitude);
            let db = radar_core::sites::haversine_km(lat, lon, b.latitude, b.longitude);
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
        .is_some_and(|nearest| nearest.id == site_id)
}

/// Sort bucket for grouping: convective warnings first (they are what the
/// radar layers and the archive jump care about), then a rough severity
/// order, then everything else alphabetically inside the same bucket.
pub(super) fn event_group_rank(event: &str) -> u8 {
    if event.contains("Tornado") {
        0
    } else if event.contains("Severe Thunderstorm") {
        1
    } else if event.contains("Flash Flood") {
        2
    } else if event.contains("Flood") {
        3
    } else if event.contains("Marine") {
        4
    } else if event.contains("Snow Squall") {
        5
    } else if event.contains("Winter")
        || event.contains("Ice")
        || event.contains("Snow")
        || event.contains("Blizzard")
        || event.contains("Freez")
        || event.contains("Frost")
    {
        6
    } else if event.contains("Wind") {
        7
    } else if event.contains("Heat") {
        8
    } else if event.contains("Fog") || event.contains("Smoke") {
        9
    } else if event.contains("Red Flag") || event.contains("Fire") {
        10
    } else {
        11
    }
}

/// Group rows by event name, ordered so the convective set leads. Stable
/// inside a group (the caller's ordering is preserved).
pub(super) fn group_by_event<'a>(rows: &[&'a Warning]) -> Vec<(String, Vec<&'a Warning>)> {
    let mut sorted: Vec<&Warning> = rows.to_vec();
    sorted.sort_by(|a, b| {
        event_group_rank(&a.event)
            .cmp(&event_group_rank(&b.event))
            .then_with(|| a.event.cmp(&b.event))
    });
    let mut groups: Vec<(String, Vec<&Warning>)> = Vec::new();
    for w in sorted {
        match groups.last_mut() {
            Some((name, items)) if name == &w.event => items.push(w),
            _ => groups.push((w.event.clone(), vec![w])),
        }
    }
    groups
}

/// The same grouping for the archive list, keeping each row's index (the
/// jump action addresses rows by their position in `archive_warning_view`).
fn group_archive<'a>(
    rows: &[(usize, &'a HistoricWarning)],
) -> Vec<(String, Vec<(usize, &'a HistoricWarning)>)> {
    let mut sorted = rows.to_vec();
    sorted.sort_by(|(_, a), (_, b)| {
        event_group_rank(&a.event)
            .cmp(&event_group_rank(&b.event))
            .then_with(|| a.event.cmp(&b.event))
    });
    let mut groups: Vec<(String, Vec<(usize, &HistoricWarning)>)> = Vec::new();
    for (i, w) in sorted {
        match groups.last_mut() {
            Some((name, items)) if name == &w.event => items.push((i, w)),
            _ => groups.push((w.event.clone(), vec![(i, w)])),
        }
    }
    groups
}

/// One group header: event name + count (the list is long enough that the
/// sections are what makes it scannable).
fn alert_group_header(ui: &mut egui::Ui, event: &str, count: usize, scale: f32) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(event).strong().size(12.5 * scale));
        ui.label(egui::RichText::new(format!("×{count}")).weak().small());
    });
}

/// Map/popup color for an alert by event type (matches the polygon colors).
pub(super) fn warning_color(w: &Warning) -> egui::Color32 {
    if w.is_tornado() {
        egui::Color32::from_rgb(255, 45, 45)
    } else if w.is_severe_thunderstorm() {
        egui::Color32::from_rgb(205, 80, 255)
    } else if w.is_flash_flood() {
        egui::Color32::from_rgb(80, 255, 120)
    } else if w.is_winter_storm() {
        // Ice blue: these are the warnings winter radar work is about, and
        // they used to be the same neutral white as a wind advisory.
        egui::Color32::from_rgb(150, 210, 255)
    } else if w.is_winter_advisory() {
        // Advisories keep the winter identity at lower weight, so a Winter
        // Weather Advisory reads as winter without competing with a warning.
        egui::Color32::from_rgb(118, 165, 210)
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

/// Is the radar on screen live? In archive mode the app is showing a past
/// scan, so today's live alerts are *not* about the data on display — both
/// panes hide the live warning layer then, and the archive day's polygons
/// (time-filtered, see [`archive_polygons_at`]) take over.
pub(super) fn showing_live_radar(state: &crate::state::AppState) -> bool {
    state.mode == crate::state::Mode::Live
}

/// The archive day's polygons as they should be *drawn*: only the ones the
/// displayed radar could be showing — within the station's warning radius
/// (the same reach the live layer uses) and, when `at` is given, in effect at
/// that time.
///
/// `at` is the displayed volume's time, which is what makes the overlay
/// follow the timeline: scrub or play the archive loop and the polygons come
/// and go exactly as the warnings did, instead of the whole day's set
/// floating over every scan. `None` means "no sync target" — the user is
/// browsing a past day's polygons from the live map (the archive tab is
/// open), where the day itself is the subject.
///
/// `view` and `historic` are index-aligned (`view[i]` is `historic[i]`
/// presented as the app's live-alert type); matching by id when the lengths
/// disagree keeps a partial list from hiding polygons.
pub(super) fn archive_polygons_at<'a>(
    view: &'a [Warning],
    historic: &[radar_data::warnings_archive::HistoricWarning],
    at: Option<chrono::DateTime<chrono::Utc>>,
    site_lat: f64,
    site_lon: f64,
) -> Vec<&'a Warning> {
    // Index-aligned by construction, so the common path is a zip (no string
    // lookups in the per-frame filter); an out-of-step pair falls back to id.
    let aligned = view.len() == historic.len();
    let issued = |i: usize, w: &Warning| -> Option<chrono::DateTime<chrono::Utc>> {
        if aligned {
            historic.get(i).map(|h| h.issued)
        } else {
            historic.iter().find(|h| h.id == w.id).map(|h| h.issued)
        }
    };
    view.iter()
        .enumerate()
        .filter(|(_, w)| {
            w.centroid().is_some_and(|(lat, lon)| {
                radar_core::sites::haversine_km(lat, lon, site_lat, site_lon)
                    <= crate::state::STATION_ALERT_RADIUS_KM
            })
        })
        .filter(|(i, w)| match at {
            Some(t) => {
                let started = issued(*i, w).unwrap_or(w.expires - chrono::Duration::minutes(60));
                started <= t && t <= w.expires
            }
            None => true,
        })
        .map(|(_, w)| w)
        .collect()
}

/// The archive day's display set for one pane: [`archive_polygons_at`] with
/// the site and the "is the radar on screen live?" sync target pulled from
/// the state. `active` is the caller's overlay gate (the map's alerts switch
/// and tab state; the 3D pane's own warning switch), so both panes filter
/// identically while staying independently switchable.
pub(super) fn archive_display_set<'a>(
    state: &'a crate::state::AppState,
    active: bool,
) -> Vec<&'a Warning> {
    if !active {
        return Vec::new();
    }
    let Some(site) = radar_core::sites::site(&state.site_id) else {
        return Vec::new();
    };
    let at = if showing_live_radar(state) {
        None
    } else {
        state.current().map(|f| f.volume_start)
    };
    let mut set = archive_polygons_at(
        &state.archive_warning_view,
        &state.archive_warnings,
        at,
        site.latitude,
        site.longitude,
    );
    // A warning the user just clicked should be visible while the archive
    // window loads and seeks, even if the playhead is momentarily outside its
    // in-effect window.
    if let Some((id, armed)) = &state.archive_focus {
        if armed.elapsed() < std::time::Duration::from_secs(90) {
            if let Some(w) = state.archive_warning_view.iter().find(|w| &w.id == id) {
                if !set.iter().any(|existing| existing.id == w.id) {
                    set.push(w);
                }
            }
        }
    }
    set
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
pub(super) fn significant_priority(w: &Warning) -> Option<u8> {
    let is_watch = w.event.contains("Watch");
    let is_warning = w.event.contains("Warning");
    if !is_watch && !is_warning {
        return None;
    }
    // The three storm families the radar overlay colors. A plain river
    // "Flood Warning" is not one of them: it is not radar-driven, and
    // flashing for it looked like a bug (the badge pulsing white).
    let family = if w.is_tornado() {
        4
    } else if w.is_severe_thunderstorm() {
        3
    } else if w.is_flash_flood() {
        2
    } else if w.is_winter_storm() {
        // Below the convective three, above nothing: a Winter Storm Warning
        // should pulse the badge (ice blue) on a quiet winter day, but a
        // tornado warning in the same alert set still owns the badge.
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
        egui::Color32::from_rgb(255, 45, 45)
    } else if w.is_severe_thunderstorm() {
        egui::Color32::from_rgb(205, 80, 255)
    } else if w.is_flash_flood() {
        egui::Color32::from_rgb(80, 255, 120)
    } else if w.is_winter_storm() {
        egui::Color32::from_rgb(150, 210, 255)
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
pub(crate) fn ring_contains(ring: &[(f64, f64)], lon: f64, lat: f64) -> bool {
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

/// Every warning polygon the pointer can be said to be "on", most clickable
/// first. Warnings routinely overlap — a tornado warning nested inside the
/// severe-thunderstorm polygon that carried it — and no single-polygon test
/// can serve a mouse that wants to reach *each* of them, so this returns the
/// whole stack, ordered by two rules:
///
/// 1. A polygon *outline* within `edge_px` of the pointer beats anything that
///    merely contains it, nearest edge first. Aiming at an edge is
///    unambiguous, and it is what keeps a polygon drawn *under* another one
///    reachable where their borders cross — the case where containment alone
///    always names the wrong one. (The pointer may also sit just *outside*
///    that outline; a few pixels of slop is what a mouse needs.)
/// 2. Otherwise the containing polygons, smallest first: the more specific
///    reading of a point inside several areas.
///
/// `project` maps lon/lat to view pixels for the edge test; `lon`/`lat` and
/// `px`/`py` are the same pointer in the two frames of reference. Clicking the
/// same spot again walks the rest of the stack (see `ui::map::cycle_pick`).
pub(super) fn warning_stack_at<'a>(
    alerts: impl IntoIterator<Item = &'a Warning>,
    lon: f64,
    lat: f64,
    project: impl Fn(f64, f64) -> (f64, f64),
    px: f64,
    py: f64,
    edge_px: f64,
) -> Vec<&'a Warning> {
    // (warning, distance to the nearest edge, area) for the edges, and
    // (warning, area) for the containment fallback.
    let mut edges: Vec<(&Warning, f64, f64)> = Vec::new();
    let mut insides: Vec<(&Warning, f64)> = Vec::new();
    for w in alerts {
        let Some(rings) = &w.polygon else { continue };
        let mut nearest = f64::INFINITY;
        for ring in rings {
            if ring.len() < 3 {
                continue;
            }
            for i in 0..ring.len() {
                let a = ring[i];
                let b = ring[(i + 1) % ring.len()];
                let (ax, ay) = project(a.0, a.1);
                let (bx, by) = project(b.0, b.1);
                nearest = nearest.min(super::map::point_segment_distance(px, py, ax, ay, bx, by));
            }
        }
        let area = rings.first().map_or(f64::INFINITY, |r| ring_area(r));
        if nearest <= edge_px {
            edges.push((w, nearest, area));
        } else if rings.first().is_some_and(|r| ring_contains(r, lon, lat)) {
            insides.push((w, area));
        }
    }
    // Nearest edge wins; ties (and equal areas) fall back to the id so the
    // order a click walks is stable from frame to frame.
    let by_area = |a: f64, b: f64| a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal);
    edges.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| by_area(a.2, b.2))
            .then_with(|| a.0.id.cmp(&b.0.id))
    });
    insides.sort_by(|a, b| by_area(a.1, b.1).then_with(|| a.0.id.cmp(&b.0.id)));
    edges
        .into_iter()
        .map(|(w, _, _)| w)
        .chain(insides.into_iter().map(|(w, _)| w))
        .collect()
}

/// Shoelace area of a lon/lat ring, in square degrees — only ever used to
/// compare two polygons, so the units do not matter.
pub(super) fn ring_area(ring: &[(f64, f64)]) -> f64 {
    if ring.len() < 3 {
        return f64::INFINITY;
    }
    let mut sum = 0.0;
    for i in 0..ring.len() {
        let (x1, y1) = ring[i];
        let (x2, y2) = ring[(i + 1) % ring.len()];
        sum += x1 * y2 - x2 * y1;
    }
    (sum / 2.0).abs().max(f64::MIN_POSITIVE)
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
    let fill = &mut state.alerts_fill;
    let site_id = &state.site_id;
    let all = &state.all_alerts;
    let station: Vec<&Warning> = station_alerts(all, site_lat, site_lon, STATION_ALERT_RADIUS_KM);

    // Archive tab (historical warnings). Rendered from split borrows, so the
    // data and the UI actions travel separately: the window collects one
    // action, the code after it applies the action with full access to app.
    let archive_day = state.archive_date;
    let archive_requested = state.archive_warnings_requested;
    let archive_crawl = state.archive_crawl.clone();
    // Historical views: every warning of the day, and the same day
    // restricted to the selected station's area.
    let archive_both = matches!(*tab, AlertsTab::Archive | AlertsTab::ArchiveStation);
    let archive_n_all = state.archive_warnings.len();
    let archive_n_near = state
        .archive_warnings
        .iter()
        .filter(|w| historic_belongs_to(w, site_id))
        .count();
    // Carrying the *original* index with each row matters: the station tab
    // is a filtered view, and the jump/text actions address the full list —
    // filtering without the index made a click load a different warning
    // (a Midland polygon for a Topeka row, in the field).
    let archive_view: Vec<(usize, &HistoricWarning)> = if archive_both {
        state
            .archive_warnings
            .iter()
            .enumerate()
            .filter(|(_, w)| *tab == AlertsTab::Archive || historic_belongs_to(w, site_id))
            .collect()
    } else {
        Vec::new()
    };
    let mut archive_action: Option<ArchiveAction> = None;

    // Match the sidebar's high-res scaling: on a 4K panel the window is
    // otherwise unreadable. The size is clamped to the screen so a long
    // alert list cannot push the window off the bottom edge.
    let scale = app.sidebar_scale.max(1.0);
    let size = super::popup_size(ctx, scale, 620.0, 520.0);
    let max_h = super::popup_max_height(ctx, scale);

    let mut center_on: Option<(f64, f64)> = None;
    let mut hover: Option<String> = None;
    let mut clicked: Option<String> = None;
    let mut detail: Option<String> = None;
    let mut switch_to: Option<String> = None;
    egui::Window::new("Alerts")
        .open(open)
        .default_size(size)
        .max_height(max_h)
        .min_size(egui::vec2(420.0 * scale, 220.0 * scale))
        // Centered *by its top-left corner*, not by a pivot: egui's pivot
        // is for anchored areas, and with a center pivot every size change
        // also shifts the stored position — which fights the resize drag.
        .default_pos(super::centered_popup_pos(ctx, size))
        .resizable(true)
        .show(ctx, |ui| {
            super::apply_text_scale(ui, scale);
            let site_city = radar_core::sites::site(site_id)
                .map(|s| s.name.split(',').next().unwrap_or("").trim().to_string())
                .unwrap_or_default();
            ui.horizontal(|ui| {
                ui.selectable_value(tab, AlertsTab::All, format!("All US ({})", all.len()));
                ui.selectable_value(
                    tab,
                    AlertsTab::Station,
                    format!("{site_id} {site_city} ({})", station.len()),
                );
                ui.selectable_value(
                    tab,
                    AlertsTab::Archive,
                    format!("Archive ({archive_n_all})"),
                )
                .on_hover_text(
                    "Historical warnings for the archive day (IEM VTEC archive, all of the US). \
                     Click a warning to load the radar at its time.",
                );
                ui.selectable_value(
                    tab,
                    AlertsTab::ArchiveStation,
                    format!("Archive: {site_id} ({archive_n_near})"),
                )
                .on_hover_text(
                    "Historical warnings within this station's area (polygon-based \
                     warnings only). Click one to load the radar at its time.",
                );
            });
            if *tab == AlertsTab::Station {
                let name = radar_core::sites::site(site_id)
                    .map(|s| s.name)
                    .unwrap_or(site_id);
                ui.label(
                    egui::RichText::new(format!(
                        "Alerts near {site_id} — {name} · within ~{:.0} mi ({})",
                        STATION_ALERT_RADIUS_KM * 0.621_371,
                        station.len()
                    ))
                    .strong()
                    .size(12.5 * scale),
                );
            }

            if archive_both {
                archive_tab_ui(
                    ui,
                    archive_day,
                    &archive_view,
                    archive_requested,
                    archive_crawl.as_ref(),
                    site_lat,
                    site_lon,
                    site_id,
                    *tab == AlertsTab::ArchiveStation,
                    scale,
                    &mut archive_action,
                );
                super::fill_popup_height(ui);
                return;
            }
            ui.label(
                egui::RichText::new(format!(
                    "source api.weather.gov · nationwide sweep every 5 min, station alerts every 60 s (within ~{:.0} mi of the site)",
                    STATION_ALERT_RADIUS_KM * 0.621_371
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
            // The polygon tint is adjusted where the warnings are read, and
            // the fill is applied on the map behind this window.
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Polygon fill").weak().small());
                ui.add_sized(
                    [110.0, 18.0],
                    egui::Slider::new(fill, 0.0..=1.0)
                        .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                )
                .on_hover_text(
                    "Tint strength of the warning polygons on the map \
                     (0% = outlines only)",
                );
            });
            ui.separator();

            let source: Vec<&Warning> = match *tab {
                AlertsTab::All => all.iter().collect(),
                AlertsTab::Station => station.clone(),
                // Handled above (the archive tab renders its own body).
                AlertsTab::Archive | AlertsTab::ArchiveStation => Vec::new(),
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
                super::fill_popup_height(ui);
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
                .max_height((ui.available_height() - 24.0 * scale).max(120.0))
                .scroll_bar_visibility(
                    egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                )
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let groups = group_by_event(&rows);
                    let mut shown = 0usize;
                    'groups: for (event, items) in &groups {
                        alert_group_header(ui, event, items.len(), scale);
                        for &w in items {
                            if shown >= MAX_ROWS {
                                break 'groups;
                            }
                            shown += 1;
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
                    }
                });
            super::fill_popup_height(ui);
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

    if let Some(action) = archive_action {
        match action {
            ArchiveAction::SetDay(day) => {
                app.state.archive_date = day;
            }
            ArchiveAction::Load(day) => {
                let _ = app.tx.send(crate::app::DataCmd::LoadArchiveWarnings(day));
                app.state.archive_warnings_requested = Some((day, std::time::Instant::now()));
            }
            ArchiveAction::ToggleCrawl(on) => {
                let _ = app.tx.send(crate::app::DataCmd::SetArchiveCrawl(on));
            }
            ArchiveAction::Jump(idx) => jump_to_warning(app, idx),
        }
    }

    // Lazy day load: only while the archive tab is open, once per day and on
    // day changes — an explicit user action each time, never a poll. The
    // in-flight stamp bounds retries to one per minute.
    if matches!(
        app.state.alerts_tab,
        AlertsTab::Archive | AlertsTab::ArchiveStation
    ) {
        let day = app.state.archive_date;
        let fresh = app.state.archive_warnings_date == Some(day);
        let in_flight = app
            .state
            .archive_warnings_requested
            .is_some_and(|(d, at)| d == day && at.elapsed() < std::time::Duration::from_secs(60));
        if !fresh && !in_flight {
            let _ = app.tx.send(crate::app::DataCmd::LoadArchiveWarnings(day));
            app.state.archive_warnings_requested = Some((day, std::time::Instant::now()));
        }
    }
}

/// What the archive tab asked for, applied after the window closure (where
/// the app is freely borrowable again).
enum ArchiveAction {
    SetDay(chrono::NaiveDate),
    Load(chrono::NaiveDate),
    ToggleCrawl(bool),
    Jump(usize),
}

/// Row color for a historical warning (the archive has no severity field).
fn archive_color(w: &HistoricWarning) -> egui::Color32 {
    if w.event.contains("Tornado") {
        egui::Color32::from_rgb(255, 45, 45)
    } else if w.event.contains("Severe Thunderstorm") {
        egui::Color32::from_rgb(205, 80, 255)
    } else if w.event.contains("Flash Flood") {
        egui::Color32::from_rgb(90, 220, 120)
    } else {
        egui::Color32::from_rgb(150, 200, 255)
    }
}

/// Mean of the exterior ring, in (lat, lon).
fn ring_centroid(rings: &Option<Vec<Vec<(f64, f64)>>>) -> Option<(f64, f64)> {
    let ring = rings.as_ref()?.first()?;
    if ring.is_empty() {
        return None;
    }
    let n = ring.len() as f64;
    let (mut slon, mut slat) = (0.0f64, 0.0f64);
    for (lon, lat) in ring {
        slon += lon;
        slat += lat;
    }
    Some((slat / n, slon / n))
}

/// The archive tab's body: day picker, backfill status, and the clickable
/// list of that day's warnings.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn archive_tab_ui(
    ui: &mut egui::Ui,
    day: chrono::NaiveDate,
    view: &[(usize, &HistoricWarning)],
    requested: Option<(chrono::NaiveDate, std::time::Instant)>,
    crawl: Option<&radar_data::warnings_archive::CrawlStatus>,
    site_lat: f64,
    site_lon: f64,
    site_id: &str,
    station_only: bool,
    scale: f32,
    action: &mut Option<ArchiveAction>,
) {
    let today = chrono::Utc::now()
        .with_timezone(crate::timefmt::central())
        .date_naive();
    ui.horizontal(|ui| {
        ui.label("Day (Central)");
        if ui.button("◀").clicked() {
            if let Some(p) = day.pred_opt() {
                *action = Some(ArchiveAction::SetDay(p));
            }
        }
        // Clickable date: the calendar picks any month/day directly; the
        // arrows are just for stepping a day at a time.
        let mut picked = super::jiff_from_naive(day);
        ui.add(egui_extras::DatePickerButton::new(&mut picked))
            .on_hover_text("Pick a day from the calendar");
        if let Some(picked) = super::naive_from_jiff(picked) {
            if picked != day {
                *action = Some(ArchiveAction::SetDay(picked.min(today)));
            }
        }
        if ui.button("▶").clicked() {
            if day < today {
                if let Some(s) = day.succ_opt() {
                    *action = Some(ArchiveAction::SetDay(s));
                }
            }
        }
        if ui
            .button("Load")
            .on_hover_text("Fetch this day's warnings (cached after the first time)")
            .clicked()
        {
            *action = Some(ArchiveAction::Load(day));
        }
    });
    if station_only {
        let name = radar_core::sites::site(site_id)
            .map(|s| s.name)
            .unwrap_or(site_id);
        ui.label(
            egui::RichText::new(format!("Warnings belonging to {site_id} — {name}"))
                .strong()
                .size(12.5 * scale),
        );
        ui.label(
            egui::RichText::new(format!(
                "{site_id} is the radar nearest each warning's polygon — neighbors' \
                 warnings (Tulsa's, Omaha's…) stay in their own tabs",
            ))
            .weak()
            .small(),
        );
        ui.label(
            egui::RichText::new(
                "polygon-based warnings only (county-based events carry no coordinates)",
            )
            .weak()
            .small(),
        );
    }
    ui.label(
        egui::RichText::new(
            "Historical warnings (IEM VTEC archive): polygon-based from 2002,              everything else from late 2005 · one small request per day, cached              forever",
        )
        .weak()
        .small(),
    );
    if let Some((d, _)) = requested {
        if d == day {
            ui.label(egui::RichText::new("loading this day…").weak().small());
        }
    }

    // Backfill controls + progress (the crawl fetches warnings only; radar
    // data is never pulled until you click a warning below).
    if let Some(c) = crawl {
        let mut on = c.enabled;
        if ui
            .checkbox(&mut on, "Backfill history (newest first)")
            .on_hover_text(
                "Walks backward from today, one small day-file per second at                  most, up to 800 per day, and never re-fetches a cached day.                  A 2002→now crawl is ~8,800 requests spread over days.",
            )
            .changed()
        {
            *action = Some(ArchiveAction::ToggleCrawl(on));
        }
        ui.label(
            egui::RichText::new(format!(
                "{} days cached (~{:.0} MB) · next {} · {}/{} fetches today{}",
                c.cached_days,
                c.cached_days as f64 * 0.15,
                c.next,
                c.done_today,
                c.budget,
                if c.complete {
                    " · complete (2002 reached)"
                } else {
                    ""
                },
            ))
            .weak()
            .small(),
        );
    }
    ui.separator();

    if view.is_empty() {
        ui.label(egui::RichText::new("No warnings loaded for this day (yet).").weak());
        return;
    }
    ui.label(
        egui::RichText::new(format!(
            "{} warnings — click one to load the radar at its time",
            view.len()
        ))
        .weak()
        .small(),
    );
    egui::ScrollArea::vertical()
        .max_height((ui.available_height() - 24.0 * scale).max(120.0))
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let groups = group_archive(view);
            for (event, items) in &groups {
                alert_group_header(ui, event, items.len(), scale);
                for &(i, w) in items {
                    let tz = crate::timefmt::central();
                    let issued = w.issued.with_timezone(tz).format("%H:%M");
                    let expires = w.expires.with_timezone(tz).format("%H:%M");
                    let dist = ring_centroid(&w.polygon)
                        .map(|(lat, lon)| {
                            let km = radar_render::surface_distance_m(lon, lat, site_lon, site_lat)
                                / 1000.0;
                            let brg =
                                radar_render::initial_bearing_deg(site_lon, site_lat, lon, lat);
                            format!(
                                " · {} {} of {site_id}",
                                super::map::format_miles(km * 0.621_371),
                                super::map::compass16(brg)
                            )
                        })
                        .unwrap_or_default();
                    let county = if w.polygon.is_some() {
                        ""
                    } else {
                        " · county-based"
                    };
                    let resp = egui::Frame::NONE
                        .fill(ui.visuals().faint_bg_color)
                        .corner_radius(4.0)
                        .inner_margin(egui::Margin::symmetric(6, 4))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.colored_label(
                                    archive_color(w),
                                    egui::RichText::new("●").size(11.0 * scale),
                                );
                                ui.strong(&w.event);
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(format!("⏪ radar at {}", issued))
                                                .small(),
                                        );
                                    },
                                );
                            });
                            ui.label(
                                egui::RichText::new(format!(
                                    "WFO {} · {issued} → {expires} CDT{dist}{county}",
                                    w.office
                                ))
                                .weak()
                                .small(),
                            );
                        })
                        .response;
                    let resp = resp.interact(egui::Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        *action = Some(ArchiveAction::Jump(i));
                    }
                    resp.on_hover_text(
                        "Load this warning's date/time: switches to the radar that \
                     covered it and pulls the archive data around its issuance",
                    );
                    ui.add_space(2.0);
                }
            }
        });
}

/// The archived NWS product text for one historical warning — opened by
/// clicking its polygon on the map. The IEM day-files carry metadata and
/// polygons only; the full bulletin is fetched once, on demand, and cached
/// forever. The popup also hosts the radar jump.
pub fn archive_detail_popup(ctx: &egui::Context, app: &mut StormApp) {
    let Some(id) = app.state.archive_detail.clone() else {
        return;
    };
    let Some(h) = app
        .state
        .archive_warnings
        .iter()
        .find(|h| h.id == id)
        .cloned()
    else {
        app.state.archive_detail = None;
        return;
    };
    // Fetch on demand, once: dedupe beyond that.
    if !app.state.archive_texts.contains_key(&id) && !app.state.archive_text_requested.contains(&id)
    {
        let _ = app
            .tx
            .send(crate::app::DataCmd::LoadArchiveText(id.clone()));
        app.state.archive_text_requested.insert(id.clone());
    }

    let scale = app.sidebar_scale.max(1.0);
    let size = super::popup_size(ctx, scale, 640.0, 520.0);
    let tz = crate::timefmt::central();
    let issued = h.issued.with_timezone(tz).format("%a %b %-d %H:%M");
    let expires = h.expires.with_timezone(tz).format("%H:%M");
    let mut open = true;
    egui::Window::new(format!("{} — NWS text", h.event))
        .id(egui::Id::new("stormscope.archive_text"))
        .open(&mut open)
        .default_size(size)
        .max_height(super::popup_max_height(ctx, scale))
        .min_size(egui::vec2(420.0 * scale, 240.0 * scale))
        .default_pos(super::centered_popup_pos(ctx, size))
        .resizable(true)
        .show(ctx, |ui| {
            super::apply_text_scale(ui, scale);
            ui.horizontal(|ui| {
                ui.strong(&h.event);
                ui.label(egui::RichText::new(format!("· WFO {}", h.office)).weak());
            });
            ui.label(
                egui::RichText::new(format!("{issued} → {expires} CDT · {}", h.ugc))
                    .weak()
                    .small(),
            );
            ui.horizontal(|ui| {
                if ui
                    .button("⏪ Load radar at this time")
                    .on_hover_text(
                        "Switches to the radar that covered this warning and                          pulls the archive data around its issuance",
                    )
                    .clicked()
                {
                    if let Some(idx) = app.state.archive_warnings.iter().position(|w| w.id == id)
                    {
                        jump_to_warning(app, idx);
                    }
                }
                if ui
                    .button("Center map")
                    .on_hover_text("Pan the map to this warning's polygon")
                    .clicked()
                {
                    if let Some((lat, lon)) = ring_centroid(&h.polygon) {
                        app.state.view.center_lat = lat;
                        app.state.view.center_lon = lon;
                    }
                }
            });
            ui.separator();
            match app.state.archive_texts.get(&id) {
                Some(Some(text)) => {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(text).monospace().size(12.0 * scale),
                                )
                                .wrap(),
                            );
                        });
                }
                Some(None) => {
                    ui.label(
                        egui::RichText::new("The NWS text for this product is unavailable.")
                            .weak(),
                    );
                }
                None => {
                    ui.label(egui::RichText::new("loading the archived NWS text…").weak());
                }
            }
            super::fill_popup_height(ui);
        });
    if !open {
        app.state.archive_detail = None;
    }
}

/// Clicking a historical warning: snap to the radar that covered it, load
/// the archive data around its issuance, and park the playhead there.
pub(super) fn jump_to_warning(app: &mut StormApp, idx: usize) {
    let Some(w) = app.state.archive_warnings.get(idx).cloned() else {
        return;
    };
    use chrono::Timelike;
    let local = w.issued.with_timezone(crate::timefmt::central());
    let date = local.date_naive();
    let minute = local.hour() * 60 + local.minute();

    if let Some((lat, lon)) = ring_centroid(&w.polygon) {
        // The nearest radar to the warning polygon is the one whose 3D/level
        // data covered it.
        let nearest = radar_core::sites::SITES.iter().min_by(|a, b| {
            let da = radar_render::surface_distance_m(lon, lat, a.longitude, a.latitude);
            let db = radar_render::surface_distance_m(lon, lat, b.longitude, b.latitude);
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        });
        if let Some(site) = nearest {
            if app.state.site_id != site.id {
                app.state.site_id = site.id.to_string();
                // Manual-site-switch bookkeeping, minus the live reload — the
                // archive load below is what kicks the data thread.
                app.prev_tracks.clear();
                app.marker_etas.clear();
            }
        }
        app.state.view.center_lat = lat;
        app.state.view.center_lon = lon;
        if app.state.view.zoom < 8.5 {
            app.state.view.zoom = 8.5;
        }
    }

    app.state.show_alerts = true;
    app.state.archive_focus = Some((w.id.clone(), std::time::Instant::now()));
    app.state.mode = Mode::Archive;
    app.state.archive_date = date;
    app.state.archive_minute = minute;
    app.state.archive_popup_open = true; // visible feedback while it loads
    app.state.loop_state = crate::state::LoopState::Paused;
    app.state.archive_jump = Some((w.issued, std::time::Instant::now()));
    let _ = app.tx.send(crate::app::DataCmd::LoadArchiveDate(
        app.state.site_id.clone(),
        date,
        Some(minute),
    ));
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
    let max_h = super::popup_max_height(ctx, scale);
    let mut open = true;
    egui::Window::new("NOAA alert details")
        .id(egui::Id::new("stormscope.alert.detail"))
        .open(&mut open)
        .default_size(size)
        .max_height(max_h)
        .min_size(egui::vec2(420.0 * scale, 220.0 * scale))
        // Centered *by its top-left corner*, not by a pivot: egui's pivot
        // is for anchored areas, and with a center pivot every size change
        // also shifts the stored position — which fights the resize drag.
        .default_pos(super::centered_popup_pos(ctx, size))
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
                .max_height((ui.available_height() - 50.0 * scale).max(120.0))
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
            super::fill_popup_height(ui);
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
pub(super) fn brief_alert_ui(
    ui: &mut egui::Ui,
    w: &Warning,
    site_lat: f64,
    site_lon: f64,
    site_id: &str,
) {
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
    if distance_km(w, site_lat, site_lon).is_some()
        && let Some(rel) = station_relative(w, site_lat, site_lon, site_id)
    {
        meta.push(rel);
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
                if distance_km.is_some()
                    && let Some(rel) = station_relative(w, site_latitude, site_longitude, site_id)
                {
                    ui.label(egui::RichText::new(format!("· {rel}")).weak().small());
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
                brief_alert_ui(ui, w, site_latitude, site_longitude, site_id);
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

    /// Overlapping warnings, the case that has to work: a small warning
    /// sitting across the big one's northern border. Deep inside both, the
    /// smallest wins (the more specific reading of the point); on the big
    /// one's outline — including where that outline runs *under* the small
    /// polygon — the edge wins, so the covered warning is still reachable;
    /// and every candidate is offered, in stack order.
    #[test]
    fn warning_stack_orders_edges_before_containment() {
        let boxed = |id: &str, clat: f64, half: f64| {
            let mut w = warn(id, "Severe Thunderstorm Warning", clat, -97.0, true);
            w.polygon = Some(vec![vec![
                (-97.0 - half, clat - half),
                (-97.0 + half, clat - half),
                (-97.0 + half, clat + half),
                (-97.0 - half, clat + half),
            ]]);
            w
        };
        // Big: 34.6..35.4 lat. Small: 35.37..35.47 — its lower part is inside
        // the big box, and the big box's top edge crosses it.
        let big = boxed("big", 35.0, 0.4);
        let small = boxed("small", 35.42, 0.05);
        let ident = |lo: f64, la: f64| (lo, la);
        let stack = |p_lon: f64, p_lat: f64| {
            warning_stack_at(
                &[small.clone(), big.clone()], // deliberately not draw order
                p_lon,
                p_lat,
                ident,
                p_lon,
                p_lat,
                0.01,
            )
            .into_iter()
            .map(|w| w.id.clone())
            .collect::<Vec<String>>()
        };
        // Inside both, clear of every edge: smallest first.
        assert_eq!(stack(-97.0, 35.385), vec!["small", "big"]);
        // On the big one's edge, where it passes under the small polygon:
        // the edge wins — this used to be unreachable.
        assert_eq!(stack(-97.0, 35.395), vec!["big", "small"]);
        // On the small one's own edge, the small one leads.
        assert_eq!(stack(-97.0, 35.375), vec!["small", "big"]);
        // Inside the big one only.
        assert_eq!(stack(-97.0, 35.0), vec!["big"]);
        // Outside both, beyond the edge reach of the big one's border.
        assert!(stack(-97.0, 34.0).is_empty());
    }

    /// Map-click hit testing: a click inside a polygon finds its alert, a
    /// click a hair outside its outline still counts, and zone-only alerts are
    /// never hit (their row in the browser is how you reach them).
    #[test]
    fn warning_hit_testing_on_the_map() {
        let a = warn("a", "Tornado Warning", 35.4, -97.3, true); // +/-0.1 deg box
        // Identity projection, so the pointer is given in degrees as well and
        // the 0.02 reach stays readable (a fifth of the box's side).
        let hit = |alerts: &[Warning], lon: f64, lat: f64| {
            warning_stack_at(alerts, lon, lat, |lo, la| (lo, la), lon, lat, 0.02)
                .into_iter()
                .map(|w| w.id.clone())
                .collect::<Vec<String>>()
        };
        assert_eq!(hit(&[a.clone()], -97.3, 35.4), vec!["a"], "inside");
        assert_eq!(
            hit(&[a.clone()], -97.3, 35.51),
            vec!["a"],
            "just outside the outline is still that polygon"
        );
        assert!(hit(&[a.clone()], -97.0, 35.6).is_empty(), "well outside");
        let zone = warn("z", "Heat Advisory", 35.4, -97.3, false);
        assert!(hit(&[zone], -97.3, 35.4).is_empty(), "no polygon, no hit");
    }

    /// The archive display set is what keeps the polygons synced with the
    /// radar: only warnings within the station's reach *and* in effect at the
    /// displayed scan's time are drawn — the rest of the day stays in the
    /// archive tab's list, where it belongs.
    #[test]
    fn archive_polygons_follow_the_playhead_and_the_coverage() {
        let site = radar_core::sites::site("KTWX").expect("KTWX");
        let (slat, slon) = (site.latitude, site.longitude);
        let t = Utc::now();
        let mk = |id: &str, issued: chrono::DateTime<Utc>, expires: chrono::DateTime<Utc>| {
            let w = warn(id, "Tornado Warning", slat, slon, true);
            let mut h = radar_data::warnings_archive::HistoricWarning {
                id: id.into(),
                event: w.event.clone(),
                office: "OUN".into(),
                issued,
                expires,
                gtype: 'P',
                ugc: String::new(),
                area_km2: 100.0,
                polygon: w.polygon.clone(),
            };
            // The display's clock reads the warning's own expiry.
            let mut w = w;
            w.expires = expires;
            h.expires = expires;
            let _ = issued;
            (w, h)
        };
        let (a, ha) = mk(
            "A",
            t - chrono::Duration::hours(3),
            t + chrono::Duration::hours(1),
        );
        let (b, hb) = mk(
            "B",
            t + chrono::Duration::hours(2),
            t + chrono::Duration::hours(4),
        );
        let (c, hc) = mk(
            "C",
            t - chrono::Duration::hours(10),
            t - chrono::Duration::hours(5),
        );
        // A warning in effect, but 900 km away: not this radar's business.
        let far_lat = slat + 8.0;
        let mut far = warn("D", "Tornado Warning", far_lat, slon, true);
        far.expires = t + chrono::Duration::hours(1);
        let mut hd = ha.clone();
        hd.id = "D".into();
        hd.issued = t - chrono::Duration::hours(1);
        hd.expires = far.expires;
        hd.polygon = far.polygon.clone();

        let view = vec![a, b, c, far];
        let historic = vec![ha, hb, hc, hd];

        let ids = |v: &[Warning]| v.iter().map(|w| w.id.clone()).collect::<Vec<String>>();
        let at = |v: &[&Warning]| v.iter().map(|w| w.id.clone()).collect::<Vec<String>>();

        // Synced to the scan time: only A is in effect (and in reach).
        let synced = archive_polygons_at(&view, &historic, Some(t), slat, slon);
        assert_eq!(at(&synced), vec!["A"]);
        // The scan time moves: B comes in, A stays.
        let later = archive_polygons_at(
            &view,
            &historic,
            Some(t + chrono::Duration::hours(3)),
            slat,
            slon,
        );
        assert_eq!(at(&later), vec!["B"]);
        // No sync target (browsing a past day from the live map): the whole
        // in-reach day is shown — but never the 900 km one.
        let browsing = archive_polygons_at(&view, &historic, None, slat, slon);
        assert_eq!(at(&browsing), vec!["A", "B", "C"]);
        assert_eq!(ids(&view), vec!["A", "B", "C", "D"], "fixture sanity");
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
        // Winter storms pulse too — below the convective three, above
        // nothing — while freeze products and plain advisories stay out.
        let winter = p("Winter Storm Warning").expect("winter storm pulses");
        assert!(winter < p("Flash Flood Warning").unwrap());
        assert!(p("Snow Squall Warning").is_some());
        assert!(p("Blizzard Warning").is_some());
        assert!(p("Freeze Warning").is_none(), "not a storm family");
        assert!(
            p("Winter Weather Advisory").is_none(),
            "advisory, not warning: winter color, never a pulse"
        );
        assert!(
            p("Lake Effect Snow Advisory").is_none(),
            "an advisory is not a storm, even when its family is"
        );
        assert!(p("Winter Storm Watch").is_some(), "winter watches pulse");
        assert_eq!(
            pulse_color(&warn("w", "Winter Storm Warning", 35.0, -97.0, true)),
            egui::Color32::from_rgb(150, 210, 255)
        );
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
        assert_eq!(pulse_color(&w), egui::Color32::from_rgb(205, 80, 255));
        assert_ne!(pulse_color(&watch), pulse_color(&w));
        assert!(pulse_color(&watch).r() < pulse_color(&w).r());
    }

    /// Alert colors follow the event family, matching the map polygons.
    #[test]
    fn alert_colors_follow_event_family() {
        assert_eq!(
            warning_color(&warn("t", "Tornado Warning", 35.0, -97.0, true)),
            egui::Color32::from_rgb(255, 45, 45)
        );
        assert_eq!(
            warning_color(&warn("s", "Severe Thunderstorm Warning", 35.0, -97.0, true)),
            egui::Color32::from_rgb(205, 80, 255)
        );
        assert_eq!(
            warning_color(&warn("f", "Flash Flood Warning", 35.0, -97.0, true)),
            egui::Color32::from_rgb(80, 255, 120)
        );
        // Winter storms have their own ice blue, and only the storm-based
        // families: a Freeze Warning stays neutral.
        assert_eq!(
            warning_color(&warn("w", "Winter Storm Warning", 35.0, -97.0, true)),
            egui::Color32::from_rgb(150, 210, 255)
        );
        assert_eq!(
            warning_color(&warn("i", "Ice Storm Warning", 35.0, -97.0, true)),
            egui::Color32::from_rgb(150, 210, 255)
        );
        assert_ne!(
            warning_color(&warn("z", "Freeze Warning", 35.0, -97.0, true)),
            egui::Color32::from_rgb(150, 210, 255)
        );
        // Advisories: winter-colored but dimmer, and never the storm blue.
        let advisory = warning_color(&warn("a", "Winter Weather Advisory", 35.0, -97.0, true));
        assert_eq!(advisory, egui::Color32::from_rgb(118, 165, 210));
        assert_ne!(
            advisory,
            warning_color(&warn("w", "Winter Storm Warning", 35.0, -97.0, true))
        );
        assert_eq!(
            warning_color(&warn("l", "Lake Effect Snow Advisory", 35.0, -97.0, true)),
            advisory
        );
        assert_eq!(
            warning_color(&warn("f", "Wind Chill Advisory", 35.0, -97.0, true)),
            advisory
        );
        // A wind advisory is not winter weather.
        assert_ne!(
            warning_color(&warn("n", "Wind Advisory", 35.0, -97.0, true)),
            advisory
        );
    }
}
