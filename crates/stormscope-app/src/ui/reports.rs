//! The two "what just happened on the ground" layers: **storm reports**
//! (Preliminary Local Storm Reports — spotter/public/official observations
//! with coordinates right in the product) and **Mesoscale Discussions**
//! (SPC's short-fused "what is organizing" areas).
//!
//! Both follow the layer conventions the warnings already set: hover after a
//! one-second dwell shows a brief tooltip, clicking opens the full product in
//! a resizable popup, and the sidebar toggles what is drawn.

use crate::app::StormApp;
use radar_data::lsrs::{Lsr, ReportKind};
use radar_data::mcds::Mcd;
use radar_render::MapView;

/// How close (view px) the pointer must be to a report dot for hover/click:
/// small deliberate targets, same reach as the radar-site dots.
const LSR_HIT_PX: f64 = 12.0;

/// Discussion-area color: SPC-ish amber so it reads as "forecaster product"
/// next to the warning palette.
fn mcd_color() -> egui::Color32 {
    egui::Color32::from_rgb(255, 152, 0)
}

/// Report colors track the event family, harmonized with the warning palette
/// (tornado pink, severe yellow are the app's existing hues).
pub(super) fn report_color(kind: ReportKind) -> egui::Color32 {
    match kind {
        ReportKind::Tornado => egui::Color32::from_rgb(255, 45, 45),
        ReportKind::Wind => egui::Color32::from_rgb(255, 220, 60),
        ReportKind::Hail => egui::Color32::from_rgb(150, 255, 90),
        ReportKind::Flood => egui::Color32::from_rgb(80, 170, 255),
        ReportKind::Rain => egui::Color32::from_rgb(80, 235, 255),
        ReportKind::Snow => egui::Color32::from_rgb(225, 240, 255),
        ReportKind::Other => egui::Color32::from_white_alpha(210),
    }
}

/// Storm report dots: the event color, a dark rim, a white ring for reports
/// from trained spotters, and a halo + tag while lit.
pub(super) fn draw_lsrs(
    painter: &egui::Painter,
    origin: egui::Pos2,
    view: &MapView,
    app: &StormApp,
) {
    if !app.state.show_lsrs {
        return;
    }
    for r in &app.state.lsrs {
        let Some((lat, lon)) = r.center() else {
            continue;
        };
        let (vx, vy) = view.lonlat_to_view_px(lon, lat);
        let at = egui::pos2(origin.x + vx as f32, origin.y + vy as f32);
        if !painter.clip_rect().contains(at) {
            continue;
        }
        let color = report_color(r.kind());
        let lit = app.state.lsr_hover.as_deref() == Some(r.id.as_str())
            || app.state.lsr_detail.as_deref() == Some(r.id.as_str());
        let radius = if lit { 6.5 } else { 4.5 };
        if lit {
            painter.circle_filled(at, radius + 5.0, color.gamma_multiply(0.22));
        }
        painter.circle_filled(at, radius, color);
        painter.circle_stroke(
            at,
            radius,
            egui::Stroke::new(1.4, egui::Color32::from_black_alpha(210)),
        );
        if r.is_spotter() {
            // Verified source: trained-spotter reports read at a glance.
            painter.circle_stroke(
                at,
                radius + 2.5,
                egui::Stroke::new(1.0, egui::Color32::from_white_alpha(200)),
            );
        }
        if lit {
            let label = if r.city.is_empty() {
                format!("{} {}", r.time, r.event)
            } else {
                format!("{} {} — {}", r.time, r.event, r.city)
            };
            super::map::draw_tag_label(
                painter,
                at - egui::vec2(0.0, radius + 5.0),
                egui::Align2::CENTER_BOTTOM,
                &label,
                egui::FontId::proportional(12.0),
                color,
            );
        }
    }
}

/// Discussion areas: translucent amber fill (concave-safe triangulation, the
/// same path the outlook fills use), a solid outline, and a tag with the
/// discussion number at the ring's centroid.
pub(super) fn draw_mcds(
    painter: &egui::Painter,
    origin: egui::Pos2,
    view: &MapView,
    app: &StormApp,
) {
    if !app.state.show_mcds {
        return;
    }
    for m in &app.state.mcds {
        let Some(ring) = m.polygon.first() else {
            continue;
        };
        let pts = super::map::project_ring(ring, view, origin);
        if pts.len() < 3 {
            continue;
        }
        let hovered = app.state.mcd_hover == Some(m.number);
        let color = mcd_color();
        // Mesoscale discussions are broad, long-lived areas. The Controls
        // slider sets the resting opacity (5% fill / 95% transparent by
        // default); hovering raises it just enough to confirm which area is
        // under the pointer.
        let base = app.state.mcd_fill.clamp(0.0, 1.0);
        let fill = color.gamma_multiply(if hovered { (base * 1.5).min(1.0) } else { base });
        if let Some(mesh) = super::map::triangulate(std::slice::from_ref(&pts), fill) {
            painter.add(egui::Shape::mesh(mesh));
        }
        painter.add(egui::Shape::closed_line(
            pts.clone(),
            egui::Stroke::new(if hovered { 2.6 } else { 1.6 }, color),
        ));
        let n = pts.len() as f32;
        let centroid = egui::pos2(
            pts.iter().map(|p| p.x).sum::<f32>() / n,
            pts.iter().map(|p| p.y).sum::<f32>() / n,
        );
        super::map::draw_tag_label(
            painter,
            centroid,
            egui::Align2::CENTER_CENTER,
            &format!("MD {}", m.number),
            egui::FontId::proportional(12.0),
            color,
        );
    }
}

