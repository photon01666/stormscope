//! Central map canvas: basemap tiles + radar overlay + site markers +
//! warning polygons, all sharing MapView's viewport-origin convention.
//!
//! Layer order (bottom -> top):
//!   1. basemap tiles          (radar_render::TileLayer)
//!   2. range rings + radar site markers/labels
//!   3. radar sweep raster     (needs decoded volumes; TODO once live)
//!   4. warning polygons
//!
//! The tile layer derives its rect from the current cursor (it does not
//! allocate), so every other layer must use the same origin: the viewport
//! rect returned by `TileLayer::paint`. Screen position of view pixel
//! (x, y) is `resp.rect.min + (x, y)`.

use crate::app::StormApp;
use radar_render::MapView;

/// Minimum zoom that still shows useful geography; keep the user out of
/// blank-ocean levels and out of the "texture stretched to mush" range.
const MIN_ZOOM: f64 = 3.0;
const MAX_ZOOM: f64 = 12.0;

pub fn map(ui: &mut egui::Ui, app: &mut StormApp) {
    // --- Interaction -----------------------------------------------------
    // The tile layer derives the viewport rect from the current cursor and
    // registers a hover widget over it; we reuse that same rect for pan/zoom
    // and for every overlay layer so nothing drifts.
    // Interactive widget over the whole viewport. Drag sense is required for
    // panning (`drag_delta()` is always zero on a hover-only widget).
    let (rect, response) =
        ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
    app.state.view.width_px = rect.width();
    app.state.view.height_px = rect.height();
    let view = app.state.view;

    // Pan: drag moves the map content with the pointer.
    if response.dragged() {
        let drag = response.drag_delta();
        if drag != egui::Vec2::ZERO {
            pan_view(&mut app.state.view, view, drag.x as f64, drag.y as f64);
        }
    }

    // Click to select the nearest radar site under the pointer. Use
    // `interact_pointer_pos()` (the press position), not `hover_pos()`, so a
    // small drag does not also count as a click.
    if response.clicked() {
        if let Some(click) = response.interact_pointer_pos() {
            // Viewport-relative click position -> same frame of reference as
            // `draw_sites` marker positions.
            let (cx, cy) = ((click.x - rect.min.x) as f64, (click.y - rect.min.y) as f64);
            const HIT_PX: f64 = 14.0;
            let mut best: Option<(&radar_core::sites::RadarSite, f64)> = None;
            for site in radar_core::sites::SITES {
                let (vx, vy) = view.lonlat_to_view_px(site.longitude, site.latitude);
                let d = ((vx - cx).powi(2) + (vy - cy).powi(2)).sqrt();
                if d <= HIT_PX && best.map_or(true, |(_, bd)| d < bd) {
                    best = Some((site, d));
                }
            }
            if let Some((site, _)) = best {
                app.select_site(site.id);
            }
        }
    }

    // Zoom: pinch / ctrl+scroll via zoom_delta(), plain scroll wheel too.
    //
    // IMPORTANT: read the pointer position *before* taking the input lock.
    // Calling `response.hover_pos()` from inside a `ctx.input(..)` closure
    // re-enters egui's Context lock (both go through `Context::write`) and
    // self-deadlocks the frame — that also starves the next frame's pointer
    // events, which is why the wheel appeared dead.
    if response.hovered() {
        // Anchor = pointer position in viewport-relative pixels.
        let anchor = response
            .hover_pos()
            .map(|p| ((p.x - rect.min.x) as f64, (p.y - rect.min.y) as f64))
            .unwrap_or((rect.width() as f64 / 2.0, rect.height() as f64 / 2.0));
        let (wheel, pinch) = ui.ctx().input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        let mut z = app.state.view.zoom;
        let zd = pinch as f64;
        if zd != 1.0 {
            z *= zd;
        }
        if wheel != 0.0 {
            z += wheel as f64 * 0.0025;
        }
        if (z - app.state.view.zoom).abs() > f64::EPSILON {
            // Keep the point under the pointer stationary.
            zoom_view(&mut app.state.view, view, z, anchor);
        }
    }

    // --- Layer 1: basemap tiles ------------------------------------------
    // `TileLayer::paint` uses `ui.cursor().min` as the viewport origin, so
    // give it a child Ui pinned to the map rect. The rect it returns is the
    // viewport rect every later layer must share.
    let mut tile_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    let tile_resp = app.tiles.paint(&tile_ui, &app.state.view);
    let origin = tile_resp.rect.min;

    let painter = ui.painter_at(tile_resp.rect);
    let view = app.state.view;

    // --- Layer 2: range rings + radar sites -------------------------------
    draw_range_rings(&painter, origin, &view, app);
    draw_sites(&painter, origin, &view, app);

    // --- Layer 3: radar sweep overlay --------------------------------------
    // Rasterize the current frame's sweep for the selected moment+tilt and
    // paint it as a texture covering the viewport. The texture is cached and
    // only re-rasterized when something that affects pixels changes (frame,
    // moment, tilt, view, opacity) — never per frame.
    if let Some(frame) = app.state.current() {
        if let Some(site) = radar_core::sites::site(&app.state.site_id) {
            if let Some(si) = frame.volume.sweep_index_for_tilt(app.state.tilt_deg) {
                let sweep = &frame.volume.sweeps[si];
                if sweep.moment(app.state.moment).is_some() {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    frame.volume_start.timestamp_millis().hash(&mut h);
                    app.state.moment.hash(&mut h);
                    app.state.tilt_deg.to_bits().hash(&mut h);
                    app.state.view.zoom.to_bits().hash(&mut h);
                    app.state.view.center_lat.to_bits().hash(&mut h);
                    app.state.view.center_lon.to_bits().hash(&mut h);
                    app.state.view.width().hash(&mut h);
                    app.state.view.height().hash(&mut h);
                    ((app.state.opacity * 100.0) as u32).hash(&mut h);
                    app.state.min_dbz.to_bits().hash(&mut h);
                    let sig = h.finish();

                    if app.radar_tex.as_ref().map_or(true, |(_, s)| *s != sig) {
                        // Drag throttle: a full-viewport raster is the app's
                        // most expensive op, so during a pan keep showing the
                        // last raster and redo it once things settle.
                        let settling = !response.dragged()
                            || app.radar_last_raster.elapsed()
                                >= std::time::Duration::from_millis(150);
                        if settling {
                            let min_dbz = match app.state.moment {
                                radar_core::Moment::Reflectivity
                                | radar_core::Moment::CompositeReflectivity => app.state.min_dbz,
                                _ => f32::NEG_INFINITY, // filter is dBZ-only
                            };
                            let img = radar_render::rasterize_sweep(
                                sweep,
                                app.state.moment,
                                &view,
                                site.latitude,
                                site.longitude,
                                app.state.opacity,
                                min_dbz,
                            );
                            let color = egui::ColorImage::from_rgba_unmultiplied(
                                [img.width, img.height],
                                &img.rgba,
                            );
                            let tex = ui.ctx().load_texture(
                                format!("radar-{}-{}", app.state.site_id, sig),
                                color,
                                egui::TextureOptions::LINEAR,
                            );
                            app.radar_tex = Some((tex, sig));
                            app.radar_last_raster = std::time::Instant::now();
                            app.radar_pending = None;
                        } else {
                            app.radar_pending = Some(sig);
                        }
                    }
                    if let Some((tex, _)) = &app.radar_tex {
                        let uv = egui::Rect::from_min_max(
                            egui::pos2(0.0, 0.0),
                            egui::pos2(1.0, 1.0),
                        );
                        painter.image(tex.id(), tile_resp.rect, uv, egui::Color32::WHITE);
                    }
                }
            }
        }
    }

    // --- Layer 4: warning polygons ----------------------------------------
    draw_warnings(&painter, origin, &view, app);

    // --- Attribution (required by OSM/Wikimedia tile usage policies) ------
    draw_attribution(&painter, tile_resp.rect);

    // Keep animating while a loop is playing (tiles may also arrive async).
    if app.state.loop_state == crate::state::LoopState::Playing {
        ui.ctx().request_repaint();
    }
}

