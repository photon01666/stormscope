//! Central map canvas: basemap tiles + radar overlay + site markers +
//! warning polygons, all sharing MapView's viewport-origin convention.
//!
//! Layer order (bottom -> top):
//!   1. basemap tiles          (radar_render::TileLayer)
//!   2. range rings + SPC outlook fills
//!   3. radar sweep raster
//!   4. SPC outlook outlines + warning polygons + storm tracks
//!   5. radar site dots + city labels (above data, always visible)
//!   6. user markers (above radar + warnings)
//!   7. measure tool line + pins + blue bubble (topmost, grabbable)
//!
//! The tile layer derives its rect from the current cursor (it does not
//! allocate), so every other layer must use the same origin: the viewport
//! rect returned by `TileLayer::paint`. Screen position of view pixel
//! (x, y) is `resp.rect.min + (x, y)`.

use crate::app::StormApp;
use crate::state::{MeasureDrag, MeasureEnd};
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

    // Pan: drag moves the map content with the pointer. Disabled in
    // measure mode, where drags draw the distance line instead.
    // While the name popup is open, measure clicks are ignored with the rest.
    let measuring = app.state.measure_mode && app.state.pending_marker.is_none();
    if response.dragged() && !measuring {
        let drag = response.drag_delta();
        if drag != egui::Vec2::ZERO {
            pan_view(&mut app.state.view, view, drag.x as f64, drag.y as f64);
        }
    }
    if measuring {
        measure_interaction(rect, &response, view, app);
    }

    // Map click priorities: edit an existing marker -> place a new marker
    // (Shift or "＋ Place" mode) -> select the nearest radar site. Use
    // `interact_pointer_pos()` (the press position), not `hover_pos()`, so a
    // small drag does not also count as a click.
    // While the name popup is open, ignore map clicks: clicking its Save /
    // Delete / Cancel buttons (or typing then clicking away to commit) must
    // not also drop a second pin or switch radar sites underneath it.
    // (Measure mode handles its own clicks in `measure_interaction` above.)
    if response.clicked() && !measuring && app.state.pending_marker.is_none() {
        if let Some(click) = response.interact_pointer_pos() {
            // Viewport-relative click position -> same frame of reference as
            // `draw_sites` marker positions.
            let (cx, cy) = ((click.x - rect.min.x) as f64, (click.y - rect.min.y) as f64);

            // 1) Clicking an existing pin opens its label editor.
            const MARKER_HIT_PX: f64 = 18.0;
            let mut marker_hit: Option<usize> = None;
            for (i, m) in app.state.markers.iter().enumerate() {
                let (vx, vy) = view.lonlat_to_view_px(m.lon, m.lat);
                if (vx - cx).hypot(vy - cy) <= MARKER_HIT_PX {
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
                    const SITE_HIT_PX: f64 = 14.0;
                    let mut best: Option<(&radar_core::sites::RadarSite, f64)> = None;
                    for site in radar_core::sites::SITES {
                        let (vx, vy) = view.lonlat_to_view_px(site.longitude, site.latitude);
                        let d = ((vx - cx).powi(2) + (vy - cy).powi(2)).sqrt();
                        if d <= SITE_HIT_PX && best.map_or(true, |(_, bd)| d < bd) {
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
        let pointer = response.hover_pos();
        // The Min-dBZ bar floats over the map and owns the wheel there.
        let over_min_dbz_bar = match (pointer, app.min_dbz_bar_rect) {
            (Some(p), Some(bar)) => bar.contains(p),
            _ => false,
        };
        if !over_min_dbz_bar {
            let anchor = pointer
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

    // --- Layer 2: range rings (under radar) --------------------------------
    // NOTE: radar site dots + city labels are painted late (after radar,
    // warnings and tracks) so they stay visible on top of radar data.
    draw_range_rings(&painter, origin, &view, app);

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
                    // Motion = we expect another re-raster right away (drag,
                    // playback, or a burst of wheel-zoom steps). Motion frames
                    // rasterize at half resolution (~4x fewer pixels); once
                    // motion stops, the next raster is full resolution. Both
                    // are LINEAR-filtered, so the half-res frame just looks
                    // slightly soft for a moment.
                    //
                    // The burst clock is keyed to *view* changes only: the
                    // raster scale rides in the raster signature below, so
                    // timestamping every signature change would make the
                    // scale flip itself back and forth.
                    {
                        use std::hash::{Hash, Hasher};
                        let mut vh = std::collections::hash_map::DefaultHasher::new();
                        app.state.view.zoom.to_bits().hash(&mut vh);
                        app.state.view.center_lat.to_bits().hash(&mut vh);
                        app.state.view.center_lon.to_bits().hash(&mut vh);
                        app.state.view.width().hash(&mut vh);
                        app.state.view.height().hash(&mut vh);
                        let vsig = vh.finish();
                        if vsig != app.radar_last_view_sig {
                            app.radar_last_view_sig = vsig;
                            app.radar_view_change_at = std::time::Instant::now();
                        }
                    }
                    let burst = app.radar_view_change_at.elapsed()
                        < std::time::Duration::from_millis(150);
                    let motion = response.dragged()
                        || burst
                        || app.state.loop_state == crate::state::LoopState::Playing;
                    let raster_scale: f32 = if motion { 0.5 } else { 1.0 };

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
                    raster_scale.to_bits().hash(&mut h); // scale changes re-raster
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
                            // Buffer grid for this raster (full-res, or the
                            // half-res motion grid). The estimator is always
                            // the full display view; the scale says how coarse
                            // the buffer is.
                            let bview = radar_render::scaled_view(&view, raster_scale);
                            let (rw, rh) = (bview.width(), bview.height());

                            // Geometry table: built once per sweep shape and
                            // site, reused across pans, zooms, moment
                            // switches and playback frames (rotated scan
                            // layouts remap rows instead of rebuilding).
                            let mut rows = app.radar_geom.as_ref().and_then(|g| {
                                g.reuse_for(sweep, site.latitude, site.longitude)
                            });
                            if rows.is_none() {
                                let t0 = std::time::Instant::now();
                                app.radar_geom = Some(radar_render::GateGeometry::build(
                                    sweep,
                                    site.latitude,
                                    site.longitude,
                                ));
                                rows = Some(radar_render::RowMap::Identity);
                                log::debug!("radar geometry rebuilt in {:?}", t0.elapsed());
                            }
                            let rows = rows.expect("rows set above");

                            let need = rw * rh * 4;
                            if app.radar_buf.len() != need {
                                app.radar_buf.resize(need, 0);
                            }
                            app.radar_buf.fill(0);
                            let t0 = std::time::Instant::now();
                            radar_render::rasterize_sweep_from_geometry_pm(
                                sweep,
                                app.state.moment,
                                app.radar_geom.as_ref().expect("geometry built above"),
                                &rows,
                                &view,
                                app.state.opacity,
                                min_dbz,
                                raster_scale,
                                &mut app.radar_buf,
                            );
                            let color = egui::ColorImage::from_rgba_premultiplied(
                                [rw, rh],
                                &app.radar_buf,
                            );
                            let (ox0, oy0) = view.origin_px();
                            match &mut app.radar_tex {
                                Some(rt) => {
                                    // Same GPU texture, new pixels: no
                                    // per-frame texture allocation churn.
                                    rt.handle.set(color, egui::TextureOptions::LINEAR);
                                    rt.sig = sig;
                                    rt.ox = ox0;
                                    rt.oy = oy0;
                                    rt.zoom = view.zoom;
                                }
                                None => {
                                    let tex = ui.ctx().load_texture(
                                        "stormscope-radar",
                                        color,
                                        egui::TextureOptions::LINEAR,
                                    );
                                    app.radar_tex = Some(crate::app::RadarTex {
                                        handle: tex,
                                        sig,
                                        ox: ox0,
                                        oy: oy0,
                                        zoom: view.zoom,
                                    });
                                }
                            }
                            if log::log_enabled!(log::Level::Debug) {
                                log::debug!(
                                    "radar raster {rw}x{rh} (scale {raster_scale}) in {:?}",
                                    t0.elapsed()
                                );
                            }
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
                            // Current view: draw in place (a motion raster is
                            // stretched to the viewport rect for free).
                            painter.image(rt.handle.id(), tile_resp.rect, uv, egui::Color32::WHITE);
                        } else if (rt.zoom - view.zoom).abs() < 1e-9 {
                            // Stale-by-pan only (zoom unchanged): keep the
                            // raster glued to the ground by shifting it by the
                            // world-origin delta — one image() call, no
                            // re-raster, so no rubber-banding. The texture is
                            // stretched across the full viewport rect, so one
                            // world pixel is one screen pixel regardless of
                            // the raster resolution (no scale factor here).
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
    // Tracks always refresh (marker ETAs ride on them); only the arrows are
    // gated on the toggle.
    update_tracks(app);
    update_marker_etas(app);
    if app.state.show_tracks {
        draw_tracks(&painter, origin, &view, app);
    }

    // --- Radar sites + city labels (above radar/warnings/tracks) ------------
    // Painted late so echoes never bury the dots or "ID City" names.
    draw_sites(&painter, origin, &view, app);

    // --- User-placed pins (above radar + warnings, below measure tool) ---
    draw_markers(&painter, origin, &view, app);

    // --- Measure tool line + draggable blue bubble (topmost) ---------------
    if app.state.measure_mode {
        draw_measure(&painter, origin, &view, app);
    } else {
        app.state.measure_bubble_rect = None;
    }

    // --- Attribution (required by OSM/Wikimedia tile usage policies) ------
    let show_spc = app.state.show_outlook && app.state.outlook.is_some();
    draw_attribution(&painter, tile_resp.rect, show_spc);

    // --- Scan-mode badge (precip vs clear-air) at top-center ---------------
    draw_mode_badge(ui, &painter, tile_resp.rect, app);

    // --- Min dBZ quick-adjust bar at bottom-center --------------------------
    draw_min_dbz_bar(ui.ctx(), tile_resp.rect, app);

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

/// Snap radius (viewport px) for measurement endpoints onto manual markers
/// (preferred) and radar sites.
const MEASURE_SNAP_PX: f64 = 16.0;
/// Grab radius (viewport px) for dragging a measurement endpoint pin.
const ENDPOINT_HIT_PX: f64 = 14.0;
/// Default bubble perch above the A–B midpoint, screen px.
const BUBBLE_HOME: (f32, f32) = (0.0, -36.0);

/// Viewport-relative position of a screen point (same frame of reference as
/// every overlay layer).
fn viewport_pos(rect: egui::Rect, p: egui::Pos2) -> (f64, f64) {
    ((p.x - rect.min.x) as f64, (p.y - rect.min.y) as f64)
}

/// Snap a viewport position onto the nearest manual marker (preferred) or
/// radar site within [`MEASURE_SNAP_PX`]; otherwise the raw geo position.
/// Points are (lon, lat) pairs so the helper stays pure and testable.
fn snap_measure_point(
    view: &MapView,
    markers: &[(f64, f64)],
    sites: &[(f64, f64)],
    cx: f64,
    cy: f64,
) -> (f64, f64) {
    let mut best: Option<((f64, f64), f64)> = None;
    let mut consider = |lon: f64, lat: f64| {
        let (vx, vy) = view.lonlat_to_view_px(lon, lat);
        let d = (vx - cx).hypot(vy - cy);
        if d <= MEASURE_SNAP_PX && best.map_or(true, |(_, bd)| d < bd) {
            best = Some(((lon, lat), d));
        }
    };
    // Markers first: strict `<` keeps them preferred on ties.
    for &(lon, lat) in markers {
        consider(lon, lat);
    }
    for &(lon, lat) in sites {
        consider(lon, lat);
    }
    best.map(|(p, _)| p)
        .unwrap_or_else(|| view.view_px_to_lonlat(cx, cy))
}

/// Which measurement endpoint (if any) sits under a viewport position.
/// A wins ties, so a fresh click (A == B) grows the line from there.
fn endpoint_hit(
    view: &MapView,
    a: Option<(f64, f64)>,
    b: Option<(f64, f64)>,
    cx: f64,
    cy: f64,
) -> Option<MeasureEnd> {
    let mut hit = None;
    let mut best = ENDPOINT_HIT_PX;
    for (end, pt) in [(MeasureEnd::A, a), (MeasureEnd::B, b)] {
        if let Some((lon, lat)) = pt {
            let (vx, vy) = view.lonlat_to_view_px(lon, lat);
            let d = (vx - cx).hypot(vy - cy);
            if d < best {
                best = d;
                hit = Some(end);
            }
        }
    }
    hit
}

/// Measure-mode pointer handling: bubble drag > endpoint drag > fresh line;
/// plain click toggles the bubble open or (re-)anchors A. Panning stays off
/// while this runs (gated at the call site).
fn measure_interaction(
    rect: egui::Rect,
    response: &egui::Response,
    view: MapView,
    app: &mut StormApp,
) {
    let markers: Vec<(f64, f64)> = app
        .state
        .markers
        .iter()
        .map(|m| (m.lon, m.lat))
        .collect();
    let sites: Vec<(f64, f64)> = radar_core::sites::SITES
        .iter()
        .map(|s| (s.longitude, s.latitude))
        .collect();
    let snap = |cx: f64, cy: f64| snap_measure_point(&view, &markers, &sites, cx, cy);

    // 1) Drag start: bubble drag > endpoint drag > fresh line from press.
    if response.drag_started() {
        if let Some(press) = response.interact_pointer_pos() {
            let (cx, cy) = viewport_pos(rect, press);
            let in_bubble = app
                .state
                .measure_bubble_rect
                .map_or(false, |r| r.expand(4.0).contains(press));
            if in_bubble {
                app.state.measure_drag = Some(MeasureDrag::Label);
            } else if let Some(end) =
                endpoint_hit(&view, app.state.measure_a, app.state.measure_b, cx, cy)
            {
                app.state.measure_drag = Some(MeasureDrag::End(end));
            } else {
                let p = snap(cx, cy);
                app.state.measure_a = Some(p);
                app.state.measure_b = Some(p);
                app.state.measure_drag = Some(MeasureDrag::End(MeasureEnd::B));
                app.state.measure_label_offset = None; // fresh line, home the bubble
            }
        }
    }

    // 2) Drag in flight: move the grabbed endpoint (snapping) or slide the
    // bubble by the drag delta (delta, not absolute: no jump on grab).
    if response.dragged() {
        match app.state.measure_drag {
            Some(MeasureDrag::End(end)) => {
                if let Some(hp) = response.hover_pos() {
                    let (cx, cy) = viewport_pos(rect, hp);
                    let p = snap(cx, cy);
                    match end {
                        MeasureEnd::A => app.state.measure_a = Some(p),
                        MeasureEnd::B => app.state.measure_b = Some(p),
                    }
                }
            }
            Some(MeasureDrag::Label) => {
                let d = response.drag_delta();
                if d != egui::Vec2::ZERO {
                    let (ox, oy) = app.state.measure_label_offset.unwrap_or(BUBBLE_HOME);
                    app.state.measure_label_offset = Some((ox + d.x, oy + d.y));
                }
            }
            None => {}
        }
    }

    if response.drag_stopped() {
        app.state.measure_drag = None;
    }

    // 3) Plain click: bubble tap toggles expand, else (re-)anchor A.
    if response.clicked() {
        if let Some(cp) = response.interact_pointer_pos() {
            let bubble_tap = app
                .state
                .measure_bubble_rect
                .map_or(false, |r| r.expand(4.0).contains(cp));
            if bubble_tap {
                app.state.measure_expanded = !app.state.measure_expanded;
            } else {
                let (cx, cy) = viewport_pos(rect, cp);
                let p = snap(cx, cy);
                app.state.measure_a = Some(p);
                if app.state.measure_b.is_none() {
                    app.state.measure_b = Some(p);
                }
            }
        }
    }
}

/// Miles readout for the bubble (RadarScope shows statute miles).
pub(super) fn format_miles(miles: f64) -> String {
    if miles < 100.0 {
        format!("{miles:.1} mi")
    } else {
        format!("{miles:.0} mi")
    }
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
///
/// Painted AFTER the radar/warnings/tracks layers so dots and city names
/// stay visible on top of radar data; each label gets a dark pill behind it
/// so it reads over bright echoes.
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
            // Dark halo first so the blue dot reads over any echo color.
            painter.circle_filled(pos, 8.5, egui::Color32::from_black_alpha(220));
            painter.circle_filled(pos, 6.0, egui::Color32::from_rgb(90, 180, 255));
            painter.circle_stroke(
                pos,
                9.0,
                egui::Stroke::new(1.5, egui::Color32::from_rgb(90, 180, 255)),
            );
        } else {
            painter.circle_filled(pos, 4.5, egui::Color32::from_black_alpha(220));
            painter.circle_filled(pos, 3.0, egui::Color32::from_white_alpha(235));
            painter.circle_stroke(
                pos,
                4.5,
                egui::Stroke::new(1.5, egui::Color32::from_black_alpha(200)),
            );
        }

        // "KTLX Oklahoma City" — ID + city (state stripped), on a dark pill
        // so it stays legible over radar echoes.
        let city = site.name.split(',').next().unwrap_or(site.name).trim();
        let text = format!("{} {}", site.id, city);
        let font = if selected {
            egui::FontId::proportional(13.0)
        } else {
            egui::FontId::proportional(11.5)
        };
        let fg = if selected {
            egui::Color32::from_rgb(150, 210, 255)
        } else {
            egui::Color32::from_white_alpha(235)
        };
        draw_tag_label(
            painter,
            egui::pos2(pos.x + 7.0, pos.y - 5.0),
            egui::Align2::LEFT_BOTTOM,
            &text,
            font,
            fg,
        );
    }
}

/// Dark pill-backed text label: measures the text, paints a translucent
/// black rounded rect behind it, then the foreground text. Guarantees the
/// label stays clear over bright radar echoes / warning fills.
fn draw_tag_label(
    painter: &egui::Painter,
    at: egui::Pos2,
    anchor: egui::Align2,
    text: &str,
    font: egui::FontId,
    fg: egui::Color32,
) {
    let galley = painter.layout_no_wrap(text.to_string(), font, fg);
    if galley.is_empty() {
        return;
    }
    let rect = anchor.anchor_size(at, galley.size());
    let bg = rect.expand2(egui::vec2(4.0, 2.5));
    painter.rect_filled(bg, 4.0, egui::Color32::from_black_alpha(195));
    painter.galley(rect.min, galley, fg);
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
        // Blend with the previous set (matched by proximity) so velocities
        // — and the marker ETAs built on them — settle instead of jumping
        // with every scan's sampling noise.
        let tracks = smooth_tracks(&app.prev_tracks, tracks);
        app.prev_tracks = tracks.clone();
        app.tracks_cache = Some((sig, tracks));
    }
}

/// Match radius (km) for blending a fresh track with its previous self.
const SMOOTH_MATCH_KM: f64 = 25.0;

/// Blend fresh cell velocities 50/50 with the previous set's (nearest match
/// within [`SMOOTH_MATCH_KM`], one-to-one). Frame-to-frame blob centroids
/// jitter with sampling noise; the blend is what makes ETAs converge instead
/// of swinging scan to scan.
fn smooth_tracks(
    prev: &[radar_render::StormTrack],
    fresh: Vec<radar_render::StormTrack>,
) -> Vec<radar_render::StormTrack> {
    let mut used = vec![false; prev.len()];
    fresh
        .into_iter()
        .map(|mut t| {
            let mut best: Option<(usize, f64)> = None;
            for (i, p) in prev.iter().enumerate() {
                if used[i] {
                    continue;
                }
                let d = radar_render::surface_distance_m(t.lon, t.lat, p.lon, p.lat) / 1000.0;
                if d <= SMOOTH_MATCH_KM && best.map_or(true, |(_, bd)| d < bd) {
                    best = Some((i, d));
                }
            }
            if let Some((i, _)) = best {
                used[i] = true;
                let p = &prev[i];
                t.vel_e = 0.5 * t.vel_e + 0.5 * p.vel_e;
                t.vel_n = 0.5 * t.vel_n + 0.5 * p.vel_n;
                t.speed_kmh = (t.vel_e.hypot(t.vel_n) * 60.0).round();
                t.bearing_deg = t.vel_e.atan2(t.vel_n).to_degrees().rem_euclid(360.0);
            }
            t
        })
        .collect()
}

/// Recompute every marker's arrival estimate from the current (smoothed)
/// tracks: earliest incoming cell wins. Aligned with `state.markers`.
fn update_marker_etas(app: &mut StormApp) {
    // Clone out of the cache: the borrow can't live across the assignment.
    let tracks: Vec<radar_render::StormTrack> = app
        .tracks_cache
        .as_ref()
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    app.marker_etas = app
        .state
        .markers
        .iter()
        .map(|m| {
            let mut best: Option<(f64, &radar_render::StormTrack)> = None;
            for t in &tracks {
                if let Some(e) = radar_render::eta_to_point(t, m.lon, m.lat) {
                    if best.map_or(true, |(bm, _)| e.minutes < bm) {
                        best = Some((e.minutes, t));
                    }
                }
            }
            best.map(|(minutes, t)| crate::app::MarkerEta {
                minutes,
                speed_mph: t.speed_kmh * 0.621371,
                compass: compass16(t.bearing_deg),
            })
        })
        .collect();
}

/// 16-wind compass point for a heading in degrees.
fn compass16(deg: f64) -> &'static str {
    const PTS: [&str; 16] = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW",
        "NW", "NNW",
    ];
    PTS[((deg.rem_euclid(360.0) / 22.5).round() as usize) % 16]
}

/// ETA countdown text (`14 min`, `2h 05m`).
pub(super) fn fmt_eta(minutes: f64) -> String {
    if minutes < 90.0 {
        format!("{} min", minutes.round().max(0.0) as i64)
    } else {
        let h = (minutes / 60.0).floor() as i64;
        let m = (minutes % 60.0).round() as i64;
        format!("{h}h {m:02}m")
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

/// Top-center badges: the scan-mode badge (precip vs clear-air, with VCP)
/// plus the exact age of the newest radar data. Clear-air returns are
/// legitimately thin and speckled — the mode badge answers "why does the
/// radar look like dots?" at a glance; the age badge answers "how stale is
/// this?", turning amber then red as the data ages past its scan cadence.
fn draw_mode_badge(ui: &egui::Ui, painter: &egui::Painter, rect: egui::Rect, app: &StormApp) {
    use radar_core::vcp::ScanMode;
    if app.state.current().is_none() {
        return; // nothing loaded yet — don't flash a confusing badge
    }
    // The age readout ticks every second, so keep a 1 Hz repaint coming
    // even when the loop is paused and nothing else animates.
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_secs(1));
    let mode = app.state.scan_mode();
    let vcp = app.state.current().and_then(|f| f.volume.meta.vcp);
    let mode_text = match vcp {
        Some(v) => format!("{} · VCP {v}", mode.label()),
        None => mode.label().to_string(),
    };
    let mode_color = match mode {
        ScanMode::Precip => egui::Color32::from_rgb(125, 255, 176), // green
        ScanMode::ClearAir => egui::Color32::from_rgb(255, 209, 102), // amber
        ScanMode::Unknown => egui::Color32::from_white_alpha(190),
    };

    let mut badges = vec![(mode_text, mode_color)];
    if let Some(lat) = app.state.latency() {
        let secs = lat.num_seconds().max(0);
        let age_color = match age_band(mode, secs) {
            AgeBand::Fresh => egui::Color32::from_white_alpha(220),
            AgeBand::Warn => egui::Color32::from_rgb(255, 209, 102), // amber
            AgeBand::Old => egui::Color32::from_rgb(255, 96, 96),    // red
        };
        badges.push((format!("{} old", fmt_data_age(secs)), age_color));
    }

    let font = egui::FontId::proportional(15.0);
    let galleys: Vec<_> = badges
        .iter()
        .map(|(t, c)| {
            ui.ctx()
                .fonts_mut(|f| f.layout_no_wrap(t.clone(), font.clone(), *c))
        })
        .collect();
    let pad = egui::vec2(24.0, 12.0);
    let gap = 8.0;
    let total_w: f32 = galleys.iter().map(|g| g.size().x + pad.x).sum::<f32>()
        + gap * (galleys.len().saturating_sub(1) as f32);
    let top = rect.min.y + 8.0;
    let mut x = rect.center().x - total_w / 2.0;
    for (galley, color) in galleys.iter().zip(badges.iter().map(|(_, c)| c)) {
        let size = galley.size();
        let bg = egui::Rect::from_min_size(
            egui::pos2(x, top),
            egui::vec2(size.x + pad.x, size.y + pad.y),
        );
        painter.rect_filled(bg, 7.0, egui::Color32::from_black_alpha(200));
        painter.rect_stroke(
            bg,
            7.0,
            egui::Stroke::new(1.0, color.gamma_multiply(0.5)),
            egui::StrokeKind::Outside,
        );
        painter.galley(
            egui::pos2(bg.min.x + pad.x / 2.0, top + pad.y / 2.0),
            (*galley).clone(),
            *color,
        );
        x += size.x + pad.x + gap;
    }
}

/// Freshness of the newest radar frame against its scan cadence: fresh under
/// 1.5× cadence, warning under 2.5×, old beyond. Cadences match the
/// `scan_mode` medians (precip ~5 min, clear-air ~10 min).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgeBand {
    Fresh,
    Warn,
    Old,
}

fn age_band(mode: radar_core::vcp::ScanMode, age_secs: i64) -> AgeBand {
    let cadence = match mode {
        radar_core::vcp::ScanMode::Precip => 5 * 60,
        radar_core::vcp::ScanMode::ClearAir => 10 * 60,
        radar_core::vcp::ScanMode::Unknown => 8 * 60,
    };
    if age_secs < cadence * 3 / 2 {
        AgeBand::Fresh
    } else if age_secs < cadence * 5 / 2 {
        AgeBand::Warn
    } else {
        AgeBand::Old
    }
}

/// Exact data age with seconds precision (`4m 32s`, `1h 05m`, `45s`).
fn fmt_data_age(total_secs: i64) -> String {
    if total_secs < 60 {
        format!("{total_secs}s")
    } else if total_secs < 3600 {
        format!("{}m {:02}s", total_secs / 60, total_secs % 60)
    } else {
        format!("{}h {:02}m", total_secs / 3600, (total_secs % 3600) / 60)
    }
}

/// Floating "Min dBZ (REF)" quick-adjust bar pinned to the bottom center of
/// the map: the same filter as the sidebar control, where the eye is while
/// watching echoes. Shown only for the REF-family moments it actually
/// filters — for VEL/SW/ZDR/… the slider would be dead UI.
fn draw_min_dbz_bar(ctx: &egui::Context, map_rect: egui::Rect, app: &mut StormApp) {
    use radar_core::Moment;
    if !matches!(
        app.state.moment,
        Moment::Reflectivity | Moment::CompositeReflectivity
    ) {
        app.min_dbz_bar_rect = None;
        return;
    }
    let width = 380.0;
    let pos = egui::pos2(
        map_rect.center().x - width / 2.0,
        map_rect.bottom() - 78.0,
    );
    let area = egui::Area::new(egui::Id::new("stormscope.min_dbz.bar"))
        .fixed_pos(pos)
        .movable(false)
        .interactable(true)
        .constrain(false)
        .fade_in(false)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .fill(egui::Color32::from_black_alpha(225))
                .show(ui, |ui| {
                    ui.set_min_width(width - 24.0);
                    ui.spacing_mut().slider_width = 200.0;
                    ui.horizontal(|ui| {
                        ui.label("Min dBZ (REF)");
                        ui.add(
                            egui::Slider::new(&mut app.state.min_dbz, 0.0..=75.0)
                                .step_by(5.0)
                                .drag_value_speed(1.0),
                        );
                    });
                    ui.label(
                        egui::RichText::new("filters weak returns on REF products · scroll to adjust")
                            .weak()
                            .small(),
                    );
                });
        });
    let rect = area.response.rect;
    app.min_dbz_bar_rect = Some(rect);

    // Wheel over the bar = adjust the filter (5 dB per notch), instead of
    // zooming the map underneath — the map's own zoom bails out while the
    // pointer is inside this rect.
    let hovering = ctx.pointer_hover_pos().map_or(false, |p| rect.contains(p));
    if hovering {
        let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            // One wheel notch is ~50 points in egui's smooth scroll delta.
            let notches = (scroll / 50.0).round();
            if notches != 0.0 {
                app.state.min_dbz =
                    (app.state.min_dbz + 5.0 * notches).clamp(0.0, 75.0);
            }
            // Consume it so nothing else (e.g. a scrollable ancestor) reacts.
            ctx.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
        }
    }
}

