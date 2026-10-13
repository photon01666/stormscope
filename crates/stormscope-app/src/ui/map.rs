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

    // Map click priorities: edit an existing marker -> place a new marker
    // (Shift or "＋ Place" mode) -> select the nearest radar site. Use
    // `interact_pointer_pos()` (the press position), not `hover_pos()`, so a
    // small drag does not also count as a click.
    if response.clicked() {
        if let Some(click) = response.interact_pointer_pos() {
            // Viewport-relative click position -> same frame of reference as
            // `draw_sites` marker positions.
            let (cx, cy) = ((click.x - rect.min.x) as f64, (click.y - rect.min.y) as f64);

            // 1) Clicking an existing pin opens its label editor.
            const HIT_PX: f64 = 14.0;
            let mut marker_hit: Option<usize> = None;
            for (i, m) in app.state.markers.iter().enumerate() {
                let (vx, vy) = view.lonlat_to_view_px(m.lon, m.lat);
                if (vx - cx).hypot(vy - cy) <= HIT_PX {
                    marker_hit = Some(i);
                    break;
                }
            }
            if let Some(i) = marker_hit {
                // Seed the persistent draft the popup edits (egui's TextEdit
                // is immediate-mode — the draft must live in app state).
                app.state.pending_marker_label = app.state.markers[i].label.clone();
                app.state.pending_marker = Some(i);
            } else {
                let shift = ui.input(|i| i.modifiers.shift);
                if shift || app.state.add_marker_mode {
                    // 2) Drop a new pin and ask for its label.
                    let (lon, lat) = view.view_px_to_lonlat(cx, cy);
                    app.state.markers.push(crate::state::MapMarker {
                        lat,
                        lon,
                        label: String::new(),
                    });
                    app.state.pending_marker = Some(app.state.markers.len() - 1);
                    app.state.pending_marker_label.clear();
                    app.state.add_marker_mode = false;
                    app.state.persist_markers();
                } else {
                    // 3) Select the nearest radar site under the pointer.
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

    // --- Layer 2.5: SPC outlook *fills* ------------------------------------
    // Under the radar so echoes stay crisp; the outline pass (Layer 3.5)
    // re-draws the risk boundaries over the echoes.
    draw_outlooks(&painter, origin, &view, app, true);

    // --- Layer 3: radar sweep overlay --------------------------------------
    // Rasterize the current frame's sweep for the selected moment+tilt and
    // paint it as a texture covering the viewport. The texture is cached and
    // only re-rasterized when something that affects pixels changes (frame,
    // moment, tilt, view, opacity) — never per frame.
    if let Some(frame) = app.state.current() {
        if let Some(site) = radar_core::sites::site(&app.state.site_id) {
            if let Some(si) =
                frame
                    .volume
                    .sweep_index_for_tilt_moment(app.state.tilt_deg, app.state.moment)
            {
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

                    if app.radar_tex.as_ref().map_or(true, |t| t.sig != sig) {
                        // Drag throttle: a full-viewport raster is the app's
                        // most expensive op, so during a pan keep the last
                        // raster and redo it once things settle. The stale
                        // raster stays georeferenced by *translating* it with
                        // the drag offset when painting (below), instead of
                        // letting it rubber-band against the moving tiles.
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
                            // Record the world origin this raster is anchored
                            // to, so later pans can translate it correctly.
                            let (ox0, oy0) = view.origin_px();
                            app.radar_tex = Some(crate::app::RadarTex {
                                handle: tex,
                                sig,
                                ox: ox0,
                                oy: oy0,
                                zoom: view.zoom,
                            });
                            app.radar_last_raster = std::time::Instant::now();
                            app.radar_pending = None;
                        } else {
                            app.radar_pending = Some(sig);
                        }
                    }
                    if let Some(rt) = &app.radar_tex {
                        let uv = egui::Rect::from_min_max(
                            egui::pos2(0.0, 0.0),
                            egui::pos2(1.0, 1.0),
                        );
                        if rt.sig == sig {
                            // Current view: draw in place.
                            painter.image(rt.handle.id(), tile_resp.rect, uv, egui::Color32::WHITE);
                        } else if (rt.zoom - view.zoom).abs() < 1e-9 {
                            // Stale-by-pan only (zoom unchanged): keep the
                            // raster glued to the ground by shifting it by the
                            // world-origin delta — one image() call, no
                            // re-raster, so no rubber-banding.
                            let (ox_now, oy_now) = view.origin_px();
                            let dx = (rt.ox - ox_now) as f32;
                            let dy = (rt.oy - oy_now) as f32;
                            let moved = egui::Rect::from_min_size(
                                egui::pos2(tile_resp.rect.min.x + dx, tile_resp.rect.min.y + dy),
                                tile_resp.rect.size(),
                            );
                            painter.image(rt.handle.id(), moved, uv, egui::Color32::WHITE);
                        }
                        // Zoom changed under a deferred raster: skip the one
                        // stale frame (translation would be scale-wrong).
                    }
                }
            }
        }
    }

    // --- Layer 3.5: SPC outlook *outlines* over the radar -----------------
    // Only the risk boundaries, so they stay legible across rain echoes.
    draw_outlooks(&painter, origin, &view, app, false);

    // --- Layer 4: warning polygons ----------------------------------------
    draw_warnings(&painter, origin, &view, app);

    // --- Derived storm tracks (cell arrows + 30-min nowcast) ---------------
    if app.state.show_tracks {
        update_tracks(app);
        draw_tracks(&painter, origin, &view, app);
    }

    // --- User-placed pins (topmost map layer, above radar + warnings) -----
    draw_markers(&painter, origin, &view, app);

    // --- Attribution (required by OSM/Wikimedia tile usage policies) ------
    let show_spc = app.state.show_outlook && app.state.outlook.is_some();
    draw_attribution(&painter, tile_resp.rect, show_spc);

    // --- Scan-mode badge (precip vs clear-air) at top-center ---------------
    draw_mode_badge(ui, &painter, tile_resp.rect, app);

    // Label editor popup for the selected/new marker (drawn in its own
    // egui layer; lives here so it has the map context).
    marker_popup(ui, app);

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

/// SPC convective-outlook overlay (Day 1/2/3 categorical risk areas).
///
/// `fills == true` paints translucent fills (called *under* the radar so
/// echoes stay crisp); `fills == false` paints only the boundary outlines
/// (called *over* the radar so the risk areas stay legible across rain).
/// Colors come straight from SPC's GeoJSON, so the overlay matches their maps.
fn draw_outlooks(
    painter: &egui::Painter,
    origin: egui::Pos2,
    view: &MapView,
    app: &StormApp,
    fills: bool,
) {
    if !app.state.show_outlook {
        return;
    }
    let Some(o) = &app.state.outlook else { return };
    if o.day != app.state.outlook_day {
        return; // a newer day is on its way; don't draw a stale one
    }
    let fill_alpha = (app.state.outlook_opacity.clamp(0.0, 1.0) * 255.0).round() as u8;

    for f in o.sorted_features() {
        let stroke = egui::Color32::from_rgb(f.stroke[0], f.stroke[1], f.stroke[2]);
        for poly in &f.polygons {
            let rings: Vec<Vec<egui::Pos2>> = poly
                .iter()
                .map(|ring| project_ring(ring, view, origin))
                .collect();
            if fills {
                let fill = egui::Color32::from_rgba_unmultiplied(
                    f.fill[0],
                    f.fill[1],
                    f.fill[2],
                    fill_alpha,
                );
                if let Some(mesh) = triangulate(&rings, fill) {
                    painter.add(egui::Shape::mesh(mesh));
                }
            } else {
                for pts in &rings {
                    if pts.len() >= 3 {
                        painter.add(egui::Shape::closed_line(
                            pts.clone(),
                            egui::Stroke::new(1.6, stroke),
                        ));
                    }
                }
            }
        }
    }
}

/// Project one lon/lat ring into a screen-space ring of `Pos2`s.
fn project_ring(ring: &[(f64, f64)], view: &MapView, origin: egui::Pos2) -> Vec<egui::Pos2> {
    ring.iter()
        .map(|&(lon, lat)| {
            let (vx, vy) = view.lonlat_to_view_px(lon, lat);
            egui::pos2(origin.x + vx as f32, origin.y + vy as f32)
        })
        .collect()
}

/// Triangulate a set of rings (`[exterior, hole, ...]`) into an egui `Mesh`
/// using earcut. Outlook polygons are strongly concave, unlike most warning
/// polygons, so a fan fill (egui's `convex_polygon`) would spill outside the
/// shapes — earcut gives exact concave + holed fills.
fn triangulate(rings: &[Vec<egui::Pos2>], color: egui::Color32) -> Option<egui::Mesh> {
    if rings.is_empty() || rings[0].len() < 3 {
        return None;
    }
    let mut verts: Vec<f64> = Vec::new();
    let mut holes: Vec<usize> = Vec::new();
    let mut all: Vec<egui::Pos2> = Vec::new();
    for (i, ring) in rings.iter().enumerate() {
        // GeoJSON rings repeat the first vertex at the end; earcut wants each
        // ring open (a duplicated point makes a zero-area edge).
        let mut pts: &[egui::Pos2] = ring.as_slice();
        if pts.len() >= 2 && pts[0] == pts[pts.len() - 1] {
            pts = &pts[..pts.len() - 1];
        }
        if pts.len() < 3 {
            continue;
        }
        if i > 0 {
            holes.push(all.len());
        }
        for p in pts {
            verts.push(p.x as f64);
            verts.push(p.y as f64);
            all.push(*p);
        }
    }
    if all.len() < 3 {
        return None;
    }
    let indices = earcutr::earcut(&verts, &holes, 2).ok()?;
    if indices.is_empty() {
        return None;
    }
    let mut mesh = egui::Mesh::default();
    for p in &all {
        mesh.colored_vertex(*p, color);
    }
    for tri in indices.chunks_exact(3) {
        mesh.add_triangle(tri[0] as u32, tri[1] as u32, tri[2] as u32);
    }
    Some(mesh)
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
/// Recompute storm tracks when the frame pair or tilt changes. Cheap enough
/// to redo per frame change (~30k gate projections), but cached anyway.
fn update_tracks(app: &mut StormApp) {
    let Some(site) = radar_core::sites::site(&app.state.site_id) else {
        app.tracks_cache = None;
        return;
    };
    let ph = app.state.playhead;
    if ph == 0 || app.state.ring.len() < 2 {
        app.tracks_cache = None;
        return;
    }
    let prev = &app.state.ring[ph - 1];
    let cur = &app.state.ring[ph];
    let tilt = app.state.tilt_deg;
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    prev.volume_start.timestamp_millis().hash(&mut h);
    cur.volume_start.timestamp_millis().hash(&mut h);
    tilt.to_bits().hash(&mut h);
    let sig = h.finish();
    if app.tracks_cache.as_ref().map_or(true, |(s, _)| *s != sig) {
        let tracks = radar_render::compute_tracks(
            &prev.volume,
            &cur.volume,
            tilt,
            site.latitude,
            site.longitude,
        );
        app.tracks_cache = Some((sig, tracks));
    }
}

/// Draw storm-motion arrows from each cell centroid plus a dashed 30-minute
/// nowcast line and an mph speed label.
fn draw_tracks(painter: &egui::Painter, origin: egui::Pos2, view: &MapView, app: &StormApp) {
    const NOWCAST_MIN: f64 = 30.0;
    let Some((_, tracks)) = &app.tracks_cache else { return };
    if tracks.is_empty() {
        return;
    }
    let (w, h) = (view.width() as f64, view.height() as f64);
    let m_per_px = radar_render::meters_per_pixel(view.center_lat, view.zoom).max(1e-6);
    let ppk = 1000.0 / m_per_px; // pixels per kilometer
    let white = egui::Color32::from_white_alpha(235);
    let dim = egui::Color32::from_white_alpha(130);

    for t in tracks {
        let (vx, vy) = view.lonlat_to_view_px(t.lon, t.lat);
        if vx < -40.0 || vy < -40.0 || vx > w + 40.0 || vy > h + 40.0 {
            continue;
        }
        let pos = egui::pos2(origin.x + vx as f32, origin.y + vy as f32);

        // Motion direction in screen px (y grows south): east -> +x, north -> -y.
        let (de, dn) = (t.vel_e, t.vel_n);
        let mag = de.hypot(dn);
        if mag < 1e-9 {
            continue;
        }
        let ux = de / mag;
        let uy = -dn / mag;

        // Solid arrow, fixed 30 px length (direction over speed; speed is in
        // the label).
        let tip = egui::pos2(pos.x + (ux * 30.0) as f32, pos.y + (uy * 30.0) as f32);
        painter.line_segment([pos, tip], egui::Stroke::new(2.0, white));
        // Arrowhead.
        let (cs, sn) = (25.0f64.to_radians().cos(), 25.0f64.to_radians().sin());
        for sign in [-1.0, 1.0] {
            let hx = ux * cs - uy * sn * sign;
            let hy = ux * sn * sign + uy * cs;
            let barb = egui::pos2(
                tip.x - (hx * 9.0) as f32,
                tip.y - (hy * 9.0) as f32,
            );
            painter.line_segment([tip, barb], egui::Stroke::new(2.0, white));
        }

        // Dashed 30-minute nowcast to where the cell is headed.
        let (flat, flon) = radar_render::extrapolate(t, NOWCAST_MIN);
        let (fvx, fvy) = view.lonlat_to_view_px(flon, flat);
        let fut = egui::pos2(origin.x + fvx as f32, origin.y + fvy as f32);
        draw_dashed(painter, pos, fut, dim, 7.0, 5.0);
        painter.circle_stroke(fut, 3.5, egui::Stroke::new(1.5, dim));

        // Centroid dot + speed label (mph — US convention).
        painter.circle_filled(pos, 3.0, white);
        let mph = (t.speed_kmh * 0.621371).round() as i64;
        let text = format!("{mph} mph");
        let at = egui::pos2(pos.x + 6.0, pos.y + 6.0);
        let font = egui::FontId::proportional(11.0);
        painter.text(
            at + egui::vec2(1.0, 1.0),
            egui::Align2::LEFT_TOP,
            &text,
            font.clone(),
            egui::Color32::BLACK.gamma_multiply(0.85),
        );
        painter.text(at, egui::Align2::LEFT_TOP, &text, font, white);
    }
}

/// Dashed line helper (egui has no built-in dash stroke).
fn draw_dashed(
    painter: &egui::Painter,
    a: egui::Pos2,
    b: egui::Pos2,
    color: egui::Color32,
    dash: f32,
    gap: f32,
) {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    let period = dash + gap;
    let mut d = 0.0;
    while d < len {
        let e = (d + dash).min(len);
        painter.line_segment(
            [
                egui::pos2(a.x + ux * d, a.y + uy * d),
                egui::pos2(a.x + ux * e, a.y + uy * e),
            ],
            egui::Stroke::new(1.6, color),
        );
        d += period;
    }
}

/// Top-center badge: whether the radar is scanning in precip or clear-air
/// mode (with the VCP number). Clear-air returns are legitimately thin and
/// speckled — this badge answers "why does the radar look like dots?" at a
/// glance.
fn draw_mode_badge(ui: &egui::Ui, painter: &egui::Painter, rect: egui::Rect, app: &StormApp) {
    use radar_core::vcp::ScanMode;
    if app.state.current().is_none() {
        return; // nothing loaded yet — don't flash a confusing badge
    }
    let mode = app.state.scan_mode();
    let vcp = app.state.current().and_then(|f| f.volume.meta.vcp);
    let text = match vcp {
        Some(v) => format!("{} · VCP {v}", mode.label()),
        None => mode.label().to_string(),
    };
    let color = match mode {
        ScanMode::Precip => egui::Color32::from_rgb(125, 255, 176), // green
        ScanMode::ClearAir => egui::Color32::from_rgb(255, 209, 102), // amber
        ScanMode::Unknown => egui::Color32::from_white_alpha(190),
    };
    let font = egui::FontId::proportional(15.0);
    let galley = ui
        .ctx()
        .fonts_mut(|f| f.layout_no_wrap(text, font, color));
    let size = galley.size();
    let top = rect.min.y + 8.0;
    let center_x = rect.center().x;
    let bg = egui::Rect::from_center_size(
        egui::pos2(center_x, top + size.y / 2.0 + 5.0),
        size + egui::vec2(24.0, 12.0),
    );
    painter.rect_filled(bg, 7.0, egui::Color32::from_black_alpha(200));
    painter.rect_stroke(
        bg,
        7.0,
        egui::Stroke::new(1.0, color.gamma_multiply(0.5)),
        egui::StrokeKind::Outside,
    );
    painter.galley(
        egui::pos2(center_x - size.x / 2.0, top + 5.0),
        galley,
        color,
    );
}

/// User-placed pins: amber dot + ring with a shadowed label. Drawn above
/// radar and warnings so they stay visible during storms.
fn draw_markers(painter: &egui::Painter, origin: egui::Pos2, view: &MapView, app: &StormApp) {
    let (w, h) = (view.width() as f64, view.height() as f64);
    const PAD: f64 = 60.0;
    for m in &app.state.markers {
        let (vx, vy) = view.lonlat_to_view_px(m.lon, m.lat);
        if vx < -PAD || vy < -PAD || vx > w + PAD || vy > h + PAD {
            continue;
        }
        let pos = egui::pos2(origin.x + vx as f32, origin.y + vy as f32);
        let amber = egui::Color32::from_rgb(255, 209, 102);
        // Pin: dark-backed amber ring + solid core.
        painter.circle_filled(pos, 5.5, egui::Color32::from_black_alpha(200));
        painter.circle_filled(pos, 4.5, amber);
        painter.circle_stroke(
            pos,
            7.5,
            egui::Stroke::new(1.4, egui::Color32::from_black_alpha(180)),
        );
        // Label with a hard shadow so it reads over radar colors.
        let text = m.display();
        let at = egui::pos2(pos.x + 9.0, pos.y - 6.0);
        let font = egui::FontId::proportional(12.5);
        painter.text(
            at + egui::vec2(1.0, 1.0),
            egui::Align2::LEFT_TOP,
            &text,
            font.clone(),
            egui::Color32::BLACK.gamma_multiply(0.85),
        );
        painter.text(
            at,
            egui::Align2::LEFT_TOP,
            &text,
            font,
            amber,
        );
    }
}

/// Small modal for naming a new marker or editing an existing one.
fn marker_popup(ui: &egui::Ui, app: &mut StormApp) {
    let Some(idx) = app.state.pending_marker else { return };
    if idx >= app.state.markers.len() {
        app.state.pending_marker = None;
        return;
    }
    let is_new = app.state.markers[idx].label.trim().is_empty();
    // The TextEdit is immediate-mode: its content must persist across frames
    // in app state, or every keystroke is erased on the next repaint (it
    // showed as "one flashing letter"). Edit the draft directly.
    let draft = &mut app.state.pending_marker_label;
    let mut to_save = false;
    let mut to_delete = false;
    let mut to_cancel = false;

    egui::Window::new(if is_new { "Name this marker" } else { "Edit marker" })
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -140.0))
        .show(ui.ctx(), |ui| {
            ui.label("Label");
            let resp = ui
                .add(egui::TextEdit::singleline(draft).desired_width(260.0))
                .on_hover_text("Type a name, then press Enter or Save");
            // Grab focus automatically while naming a new pin so typing works
            // immediately (no need to click the field first).
            if is_new {
                resp.request_focus();
            }
            let typed_ok = !draft.trim().is_empty();
            if typed_ok && resp.lost_focus() {
                to_save = true; // Enter or click-away commits a non-empty name
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(typed_ok, egui::Button::new("Save")).clicked() {
                    to_save = true;
                }
                if ui.button("Delete").clicked() {
                    to_delete = true;
                }
                if ui.button("Cancel").clicked() {
                    to_cancel = true;
                }
            });
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                to_cancel = true;
            }
        });

    if to_save {
        let name = app.state.pending_marker_label.trim().to_string();
        if !name.is_empty() {
            if let Some(m) = app.state.markers.get_mut(idx) {
                m.label = name;
            }
            app.state.persist_markers();
            app.state.pending_marker = None;
            app.state.pending_marker_label.clear();
        }
    } else if to_delete {
        if idx < app.state.markers.len() {
            app.state.markers.remove(idx);
        }
        app.state.persist_markers();
        app.state.pending_marker = None;
        app.state.pending_marker_label.clear();
    } else if to_cancel {
        app.state.pending_marker = None;
        app.state.pending_marker_label.clear();
    }
}