/// Move the view center so the map content follows a drag of (dx, dy) px.
fn pan_view(dst: &mut MapView, cur: MapView, dx: f64, dy: f64) {
    // Dragging right moves content right -> center moves left in world px.
    let (cx, cy) = radar_render::lonlat_to_merc_px(cur.center_lon, cur.center_lat, cur.zoom);
    let (lon, lat) = radar_render::merc_px_to_lonlat(cx - dx, cy - dy, cur.zoom);
    dst.center_lon = lon;
    dst.center_lat = lat.clamp(-85.0, 85.0);
}

/// Zoom to `new_zoom` keeping the geo point under `anchor` (viewport px)
/// stationary.
fn zoom_view(dst: &mut MapView, cur: MapView, new_zoom: f64, anchor: (f64, f64)) {
    let new_zoom = new_zoom.clamp(MIN_ZOOM, MAX_ZOOM);
    // Geo point currently under the anchor...
    let (lon, lat) = cur.view_px_to_lonlat(anchor.0, anchor.1);
    // ...its world (Mercator) position at the new zoom...
    let (ax, ay) = radar_render::lonlat_to_merc_px(lon, lat, new_zoom);
    // ...which pins the new view center: after the zoom the anchor pixel must
    // still show that same geo point, so center = point - anchor + halfsize.
    // (An earlier "probe" formulation double-counted the offset and flung
    // the view thousands of km away on every zoom — the vanishing-map bug.)
    let half_w = cur.width() as f64 / 2.0;
    let half_h = cur.height() as f64 / 2.0;
    let (cx, cy) = radar_render::merc_px_to_lonlat(
        ax - anchor.0 + half_w,
        ay - anchor.1 + half_h,
        new_zoom,
    );
    dst.zoom = new_zoom;
    dst.center_lon = cx;
    dst.center_lat = cy.clamp(-85.0, 85.0);
}