/// Report dot under (view px): nearest within reach wins.
pub(super) fn lsr_at<'a>(lsrs: &'a [Lsr], view: &MapView, cx: f64, cy: f64) -> Option<&'a Lsr> {
    let mut best: Option<(&Lsr, f64)> = None;
    for r in lsrs {
        let Some((lat, lon)) = r.center() else {
            continue;
        };
        let (vx, vy) = view.lonlat_to_view_px(lon, lat);
        let d = (vx - cx).hypot(vy - cy);
        if d <= LSR_HIT_PX && best.map_or(true, |(_, bd)| d < bd) {
            best = Some((r, d));
        }
    }
    best.map(|(r, _)| r)
}

/// Discussion containing (lon, lat), if any.
pub(super) fn mcd_at<'a>(mcds: &'a [Mcd], lon: f64, lat: f64) -> Option<&'a Mcd> {
    mcds.iter().find(|m| {
        m.polygon
            .first()
            .is_some_and(|ring| super::alerts::ring_contains(ring, lon, lat))
    })
}

/// Brief tooltip for the report under the pointer (one-second dwell, same as
/// the warning tooltip): what it was, where, when, who reported it.
pub(super) fn lsr_hover_tooltip(ui: &egui::Ui, response: &egui::Response, app: &StormApp) {
    let Some(id) = app.state.lsr_hover.as_deref() else {
        return;
    };
    if ui.input(|i| i.time) - app.state.lsr_hover_at < 1.0 {
        return;
    }
    let Some(r) = app.state.lsrs.iter().find(|x| x.id == id) else {
        return;
    };
    let color = report_color(r.kind());
    egui::containers::Tooltip::for_widget(response)
        .at_pointer()
        .show(|ui| {
            super::apply_text_scale(ui, app.sidebar_scale.max(1.0));
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(color, "●");
                ui.strong(&r.event);
            });
            let where_ = if r.city.is_empty() {
                format!("{}, {}", r.county, r.state)
            } else {
                format!("{}, {} {}", r.city, r.county, r.state)
            };
            let mut meta = vec![where_];
            if !r.magnitude.is_empty() {
                meta.push(r.magnitude.clone());
            }
            meta.push(format!("{} {}", r.time, r.date));
            ui.label(egui::RichText::new(meta.join(" · ")).small());
            ui.label(
                egui::RichText::new(if r.is_spotter() {
                    format!("reported by a trained spotter · {}", r.source)
                } else {
                    format!("source: {}", r.source)
                })
                .weak()
                .small(),
            );
            if !r.remarks.is_empty() {
                ui.label(egui::RichText::new(&r.remarks).weak().small());
            }
            ui.label(
                egui::RichText::new("click for the full report")
                    .weak()
                    .small(),
            );
        });
}

/// Brief tooltip for the discussion area under the pointer.
pub(super) fn mcd_hover_tooltip(ui: &egui::Ui, response: &egui::Response, app: &StormApp) {
    let Some(number) = app.state.mcd_hover else {
        return;
    };
    if ui.input(|i| i.time) - app.state.mcd_hover_at < 1.0 {
        return;
    }
    let Some(m) = app.state.mcds.iter().find(|m| m.number == number) else {
        return;
    };
    egui::containers::Tooltip::for_widget(response)
        .at_pointer()
        .show(|ui| {
            super::apply_text_scale(ui, app.sidebar_scale.max(1.0));
            ui.horizontal(|ui| {
                ui.colored_label(mcd_color(), "●");
                ui.strong(format!("Mesoscale Discussion {}", m.number));
            });
            ui.label(egui::RichText::new(&m.headline()).small());
            let mut meta = Vec::new();
            if !m.valid.is_empty() {
                meta.push(format!("valid {}", m.valid));
            }
            if !m.watch_prob.is_empty() {
                meta.push(format!("watch chance {}", m.watch_prob));
            }
            if !meta.is_empty() {
                ui.label(egui::RichText::new(meta.join(" · ")).weak().small());
            }
            ui.label(
                egui::RichText::new("click for the full discussion")
                    .weak()
                    .small(),
            );
        });
}