/// RadarScope-style measurement: A–B line, endpoint pins, and the draggable
/// blue bubble with the miles readout (tap to expand km + bearing).
/// Topmost tool layer so pins stay grabbable over markers.
fn draw_measure(painter: &egui::Painter, origin: egui::Pos2, view: &MapView, app: &mut StormApp) {
    let (Some(a), Some(b)) = (app.state.measure_a, app.state.measure_b) else {
        app.state.measure_bubble_rect = None;
        return;
    };
    let (ax, ay) = view.lonlat_to_view_px(a.0, a.1);
    let (bx, by) = view.lonlat_to_view_px(b.0, b.1);
    let pa = egui::pos2(origin.x + ax as f32, origin.y + ay as f32);
    let pb = egui::pos2(origin.x + bx as f32, origin.y + by as f32);

    // Line with a dark casing so it reads over echoes.
    painter.line_segment(
        [pa, pb],
        egui::Stroke::new(4.0, egui::Color32::from_black_alpha(200)),
    );
    painter.line_segment([pa, pb], egui::Stroke::new(2.0, egui::Color32::WHITE));

    // Endpoint pins (A green, B red) + amber halo when snapped to a pin/site.
    draw_measure_pin(
        painter,
        pa,
        egui::Color32::from_rgb(80, 220, 100),
        is_snapped(app, a),
    );
    draw_measure_pin(
        painter,
        pb,
        egui::Color32::from_rgb(255, 90, 90),
        is_snapped(app, b),
    );

    // Blue bubble at the midpoint (+ user drag offset).
    let meters = radar_render::surface_distance_m(a.0, a.1, b.0, b.1);
    let brg = radar_render::initial_bearing_deg(a.0, a.1, b.0, b.1);
    let mut lines: Vec<(String, egui::FontId, egui::Color32)> = vec![(
        format_miles(meters / 1609.344),
        egui::FontId::proportional(16.0),
        egui::Color32::WHITE,
    )];
    if app.state.measure_expanded {
        lines.push((
            format!("{:.1} km · brg {brg:03.0}°", meters / 1000.0),
            egui::FontId::proportional(12.0),
            egui::Color32::from_rgb(215, 235, 255),
        ));
    }
    let mid = egui::pos2((pa.x + pb.x) / 2.0, (pa.y + pb.y) / 2.0);
    let (ox, oy) = app.state.measure_label_offset.unwrap_or(BUBBLE_HOME);
    let rect = draw_bubble(painter, mid + egui::vec2(ox, oy), &lines);
    app.state.measure_bubble_rect = Some(rect);
}

