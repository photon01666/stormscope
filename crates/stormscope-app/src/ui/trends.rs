//! Multi-scan storm attribute trends and lightning flash-rate trends.

use crate::app::StormApp;
use radar_render::{StormHistory, StormHistoryPoint};

pub fn storm_trends_window(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    if !app.state.storm_trends_open {
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(250));

    let Some(site) = radar_core::sites::site(&app.state.site_id) else {
        return;
    };
    let volumes: Vec<std::sync::Arc<radar_core::Volume>> =
        app.state.ring.iter().map(|f| f.volume.clone()).collect();
    let key = history_key(&volumes, app.state.tilt_deg);
    if volumes.len() >= 2
        && app.state.storm_trends_key != Some(key)
        && app.state.storm_trends_requested != Some(key)
        && app.state.storm_trends_error.is_none()
    {
        app.state.storm_trends_requested = Some(key);
        app.state.storm_trends_pending = true;
        let _ = app.tx.send(crate::app::DataCmd::LoadStormHistory {
            volumes: volumes.clone(),
            key,
            tilt_deg: app.state.tilt_deg,
            lat: site.latitude,
            lon: site.longitude,
        });
    }

    let fill = cfg!(target_os = "android");
    let scale = super::big_text_scale(app);
    let screen = ctx.content_rect().size();
    let max_h = super::popup_max_height(ctx, scale);
    // Auto-sizing is unreliable while a station switch temporarily empties
    // the ring: egui can latch onto the full available height and leave a big
    // blank window. Use a fixed compact width and a height that follows the
    // two content states instead.
    let has_cells = app
        .state
        .storm_trends
        .as_ref()
        .is_some_and(|histories| !histories.is_empty());
    let content_h = if has_cells { 680.0 } else { 300.0 };
    let desktop_size = egui::vec2(460.0 * scale, (content_h * scale).min(max_h));
    let mut open = app.state.storm_trends_open;
    let mut refresh = false;
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut close = false;
    let mut window = egui::Window::new("Storm trends")
        .id(egui::Id::new("stormscope.storm_trends.compact"))
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .max_width((screen.x - 32.0).max(200.0));
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
            .fixed_size(desktop_size)
            .resizable(false)
            .collapsible(false)
    };
    window.open(&mut open).show(ctx, |ui| {
        #[cfg(target_os = "android")]
        if fill {
            close |= super::phone_header(ui, "Storm trends");
        }
        super::apply_text_scale(ui, scale);
        if !fill {
            // A width-filling separator otherwise tells egui's auto-size pass
            // to make the whole window as wide as the screen.
            ui.set_max_width(440.0 * scale);
        }

        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{} · {:.1}° tilt · {} scans",
                    site.name,
                    app.state.tilt_deg,
                    app.state.ring.len()
                ))
                .strong(),
            );
            if ui.small_button("Refresh").clicked() {
                refresh = true;
            }
            if app.state.storm_trends_pending {
                ui.spinner();
                ui.label("building trajectories…");
            }
        });
        if let Some(err) = &app.state.storm_trends_error {
            ui.label(egui::RichText::new(format!("Storm history failed: {err}")).weak());
        }
        if app.state.ring.len() < 2 {
            ui.label(
                egui::RichText::new(
                    "Load at least two scans (or wait for the live loop) to build storm-cell trends.",
                )
                .weak(),
            );
            ui.add_space(8.0);
            ui.separator();
            lightning_trend_section(ui, app, scale);
            if !fill {
                super::fill_popup_height(ui);
            }
            return;
        }
        if let Some(histories) = app.state.storm_trends.as_ref() {
            if histories.is_empty() {
                ui.label(
                    egui::RichText::new(
                        "No tracked cells with ≥2 scans. Tracks need a moving cell with a ≥40 dBZ core.",
                    )
                    .weak()
                    .small(),
                );
                ui.add_space(8.0);
                ui.separator();
                lightning_trend_section(ui, app, scale);
                return;
            }
            let selected = app
                .state
                .storm_trends_selected
                .min(histories.len().saturating_sub(1));
            let mut sel = selected;
            let combo_width = if fill {
                ui.available_width() * 0.9
            } else {
                380.0 * scale
            };
            egui::ComboBox::from_id_salt("storm-trend-cell")
                .width(combo_width)
                .selected_text(history_label(histories, selected))
                .show_ui(ui, |ui| {
                    for i in 0..histories.len() {
                        ui.selectable_value(&mut sel, i, history_label(histories, i));
                    }
                });
            app.state.storm_trends_selected = sel;
            let history = &histories[selected];
            if let Some(last) = history.latest() {
                let speed_mph = last.speed_kmh * 0.621371;
                ui.label(
                    egui::RichText::new(format!(
                        "Latest: {:.0} dBZ · {:.0} km² · {:.0} mph · Δ {:+.0} dBZ / {:+.0} km² · {} scans",
                        last.max_dbz,
                        last.area_km2,
                        speed_mph,
                        history.delta_dbz(),
                        history.delta_area_km2(),
                        history.points.len(),
                    ))
                    .weak()
                    .small(),
                );
                ui.add_space(4.0);
                let w = if fill {
                    ui.available_width().max(260.0)
                } else {
                    400.0 * scale
                };
                chart(
                    ui,
                    "Peak reflectivity",
                    &history.points,
                    |p| f64::from(p.max_dbz),
                    30.0,
                    80.0,
                    "dBZ",
                    egui::Color32::from_rgb(255, 180, 90),
                    egui::vec2(w, 110.0),
                );
                ui.add_space(6.0);
                chart(
                    ui,
                    "Core area",
                    &history.points,
                    |p| p.area_km2,
                    0.0,
                    history
                        .points
                        .iter()
                        .map(|p| p.area_km2)
                        .fold(1.0, f64::max)
                        .max(32.0),
                    "km²",
                    egui::Color32::from_rgb(120, 200, 255),
                    egui::vec2(w, 110.0),
                );
                ui.add_space(4.0);
                egui::Grid::new("storm-trend-points")
                    .num_columns(3)
                    .striped(true)
                    .spacing([10.0, 2.0])
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new("Scan").strong().small());
                        ui.label(egui::RichText::new("Max dBZ").strong().small());
                        ui.label(egui::RichText::new("Area").strong().small());
                        ui.end_row();
                        for point in history.points.iter().rev().take(5) {
                            ui.label(egui::RichText::new(crate::timefmt::fmt_central(&point.time)).small());
                            ui.label(egui::RichText::new(format!("{:.0}", point.max_dbz)).small());
                            ui.label(egui::RichText::new(format!("{:.0} km²", point.area_km2)).small());
                            ui.end_row();
                        }
                    });
            }
        } else if !app.state.storm_trends_pending {
            ui.label(
                egui::RichText::new("Open the window again to build the trajectory.")
                    .weak()
                    .small(),
            );
        }
        ui.add_space(6.0);
        ui.separator();
        lightning_trend_section(ui, app, scale);
    });
    if close {
        open = false;
    }
    if refresh {
        app.state.storm_trends = None;
        app.state.storm_trends_key = None;
        app.state.storm_trends_requested = None;
        app.state.storm_trends_error = None;
        app.state.storm_trends_pending = true;
        let _ = app.tx.send(crate::app::DataCmd::LoadStormHistory {
            volumes,
            key,
            tilt_deg: app.state.tilt_deg,
            lat: site.latitude,
            lon: site.longitude,
        });
    }
    app.state.storm_trends_open = open;
}