/// Detail popup for one storm report: the fields first, then the untouched
/// product text (what NWS offices actually issued).
pub fn lsr_detail_window(ctx: &egui::Context, app: &mut StormApp) {
    let Some(id) = app.state.lsr_detail.clone() else {
        return;
    };
    let Some(r) = app.state.lsrs.iter().find(|x| x.id == id) else {
        app.state.lsr_detail = None;
        return;
    };
    let scale = app.sidebar_scale.max(1.0);
    let size = super::popup_size(ctx, scale, 560.0, 480.0);
    let max_h = super::popup_max_height(ctx, scale);
    let color = report_color(r.kind());
    let mut open = true;
    egui::Window::new("Storm report")
        .id(egui::Id::new("stormscope.lsr.detail"))
        .open(&mut open)
        .default_size(size)
        .max_height(max_h)
        .min_size(egui::vec2(420.0 * scale, 220.0 * scale))
        .default_pos(super::centered_popup_pos(ctx, size))
        .resizable(true)
        .show(ctx, |ui| {
            super::apply_text_scale(ui, scale);
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(color, egui::RichText::new("●").size(12.0 * scale));
                ui.heading(&r.event);
            });
            let place = if r.city.is_empty() {
                format!("{}, {}", r.county, r.state)
            } else {
                format!("{}, {} {}", r.city, r.county, r.state)
            };
            let mut meta = vec![place];
            if !r.magnitude.is_empty() {
                meta.push(format!("magnitude {}", r.magnitude));
            }
            meta.push(format!("{} {}", r.time, r.date));
            ui.label(egui::RichText::new(meta.join(" · ")).weak());
            ui.horizontal_wrapped(|ui| {
                let source = if r.is_spotter() {
                    format!("• trained spotter — {}", r.source)
                } else {
                    format!("source: {}", r.source)
                };
                ui.label(
                    egui::RichText::new(source)
                        .color(if r.is_spotter() {
                            color
                        } else {
                            ui.visuals().weak_text_color()
                        })
                        .small(),
                );
                ui.label(
                    egui::RichText::new(format!("NWS {}", r.office))
                        .weak()
                        .small(),
                );
            });
            if !r.remarks.is_empty() {
                ui.label(&r.remarks);
            }
            ui.separator();

            // The product as issued (WMO header stripped), scrollable.
            let scroll_h = (ui.available_height() - 40.0 * scale).max(120.0);
            egui::ScrollArea::vertical()
                .max_height(scroll_h)
                .auto_shrink([false, false])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                .show(ui, |ui| {
                    for para in r.display_text().lines() {
                        if para.trim().is_empty() {
                            ui.add_space(4.0);
                        } else {
                            ui.label(para);
                        }
                    }
                });
            ui.label(
                egui::RichText::new("source: api.weather.gov — Preliminary Local Storm Report")
                    .weak()
                    .small(),
            );
            super::fill_popup_height(ui);
        });
    if !open {
        app.state.lsr_detail = None;
    }
}