fn draw_attribution(painter: &egui::Painter, rect: egui::Rect, spc: bool) {
    let text = if spc {
        "© OpenStreetMap contributors · SPC/NWS outlook"
    } else {
        "© OpenStreetMap contributors"
    };
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

    /// A concave polygon must come back fully triangulated (a fan fill would
    /// leak outside it — that's the whole reason we use earcut).
    #[test]
    fn triangulate_fills_concave_polygon() {
        let ring = vec![
            egui::pos2(0.0, 0.0),
            egui::pos2(10.0, 0.0),
            egui::pos2(10.0, 10.0),
            egui::pos2(5.0, 5.0),
            egui::pos2(0.0, 10.0),
        ];
        let mesh = triangulate(&[ring], egui::Color32::RED).expect("mesh");
        assert_eq!(mesh.vertices.len(), 5);
        assert!(mesh.indices.len() >= 3, "must produce at least one triangle");
        assert_eq!(mesh.indices.len() % 3, 0);
    }

    /// Exterior + hole ring must both be included and result in a valid
    /// triangulation (>=2 triangles for a square with a square hole).
    #[test]
    fn triangulate_handles_holes() {
        let outer = vec![
            egui::pos2(0.0, 0.0),
            egui::pos2(10.0, 0.0),
            egui::pos2(10.0, 10.0),
            egui::pos2(0.0, 10.0),
            egui::pos2(0.0, 0.0), // GeoJSON-style closed ring
        ];
        let hole = vec![
            egui::pos2(3.0, 3.0),
            egui::pos2(7.0, 3.0),
            egui::pos2(7.0, 7.0),
            egui::pos2(3.0, 7.0),
            egui::pos2(3.0, 3.0),
        ];
        let mesh = triangulate(&[outer, hole], egui::Color32::BLUE).expect("mesh");
        // 4 + 4 points, all referenced.
        assert_eq!(mesh.vertices.len(), 8);
        assert!(mesh.indices.len() >= 6, "square hole needs >=2 triangles");
        assert_eq!(mesh.indices.len() % 3, 0);
    }
}