fn lightning_trend_section(ui: &mut egui::Ui, app: &mut StormApp, scale: f32) {
    ui.label(egui::RichText::new("Lightning flash rate").strong().small());
    let now = chrono::Utc::now();
    let cutoff = now - chrono::Duration::minutes(60);
    // Build 60 one-minute bins ending at current minute.
    use chrono::Timelike;
    use std::collections::BTreeMap;
    let mut bins: BTreeMap<chrono::DateTime<chrono::Utc>, f64> = BTreeMap::new();
    for m in (0..60).rev() {
        let t = now - chrono::Duration::minutes(m);
        let minute = t
            .with_second(0)
            .and_then(|t| t.with_nanosecond(0))
            .unwrap_or(t);
        if minute < cutoff {
            continue;
        }
        bins.insert(minute, 0.0);
    }
    for (bt, c) in &app.state.lightning_bins {
        if *bt >= cutoff {
            bins.insert(*bt, *c as f64);
        }
    }
    let points: Vec<(chrono::DateTime<chrono::Utc>, f64)> = bins.into_iter().collect();
    if points.is_empty() {
        ui.label(
            egui::RichText::new("No GLM flash data in the last 60 min.")
                .weak()
                .small(),
        );
        return;
    }
    let total_60: f64 = points.iter().map(|(_, c)| *c).sum();
    let total_30: f64 = points.iter().rev().take(30).map(|(_, c)| *c).sum();
    let total_10: f64 = points.iter().rev().take(10).map(|(_, c)| *c).sum();
    let peak = points.iter().map(|(_, c)| *c).fold(0.0f64, f64::max);
    let peak_time = points
        .iter()
        .find(|(_, c)| (*c - peak).abs() < 1e-9)
        .map(|(t, _)| *t);
    let last_min = points.last().map(|(_, c)| *c).unwrap_or(0.0);
    let prev_min = if points.len() >= 2 {
        points[points.len() - 2].1
    } else {
        0.0
    };
    let change_1m = if prev_min > 0.0 {
        ((last_min - prev_min) / prev_min) * 100.0
    } else if last_min > 0.0 {
        100.0
    } else {
        0.0
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(format!("Last min: {:.0} fl/min", last_min)).small());
        ui.label(egui::RichText::new(format!("Peak: {:.0} fl/min", peak)).small());
        if let Some(pt) = peak_time {
            ui.label(
                egui::RichText::new(format!("at {}", pt.format("%H:%MZ")))
                    .weak()
                    .small(),
            );
        }
        ui.label(egui::RichText::new(format!("1-min Δ: {:+.0}%", change_1m)).small());
    });
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(format!("Total 10 min: {:.0}", total_10)).small());
        ui.label(egui::RichText::new(format!("30 min: {:.0}", total_30)).small());
        ui.label(egui::RichText::new(format!("60 min: {:.0}", total_60)).small());
    });
    let w = if cfg!(target_os = "android") {
        ui.available_width().max(260.0)
    } else {
        400.0 * scale
    };
    let h = 110.0 * scale.min(1.0);
    lightning_chart(ui, &points, w, h);
}