/// Detail popup for one mesoscale discussion: the operational fields, then
/// the full SPC text.
pub fn mcd_detail_window(ctx: &egui::Context, app: &mut StormApp) {
    let Some(number) = app.state.mcd_detail else {
        return;
    };
    let Some(m) = app.state.mcds.iter().find(|m| m.number == number) else {
        app.state.mcd_detail = None;
        return;
    };
    let scale = app.sidebar_scale.max(1.0);
    let size = super::popup_size(ctx, scale, 600.0, 520.0);
    let max_h = super::popup_max_height(ctx, scale);
    let mut open = true;
    egui::Window::new(format!("Mesoscale Discussion {}", m.number))
        .id(egui::Id::new("stormscope.mcd.detail"))
        .open(&mut open)
        .default_size(size)
        .max_height(max_h)
        .min_size(egui::vec2(420.0 * scale, 220.0 * scale))
        .default_pos(super::centered_popup_pos(ctx, size))
        .resizable(true)
        .show(ctx, |ui| {
            super::apply_text_scale(ui, scale);
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(mcd_color(), egui::RichText::new("●").size(12.0 * scale));
                ui.heading(&m.headline());
            });
            let mut meta = Vec::new();
            if !m.concerning.is_empty() {
                meta.push(m.concerning.clone());
            }
            if !m.valid.is_empty() {
                meta.push(format!("valid {}", m.valid));
            }
            if !m.watch_prob.is_empty() {
                meta.push(format!("watch chance {}", m.watch_prob));
            }
            if !meta.is_empty() {
                ui.label(egui::RichText::new(meta.join(" · ")).weak());
            }
            ui.horizontal_wrapped(|ui| {
                if !m.forecaster.is_empty() {
                    ui.label(
                        egui::RichText::new(format!("by {}", m.forecaster))
                            .weak()
                            .small(),
                    );
                }
                if !m.offices.is_empty() {
                    ui.label(
                        egui::RichText::new(format!("to {}", m.offices.join(", ")))
                            .weak()
                            .small(),
                    );
                }
            });
            ui.separator();

            let scroll_h = (ui.available_height() - 40.0 * scale).max(120.0);
            egui::ScrollArea::vertical()
                .max_height(scroll_h)
                .auto_shrink([false, false])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                .show(ui, |ui| {
                    for para in m.text.lines() {
                        if para.trim().is_empty() {
                            ui.add_space(4.0);
                        } else {
                            ui.label(para);
                        }
                    }
                });
            ui.label(
                egui::RichText::new("source: spc.noaa.gov/products/md")
                    .weak()
                    .small(),
            );
            super::fill_popup_height(ui);
        });
    if !open {
        app.state.mcd_detail = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(id: &str, lat: f64, lon: f64) -> Lsr {
        Lsr {
            id: id.into(),
            event: "Hail".into(),
            time: "0530 PM".into(),
            date: "10/15/2026".into(),
            magnitude: "M1.00 Inch".into(),
            city: "Rural 3 N".into(),
            county: "Shawnee".into(),
            state: "KS".into(),
            lat: Some(lat),
            lon: Some(lon),
            source: "Trained Spotter".into(),
            remarks: String::new(),
            office: "KOUN".into(),
            issued: "2026-10-16T00:00:00+00:00".into(),
            text: String::new(),
        }
    }

    fn discussion(number: u32, ring: Vec<(f64, f64)>) -> Mcd {
        Mcd {
            number,
            page: String::new(),
            areas: "central Kansas".into(),
            concerning: "Severe potential...Watch possible".into(),
            valid: "0015Z - 0200Z".into(),
            watch_prob: "40 percent".into(),
            forecaster: "Smith".into(),
            offices: vec!["OUN".into()],
            polygon: vec![ring],
            text: String::new(),
            issued: "Fri, 16 Oct 2026 00:15:05 +0000".into(),
        }
    }

    /// Report dots hit-test by proximity: nearest within reach, nothing
    /// beyond it, and reports without coordinates are never hit.
    #[test]
    fn report_hit_test_is_nearest_within_range() {
        let view = MapView {
            center_lon: -97.0,
            center_lat: 38.0,
            zoom: 7.0,
            width_px: 800.0,
            height_px: 600.0,
        };
        let near = report("a", 38.0, -97.0);
        let far = report("b", 38.0, -96.0);
        let no_geo = {
            let mut r = report("c", 0.0, 0.0);
            r.lat = None;
            r.lon = None;
            r
        };
        let all = vec![near, far, no_geo];
        let (cx, cy) = view.lonlat_to_view_px(-97.0, 38.0);
        assert!(lsr_at(&all, &view, cx, cy).is_some_and(|r| r.id == "a"));
        // 30 px away from the dot: past reach.
        assert!(lsr_at(&all, &view, cx + 30.0, cy).is_none());
        // Coordinate-less reports cannot be placed.
        let only = vec![report("c", 0.0, 0.0)];
        let (ox, oy) = view.lonlat_to_view_px(0.0, 0.0);
        assert!(lsr_at(&only, &view, ox, oy).is_some());
        let mut c = report("c", 0.0, 0.0);
        c.lat = None;
        let no_geo_only = vec![c];
        assert!(lsr_at(&no_geo_only, &view, ox, oy).is_none());
    }

    /// Discussion areas hit by containment, only while a ring exists.
    #[test]
    fn discussion_hit_test_uses_the_ring() {
        let ring = vec![(-97.5, 37.5), (-96.5, 37.5), (-96.5, 38.5), (-97.5, 38.5)];
        let mcds = vec![discussion(42, ring)];
        assert!(mcd_at(&mcds, -97.0, 38.0).is_some_and(|m| m.number == 42));
        assert!(mcd_at(&mcds, -95.0, 38.0).is_none(), "outside the area");
        let empty = vec![discussion(43, Vec::new())];
        assert!(mcd_at(&empty, -97.0, 38.0).is_none(), "no ring, no hit");
    }

    /// Report colors follow the event family.
    #[test]
    fn report_colors_differ_per_kind() {
        let c = |e: &str| report_color(ReportKind::from_event(e));
        assert_ne!(c("Tornado"), c("Hail"));
        assert_ne!(c("Severe Thunderstorm Wind"), c("Flash Flood"));
        assert_ne!(c("Rain"), c("Snow Accumulation"));
        assert_eq!(c("Funnel Cloud"), c("Funnel Cloud"), "stable");
    }
}