/// Ground-range rings around the selected radar (50 km steps).
fn draw_range_rings(
    painter: &egui::Painter,
    origin: egui::Pos2,
    view: &MapView,
    app: &StormApp,
) {
    let Some(site) = radar_core::sites::site(&app.state.site_id) else { return };
    let stroke = egui::Stroke::new(1.0, egui::Color32::from_white_alpha(60));
    for km in [50u32, 100, 150, 200, 250] {
        let mut pts = Vec::with_capacity(96);
        for i in 0..=96 {
            let az = i as f32 * 360.0 / 96.0;
            let (lat, lon) =
                radar_render::gate_latlon(site.latitude, site.longitude, az, 0.0, km as f32 * 1000.0);
            let (vx, vy) = view.lonlat_to_view_px(lon, lat);
            pts.push(egui::pos2(origin.x + vx as f32, origin.y + vy as f32));
        }
        painter.add(egui::Shape::closed_line(pts, stroke));
    }
}

/// Every radar site whose position falls inside the viewport: marker dot +
/// "ID City" label. The selected site is emphasized.
fn draw_sites(painter: &egui::Painter, origin: egui::Pos2, view: &MapView, app: &StormApp) {
    let (w, h) = (view.width() as f64, view.height() as f64);
    let pad = 24.0f64;

    for site in radar_core::sites::SITES {
        let (vx, vy) = view.lonlat_to_view_px(site.longitude, site.latitude);
        if vx < -pad || vy < -pad || vx > w + pad || vy > h + pad {
            continue; // off-screen; "all of them on the current map section"
        }
        let selected = site.id == app.state.site_id;
        let pos = egui::pos2(origin.x + vx as f32, origin.y + vy as f32);

        if selected {
            painter.circle_filled(pos, 6.0, egui::Color32::from_rgb(90, 180, 255));
            painter.circle_stroke(
                pos,
                9.0,
                egui::Stroke::new(1.5, egui::Color32::from_rgb(90, 180, 255)),
            );
        } else {
            painter.circle_filled(pos, 3.0, egui::Color32::from_white_alpha(170));
            painter.circle_stroke(
                pos,
                4.5,
                egui::Stroke::new(1.0, egui::Color32::from_black_alpha(140)),
            );
        }

        // "KTLX Oklahoma City" — ID + city (state stripped), label shaded for
        // legibility over any basemap.
        let city = site.name.split(',').next().unwrap_or(site.name).trim();
        let text = format!("{} {}", site.id, city);
        let font = if selected {
            egui::FontId::proportional(13.0)
        } else {
            egui::FontId::proportional(11.5)
        };
        let anchor = egui::Align2::LEFT_BOTTOM;
        let at = egui::pos2(pos.x + 7.0, pos.y - 5.0);
        painter.text(at, anchor, &text, font.clone(), egui::Color32::BLACK.gamma_multiply(0.6));
        painter.text(
            at - egui::vec2(1.0, 1.0),
            anchor,
            &text,
            font,
            if selected {
                egui::Color32::from_rgb(150, 210, 255)
            } else {
                egui::Color32::from_white_alpha(220)
            },
        );
    }
}