fn lightning_chart(
    ui: &mut egui::Ui,
    points: &[(chrono::DateTime<chrono::Utc>, f64)],
    width: f32,
    height: f32,
) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, egui::Color32::from_gray(10));
    if points.is_empty() {
        return;
    }
    let left = rect.left() + 34.0;
    let right = rect.right() - 6.0;
    let top = rect.top() + 6.0;
    let bottom = rect.bottom() - 16.0;
    if right <= left || bottom <= top {
        return;
    }
    let mut maxv = points.iter().map(|(_, v)| *v).fold(1.0f64, f64::max);
    if maxv < 4.0 {
        maxv = 4.0;
    }
    for i in 0..=4 {
        let t = i as f32 / 4.0;
        let y = top + t * (bottom - top);
        p.line_segment(
            [egui::pos2(left, y), egui::pos2(right, y)],
            egui::Stroke::new(0.5, egui::Color32::from_gray(35)),
        );
        let v = maxv - f64::from(t) * maxv;
        p.text(
            egui::pos2(rect.left() + 2.0, y),
            egui::Align2::LEFT_CENTER,
            format!("{v:.0}"),
            egui::FontId::monospace(9.0),
            egui::Color32::from_gray(120),
        );
    }
    p.text(
        egui::pos2(rect.left() + 2.0, rect.top() + 2.0),
        egui::Align2::LEFT_TOP,
        "fl/min",
        egui::FontId::monospace(9.0),
        egui::Color32::from_gray(120),
    );
    let n = points.len();
    let dx = if n > 1 {
        (right - left) / (n - 1) as f32
    } else {
        0.0
    };
    let mut line = Vec::with_capacity(n);
    for (i, (_t, v)) in points.iter().enumerate() {
        let v = (*v).min(maxv);
        let x = left + i as f32 * dx;
        let y = bottom - (v / maxv).max(0.0) as f32 * (bottom - top);
        line.push(egui::pos2(x, y));
    }
    if line.len() > 1 {
        p.add(egui::Shape::line(
            line.clone(),
            egui::Stroke::new(2.0, egui::Color32::from_rgb(180, 220, 255)),
        ));
    }
    for pt in &line {
        p.circle_filled(*pt, 2.0, egui::Color32::from_rgb(180, 220, 255));
    }
    if n > 0 {
        let labels = [0, n / 2, n - 1];
        for i in labels {
            if let Some((t, _)) = points.get(i) {
                let x = left + i as f32 * dx;
                p.text(
                    egui::pos2(x, rect.bottom() - 2.0),
                    egui::Align2::CENTER_BOTTOM,
                    t.format("%H:%M").to_string(),
                    egui::FontId::monospace(9.0),
                    egui::Color32::from_gray(120),
                );
            }
        }
    }
}