/// One measurement endpoint pin with an optional snapped-on halo.
fn draw_measure_pin(
    painter: &egui::Painter,
    at: egui::Pos2,
    fill: egui::Color32,
    snapped: bool,
) {
    painter.circle_filled(at, 8.0, egui::Color32::from_black_alpha(200));
    painter.circle_filled(at, 6.0, fill);
    painter.circle_stroke(at, 6.0, egui::Stroke::new(1.5, egui::Color32::WHITE));
    if snapped {
        painter.circle_stroke(
            at,
            11.0,
            egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 209, 102)),
        );
    }
}

/// True when an endpoint sits exactly on a manual marker or radar site
/// (snapped points are copied verbatim, so exact equality holds).
fn is_snapped(app: &StormApp, pt: (f64, f64)) -> bool {
    app.state.markers.iter().any(|m| (m.lon, m.lat) == pt)
        || radar_core::sites::SITES
            .iter()
            .any(|s| (s.longitude, s.latitude) == pt)
}

/// Blue info bubble: stacked centered lines on a blue pill. Returns the
/// bubble rect for next frame's drag/tap hit-testing.
fn draw_bubble(
    painter: &egui::Painter,
    center: egui::Pos2,
    lines: &[(String, egui::FontId, egui::Color32)],
) -> egui::Rect {
    let galleys: Vec<_> = lines
        .iter()
        .map(|(t, f, c)| painter.layout_no_wrap(t.clone(), f.clone(), *c))
        .collect();
    let w = galleys
        .iter()
        .map(|g| g.size().x)
        .fold(0.0, f32::max);
    let h: f32 = galleys.iter().map(|g| g.size().y).sum();
    let pad = egui::vec2(12.0, 8.0);
    let rect = egui::Rect::from_center_size(center, egui::vec2(w, h) + pad * 2.0);
    painter.rect_filled(rect, 10.0, egui::Color32::from_rgb(10, 132, 255));
    painter.rect_stroke(
        rect,
        10.0,
        egui::Stroke::new(1.5, egui::Color32::from_rgb(6, 82, 160)),
        egui::StrokeKind::Outside,
    );
    let mut y = rect.min.y + pad.y;
    for (galley, (_, _, color)) in galleys.iter().zip(lines.iter()) {
        let x = rect.center().x - galley.size().x / 2.0;
        painter.galley(egui::pos2(x, y), galley.clone(), *color);
        y += galley.size().y;
    }
    rect
}