/// NWS warning polygons, colored by event type.
fn draw_warnings(painter: &egui::Painter, origin: egui::Pos2, view: &MapView, app: &StormApp) {
    if !app.state.show_alerts {
        return;
    }
    for w in &app.state.alerts {
        let Some(rings) = &w.polygon else { continue };
        let color = if w.is_tornado() {
            egui::Color32::from_rgb(255, 60, 120)
        } else if w.is_severe_thunderstorm() {
            egui::Color32::from_rgb(255, 220, 60)
        } else if w.is_flash_flood() {
            egui::Color32::from_rgb(80, 255, 120)
        } else {
            egui::Color32::from_white_alpha(140)
        };
        for ring in rings {
            if ring.len() < 3 {
                continue;
            }
            let pts: Vec<egui::Pos2> = ring
                .iter()
                .map(|&(lon, lat)| {
                    let (vx, vy) = view.lonlat_to_view_px(lon, lat);
                    egui::pos2(origin.x + vx as f32, origin.y + vy as f32)
                })
                .collect();
            painter.add(egui::Shape::closed_line(
                pts.clone(),
                egui::Stroke::new(2.0, color),
            ));
            painter.add(egui::Shape::convex_polygon(
                pts,
                color.gamma_multiply(0.12),
                egui::Stroke::NONE,
            ));
        }
    }
}

/// Bottom-right tile attribution. Both Wikimedia and OSM tile policies
/// require "© OpenStreetMap contributors" to stay visible.
fn draw_attribution(painter: &egui::Painter, rect: egui::Rect) {
    let text = "© OpenStreetMap contributors";
    let font = egui::FontId::proportional(10.0);
    let pos = egui::pos2(rect.right() - 6.0, rect.bottom() - 4.0);
    painter.text(
        pos,
        egui::Align2::RIGHT_BOTTOM,
        text,
        font.clone(),
        egui::Color32::BLACK.gamma_multiply(0.7),
    );
    painter.text(
        pos - egui::vec2(1.0, 1.0),
        egui::Align2::RIGHT_BOTTOM,
        text,
        font,
        egui::Color32::from_white_alpha(160),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(zoom: f64) -> MapView {
        MapView {
            center_lat: 35.3331,
            center_lon: -97.2778,
            zoom,
            width_px: 1200.0,
            height_px: 800.0,
        }
    }

    /// The geo point under the anchor must stay under the anchor after
    /// zooming. A previous implementation double-counted the probe offset and
    /// flung the view thousands of km away on every zoom ("map disappears").
    #[test]
    fn zoom_keeps_anchor_point_stable() {
        for &(z0, z1) in &[(8.0, 9.0), (9.0, 8.0), (7.3, 11.9), (12.0, 4.0)] {
            for &anchor in &[(600.0, 400.0), (100.0, 700.0), (1150.0, 50.0)] {
                let cur = view(z0);
                let (lon, lat) = cur.view_px_to_lonlat(anchor.0, anchor.1);
                let mut dst = cur;
                zoom_view(&mut dst, cur, z1, anchor);
                let (nx, ny) = dst.lonlat_to_view_px(lon, lat);
                assert!(
                    (nx - anchor.0).abs() < 1e-6 && (ny - anchor.1).abs() < 1e-6,
                    "zoom {z0}->{z1} anchor {anchor:?}: point drifted to ({nx}, {ny})"
                );
            }
        }
    }

    /// Zooming straight in at the exact center must keep the center fixed.
    #[test]
    fn zoom_at_center_keeps_center() {
        let cur = view(8.0);
        let anchor = (600.0, 400.0);
        let mut dst = cur;
        zoom_view(&mut dst, cur, 9.5, anchor);
        assert!((dst.center_lat - cur.center_lat).abs() < 1e-6);
        assert!((dst.center_lon - cur.center_lon).abs() < 1e-6);
    }
}