fn history_key(volumes: &[std::sync::Arc<radar_core::Volume>], tilt: f32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for v in volumes {
        v.meta.volume_start.timestamp_millis().hash(&mut h);
    }
    tilt.to_bits().hash(&mut h);
    h.finish()
}

fn history_label(histories: &[StormHistory], i: usize) -> String {
    let Some(h) = histories.get(i) else {
        return "cell".to_string();
    };
    match h.latest() {
        Some(p) => format!(
            "#{} · {:.0} dBZ · {:.0} km² · {:.2},{:.2}",
            i + 1,
            p.max_dbz,
            p.area_km2,
            p.lat,
            p.lon
        ),
        None => format!("#{}", i + 1),
    }
}

#[allow(clippy::too_many_arguments)]
fn chart(
    ui: &mut egui::Ui,
    title: &str,
    points: &[StormHistoryPoint],
    value: impl Fn(&StormHistoryPoint) -> f64,
    min: f64,
    max: f64,
    unit: &str,
    color: egui::Color32,
    size: egui::Vec2,
) {
    ui.label(egui::RichText::new(title).strong().small());
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, egui::Color32::from_gray(10));
    let left = rect.left() + 44.0;
    let right = rect.right() - 8.0;
    let top = rect.top() + 8.0;
    let bottom = rect.bottom() - 20.0;
    if points.is_empty() || right <= left || bottom <= top {
        return;
    }
    for i in 0..=4 {
        let t = i as f32 / 4.0;
        let y = top + t * (bottom - top);
        p.line_segment(
            [egui::pos2(left, y), egui::pos2(right, y)],
            egui::Stroke::new(0.5, egui::Color32::from_gray(35)),
        );
        let v = max - f64::from(t) * (max - min);
        p.text(
            egui::pos2(rect.left() + 2.0, y),
            egui::Align2::LEFT_CENTER,
            format!("{v:.0}"),
            egui::FontId::monospace(9.0),
            egui::Color32::from_gray(120),
        );
    }
    p.text(
        egui::pos2(rect.left() + 2.0, rect.top() + 2.0),
        egui::Align2::LEFT_TOP,
        unit,
        egui::FontId::monospace(9.0),
        egui::Color32::from_gray(120),
    );
    let n = points.len();
    let dx = if n > 1 {
        (right - left) / (n - 1) as f32
    } else {
        0.0
    };
    let mut line = Vec::with_capacity(n);
    for (i, point) in points.iter().enumerate() {
        let v = value(point).clamp(min, max);
        let x = left + i as f32 * dx;
        let y = bottom - ((v - min) / (max - min).max(f64::EPSILON)) as f32 * (bottom - top);
        line.push(egui::pos2(x, y));
    }
    if line.len() > 1 {
        p.add(egui::Shape::line(
            line.clone(),
            egui::Stroke::new(2.0, color),
        ));
    }
    for pt in &line {
        p.circle_filled(*pt, 3.0, color);
    }
    for i in [0usize, n / 2, n - 1] {
        if let Some(point) = points.get(i) {
            let x = left + i as f32 * dx;
            p.text(
                egui::pos2(x, rect.bottom() - 2.0),
                egui::Align2::CENTER_BOTTOM,
                point.time.format("%H:%MZ").to_string(),
                egui::FontId::monospace(9.0),
                egui::Color32::from_gray(120),
            );
        }
    }
}