/// User-placed pins: amber dot + ring with a pill-backed label. Drawn above
/// radar, outlooks, warnings, tracks and site labels (below only the measure
/// tool) so dropped markers are never buried; the dark pill keeps the label
/// clear over bright echoes.
fn draw_markers(painter: &egui::Painter, origin: egui::Pos2, view: &MapView, app: &StormApp) {
    let (w, h) = (view.width() as f64, view.height() as f64);
    const PAD: f64 = 60.0;
    for (i, m) in app.state.markers.iter().enumerate() {
        let (vx, vy) = view.lonlat_to_view_px(m.lon, m.lat);
        if vx < -PAD || vy < -PAD || vx > w + PAD || vy > h + PAD {
            continue;
        }
        let pos = egui::pos2(origin.x + vx as f32, origin.y + vy as f32);
        let amber = egui::Color32::from_rgb(255, 209, 102);
        // Pin: dark-backed amber ring + solid core.
        painter.circle_filled(pos, 9.0, egui::Color32::from_black_alpha(200));
        painter.circle_filled(pos, 7.5, amber);
        painter.circle_stroke(
            pos,
            12.0,
            egui::Stroke::new(2.0, egui::Color32::from_black_alpha(180)),
        );
        // Pill-backed label so it reads over any radar color. An incoming
        // cell's ETA rides along when the tracker has one.
        let mut label = m.display();
        if let Some(eta) = app.marker_etas.get(i).and_then(|e| e.as_ref()) {
            label.push_str(&format!(" · ~{}", fmt_eta(eta.minutes)));
        }
        draw_tag_label(
            painter,
            egui::pos2(pos.x + 14.0, pos.y - 8.0),
            egui::Align2::LEFT_TOP,
            &label,
            egui::FontId::proportional(14.0),
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

    #[test]
    fn snap_prefers_manual_markers_then_sites() {
        use crate::state::MeasureEnd;
        let v = view(8.0);
        let markers = vec![(-97.0, 35.5)];
        let sites = vec![(-97.2778, 35.3331)];
        // Dead on a manual pin: snaps to it.
        let (mx, my) = v.lonlat_to_view_px(-97.0, 35.5);
        assert_eq!(
            snap_measure_point(&v, &markers, &sites, mx, my),
            (-97.0, 35.5)
        );
        // A few px off the pin: still snaps.
        assert_eq!(
            snap_measure_point(&v, &markers, &sites, mx + 5.0, my),
            (-97.0, 35.5)
        );
        // On a site (no marker near): snaps to the site.
        let (sx, sy) = v.lonlat_to_view_px(-97.2778, 35.3331);
        assert_eq!(
            snap_measure_point(&v, &[], &sites, sx, sy),
            (-97.2778, 35.3331)
        );
        // Open water: raw geo position round-trips through pixels.
        let (lon, lat) = snap_measure_point(&v, &markers, &sites, 100.0, 100.0);
        let (ex, ey) = v.lonlat_to_view_px(lon, lat);
        assert!((ex - 100.0).abs() < 1e-6 && (ey - 100.0).abs() < 1e-6);
        // Endpoint hit-testing finds the right pin.
        let a = (-97.0, 35.5);
        let b = (-96.0, 35.5);
        let (ax, ay) = v.lonlat_to_view_px(a.0, a.1);
        assert_eq!(
            endpoint_hit(&v, Some(a), Some(b), ax, ay),
            Some(MeasureEnd::A)
        );
        let (bx, by) = v.lonlat_to_view_px(b.0, b.1);
        assert_eq!(
            endpoint_hit(&v, Some(a), Some(b), bx, by),
            Some(MeasureEnd::B)
        );
        assert_eq!(endpoint_hit(&v, Some(a), Some(b), 5.0, 5.0), None);
    }

    #[test]
    fn miles_readout_uses_statute_miles() {
        // 1 degree of latitude ≈ 69.1 mi; long hauls drop the decimal.
        assert_eq!(format_miles(111_194.9 / 1609.344), "69.1 mi");
        assert_eq!(format_miles(150.0), "150 mi");
    }

    #[test]
    fn data_age_formats_with_seconds_precision() {
        assert_eq!(fmt_data_age(0), "0s");
        assert_eq!(fmt_data_age(45), "45s");
        assert_eq!(fmt_data_age(272), "4m 32s");
        assert_eq!(fmt_data_age(3600 + 300), "1h 05m");
    }

    #[test]
    fn age_bands_follow_scan_cadence() {
        use radar_core::vcp::ScanMode;
        // Precip (~5 min cadence): fresh < 7.5 min, warn < 12.5 min.
        assert_eq!(age_band(ScanMode::Precip, 60), AgeBand::Fresh);
        assert_eq!(age_band(ScanMode::Precip, 8 * 60), AgeBand::Warn);
        assert_eq!(age_band(ScanMode::Precip, 20 * 60), AgeBand::Old);
        // Clear-air (~10 min cadence): an 11-min-old frame is still fresh.
        assert_eq!(age_band(ScanMode::ClearAir, 11 * 60), AgeBand::Fresh);
        assert_eq!(age_band(ScanMode::ClearAir, 20 * 60), AgeBand::Warn);
        assert_eq!(age_band(ScanMode::ClearAir, 30 * 60), AgeBand::Old);
    }

    #[test]
    fn eta_text_is_readable() {
        assert_eq!(fmt_eta(0.4), "0 min");
        assert_eq!(fmt_eta(14.2), "14 min");
        assert_eq!(fmt_eta(89.0), "89 min");
        assert_eq!(fmt_eta(125.0), "2h 05m");
    }

    #[test]
    fn compass_points_at_16_winds() {
        assert_eq!(compass16(0.0), "N");
        assert_eq!(compass16(45.0), "NE");
        assert_eq!(compass16(90.0), "E");
        assert_eq!(compass16(180.0), "S");
        assert_eq!(compass16(270.0), "W");
        assert_eq!(compass16(359.0), "N"); // wraps
    }

    #[test]
    fn smoothing_blends_matched_velocities() {
        let mk = |lon: f64, lat: f64, ve: f64, vn: f64| radar_render::StormTrack {
            lat,
            lon,
            vel_e: ve,
            vel_n: vn,
            speed_kmh: ve.hypot(vn) * 60.0,
            bearing_deg: 0.0,
            max_dbz: 50.0,
        };
        // Fresh cell 5 km from the previous one: 50/50 blend.
        let prev = vec![mk(-97.0, 35.0, 1.0, 0.0)];
        let fresh = vec![mk(-96.95, 35.0, 1.2, 0.2)];
        let out = smooth_tracks(&prev, fresh);
        assert_eq!(out.len(), 1);
        assert!((out[0].vel_e - 1.1).abs() < 1e-9);
        assert!((out[0].vel_n - 0.1).abs() < 1e-9);
        // An unmatched cell keeps its own velocity.
        let far = vec![mk(-90.0, 40.0, 2.0, 0.0)];
        let out = smooth_tracks(&prev, far);
        assert!((out[0].vel_e - 2.0).abs() < 1e-9);
    }
}
