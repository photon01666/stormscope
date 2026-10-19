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
use crate::state::BadgeKind;
use crate::state::{MeasureDrag, MeasureEnd};
use radar_render::MapView;

/// Minimum zoom that still shows useful geography; keep the user out of
/// blank-ocean levels and out of the "texture stretched to mush" range.
const MIN_ZOOM: f64 = 3.0;
const MAX_ZOOM: f64 = 12.0;

pub fn map(ui: &mut egui::Ui, app: &mut StormApp) {
    // --- Interaction -----------------------------------------------------
    // The tile layer derives the viewport rect from the current cursor and
    // registers a hover widget over it; that same rect is reused for pan/zoom
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
    // Forecast point picking owns the click while armed: the user explicitly
    // asked for "that spot", so nothing else should react to it.
    if app.state.forecast_pick_mode && response.clicked() {
        if let Some(click) = response.interact_pointer_pos() {
            let (cx, cy) = viewport_pos(rect, click);
            let (lon, lat) = view.view_px_to_lonlat(cx, cy);
            app.state.forecast_point = Some((lat, lon));
            app.state.forecast_pick_mode = false;
            app.state.forecast = None;
            app.state.forecast_requested = None;
            let _ = app.tx.send(crate::app::DataCmd::LoadForecast(lat, lon));
        }
    } else if response.clicked() && !measuring && app.state.pending_marker.is_none() {
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
                    } else if app.state.show_lsrs
                        && let Some(r) = super::reports::lsr_at(&app.state.lsrs, &view, cx, cy)
                    {
                        // 4) A storm-report dot (drawn on top, small precise
                        // target): open its full report.
                        app.state.lsr_detail = Some(r.id.clone());
                    } else if app.state.show_totals
                        && super::totals::totals_at(&app.state.totals, &view, cx, cy).is_some()
                    {
                        // 5) A station-total dot: the marker is the shortcut,
                        // the window the table.
                        app.state.totals_open = true;
                    } else {
                        // 6) A warning polygon under the click — inside it, or
                        // on its outline (8 px of reach, so a polygon drawn
                        // under another one stays clickable where they
                        // overlap). Opens its NOAA text and lights it up (site
                        // dots win above: they are small, deliberate targets).
                        // Clicking the same spot again steps to the next
                        // warning in the stack.
                        let (lon, lat) = view.view_px_to_lonlat(cx, cy);
                        let at = egui::pos2(cx as f32, cy as f32);
                        let stack = if super::alerts::showing_live_radar(&app.state) {
                            super::alerts::warning_stack_at(
                                &app.state.alerts,
                                lon,
                                lat,
                                |lo, la| view.lonlat_to_view_px(lo, la),
                                cx,
                                cy,
                                WARNING_EDGE_PX,
                            )
                        } else {
                            Vec::new()
                        };
                        if !stack.is_empty() {
                            let ids: Vec<String> = stack.iter().map(|w| w.id.clone()).collect();
                            let idx = cycle_pick(app.map_alert_cycle.as_ref(), at, &ids);
                            let w = stack[idx];
                            app.state.alert_detail = Some(w.id.clone());
                            app.state.alerts_focus = Some(w.id.clone());
                            app.state.alerts_focus_at = ui.input(|i| i.time);
                            app.map_alert_cycle = Some((at, ids, idx));
                        } else if let Some(idx) = archive_click_target(app, &view, lon, lat, cx, cy)
                        {
                            // Historical-warning overlay: clicking one opens
                            // its archived NWS text (fetched once, on demand);
                            // the popup carries the radar-at-this-time button.
                            if let Some(w) = app.state.archive_warnings.get(idx) {
                                app.state.archive_detail = Some(w.id.clone());
                            }
                            app.map_alert_cycle = None;
                        } else {
                            // Clicked off the warnings: the next click on a
                            // stack starts from the top of it again.
                            app.map_alert_cycle = None;
                            if app.state.show_mcds
                                && let Some(m) = super::reports::mcd_at(&app.state.mcds, lon, lat)
                            {
                                // Discussion area: open the full SPC text.
                                app.state.mcd_detail = Some(m.number);
                            } else if app.state.show_outlook {
                                // Clicking an SPC outline opens that day's
                                // forecaster discussion.
                                let hit = app.state.outlook.as_ref().and_then(|o| {
                                    outlook_border_at(o, &view, lon, lat, OUTLOOK_BORDER_PX)
                                        .map(|f| f.dn)
                                });
                                if let Some(dn) = hit {
                                    app.state.outlook_detail_open = true;
                                    app.state.outlook_detail_dn = Some(dn);
                                    let day = app.state.outlook_day;
                                    if app.state.outlook_discussion_requested != Some(day) {
                                        app.state.outlook_discussion_requested = Some(day);
                                        let _ = app
                                            .tx
                                            .send(crate::app::DataCmd::LoadOutlookDiscussion(day));
                                    }
                                }
                            }
                        }
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
    // self-deadlocks the frame, starving the next frame's pointer events.
    if response.hovered() {
        // Anchor = pointer position in viewport-relative pixels.
        let pointer = response.hover_pos();
        // The Min-dBZ slider lives in the map's Controls window now; the
        // wheel belongs to the zoom wherever the pointer is over the map.
        let over_min_dbz_bar = false;
        if !over_min_dbz_bar {
            let anchor = pointer
                .map(|p| ((p.x - rect.min.x) as f64, (p.y - rect.min.y) as f64))
                .unwrap_or((rect.width() as f64 / 2.0, rect.height() as f64 / 2.0));
            let (wheel, pinch) = ui
                .ctx()
                .input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
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

    // --- Warning polygons under the pointer -------------------------------
    // Hovering a warning on the map gives the same glow + label as hovering
    // its row in the alerts browser, so a polygon can be identified by
    // pointing at it. Warnings stack — a tornado warning inside the
    // severe-thunderstorm polygon carrying it — so what comes back is an
    // ordered *stack*: the outline nearest the pointer first, then the
    // containing polygons smallest first. Every member glows and is named;
    // the first one is the click target, and clicking the same spot again
    // steps to the next. Only when the map itself is hovered (panel overlays
    // are higher layers, so this goes quiet while the pointer is on them).
    let map_pointer = response.hover_pos();
    let mut map_stack: Vec<String> = if app.state.show_alerts
        && super::alerts::showing_live_radar(&app.state)
        && !response.dragged()
    {
        map_pointer
            .map(|p| {
                let (cx, cy) = viewport_pos(rect, p);
                let (lon, lat) = view.view_px_to_lonlat(cx, cy);
                super::alerts::warning_stack_at(
                    &app.state.alerts,
                    lon,
                    lat,
                    |lo, la| view.lonlat_to_view_px(lo, la),
                    cx,
                    cy,
                    WARNING_EDGE_PX,
                )
                .into_iter()
                .map(|w| w.id.clone())
                .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    // The click's choice leads while the pointer rests there: after stepping
    // to the next warning in a stack, the highlight and the tooltip should
    // agree with the details that opened.
    if let (Some(p), Some((at, ids, idx))) = (map_pointer, app.map_alert_cycle.as_ref()) {
        if ids.as_slice() == map_stack.as_slice()
            && at.distance(p) <= STACK_CLICK_PX
            && *idx < map_stack.len()
        {
            map_stack.rotate_left(*idx);
        }
    }
    if map_stack.first() != app.state.alerts_map_stack.first() {
        // New warning (or none): restart the tooltip dwell clock.
        app.state.alerts_map_hover_at = ui.input(|i| i.time);
    }
    app.state.alerts_map_stack = map_stack;
    if !app.state.alerts_map_stack.is_empty() && !response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // Historical-warning overlay hover (the live feed always wins the
    // pointer): dwell, then the tooltip drawn with the layer below.
    let archive_set = super::alerts::archive_display_set(&app.state, archive_overlay_active(app));
    let archive_hover: Option<String> = if !archive_set.is_empty()
        && app.state.alerts_map_stack.is_empty()
        && !response.dragged()
    {
        map_pointer.and_then(|p| {
            let (cx, cy) = viewport_pos(rect, p);
            let (lon, lat) = view.view_px_to_lonlat(cx, cy);
            super::alerts::warning_stack_at(
                archive_set.iter().copied(),
                lon,
                lat,
                |lo, la| view.lonlat_to_view_px(lo, la),
                cx,
                cy,
                WARNING_EDGE_PX,
            )
            .first()
            .map(|w| w.id.clone())
        })
    } else {
        None
    };
    if archive_hover != app.state.archive_hover {
        app.state.archive_hover = archive_hover;
        app.state.archive_hover_at = ui.input(|i| i.time);
    }
    if app.state.archive_hover.is_some() && !response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // --- Storm reports / discussion areas under the pointer ---------------

    // --- Storm reports / discussion areas under the pointer ---------------
    // Reports are small dots (nearest within reach wins), discussion areas
    // are regions (containment wins); reports take precedence as the smaller
    // target. Both restart their tooltip dwell on change, like warnings.
    let report_hover = if app.state.show_lsrs && !response.dragged() {
        map_pointer.and_then(|p| {
            let (cx, cy) = viewport_pos(rect, p);
            super::reports::lsr_at(&app.state.lsrs, &view, cx, cy).map(|r| r.id.clone())
        })
    } else {
        None
    };
    if report_hover != app.state.lsr_hover {
        app.state.lsr_hover_at = ui.input(|i| i.time);
    }
    app.state.lsr_hover = report_hover;

    let totals_hover =
        if app.state.show_totals && app.state.lsr_hover.is_none() && !response.dragged() {
            map_pointer.and_then(|p| {
                let (cx, cy) = viewport_pos(rect, p);
                super::totals::totals_at(&app.state.totals, &view, cx, cy)
                    .map(|t| t.station_id.clone())
            })
        } else {
            None
        };
    if totals_hover != app.state.totals_hover {
        app.state.totals_hover_at = ui.input(|i| i.time);
    }
    app.state.totals_hover = totals_hover;

    let mcd_hover = if app.state.show_mcds
        && app.state.lsr_hover.is_none()
        && app.state.totals_hover.is_none()
        && !response.dragged()
    {
        map_pointer.and_then(|p| {
            let (cx, cy) = viewport_pos(rect, p);
            let (lon, lat) = view.view_px_to_lonlat(cx, cy);
            super::reports::mcd_at(&app.state.mcds, lon, lat).map(|m| m.number)
        })
    } else {
        None
    };
    if mcd_hover != app.state.mcd_hover {
        app.state.mcd_hover_at = ui.input(|i| i.time);
    }
    app.state.mcd_hover = mcd_hover;
    if (app.state.lsr_hover.is_some()
        || app.state.totals_hover.is_some()
        || app.state.mcd_hover.is_some())
        && !response.dragged()
    {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // SPC risk area *outline* under the pointer — only when no warning is (a
    // warning is the more specific reading of the same spot). The risk areas
    // cover most of the map, so the emphasis, the tooltip and the click all
    // key off the border.
    let outlook_hover = if app.state.alerts_map_stack.is_empty()
        && app.state.lsr_hover.is_none()
        && app.state.totals_hover.is_none()
        && app.state.mcd_hover.is_none()
        && app.state.show_outlook
    {
        response.hover_pos().and_then(|p| {
            let (cx, cy) = viewport_pos(rect, p);
            let (lon, lat) = view.view_px_to_lonlat(cx, cy);
            app.state.outlook.as_ref().and_then(|o| {
                outlook_border_at(o, &view, lon, lat, OUTLOOK_BORDER_PX).map(|f| f.dn)
            })
        })
    } else {
        None
    };
    if outlook_hover != app.state.outlook_hover {
        app.state.outlook_hover = outlook_hover;
        app.state.outlook_hover_at = ui.input(|i| i.time);
    }
    if app.state.outlook_hover.is_some() && !response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // --- Layer 1: basemap tiles ------------------------------------------
    // `TileLayer::paint` uses `ui.cursor().min` as the viewport origin, so
    // give it a child Ui pinned to the map rect. The rect it returns is the
    // viewport rect every later layer must share.
    let tile_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
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
        // Derived products are read through the display volume: the column
        // products ride in the frame, SRM comes from the motion-keyed cache.
        let volume = app.state.display_volume();
        if let (Some(volume), Some(site)) = (volume, radar_core::sites::site(&app.state.site_id)) {
            if let Some(si) =
                volume.sweep_index_for_tilt_moment(app.state.tilt_deg, app.state.moment)
            {
                let sweep = &volume.sweeps[si];
                if sweep.moment(app.state.moment).is_some() {
                    // Motion = another re-raster is expected imminently (drag,
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
                    let burst =
                        app.radar_view_change_at.elapsed() < std::time::Duration::from_millis(150);
                    // Motion includes the 3D pane's camera: while the user
                    // is flying the 3D view, an active playback loop would
                    // otherwise re-raster this pane every 125 ms tick and
                    // spike the UI thread under their fingers.
                    let motion = response.dragged()
                        || burst
                        || app.state.loop_state == crate::state::LoopState::Playing
                            && app.view3d_interact_at.elapsed()
                                < std::time::Duration::from_millis(150);
                    let raster_scale: f32 = if motion { 0.5 } else { 1.0 };

                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    frame.volume_start.timestamp_millis().hash(&mut h);
                    std::sync::Arc::as_ptr(&volume).hash(&mut h);
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
                        // Gesture throttle: a full-viewport raster is the
                        // app's most expensive op, so during *any* view
                        // gesture — pan, wheel-zoom, timeline — keep the last
                        // raster and redo it once things settle. The stale
                        // raster stays georeferenced by *translating* it with
                        // the drag offset when painting (below), instead of
                        // letting it rubber-band against the moving tiles.
                        //
                        // The measured traffic that made this unconditional:
                        // trackpad zoom emits steps at frame rate, and with a
                        // drag-only gate every step rasterized — real logs
                        // showed 16–46 rasters/s of pure churn. 100 ms caps
                        // every non-drag gesture at ~10/s; drags keep 150 ms
                        // (their translation path is cheaper and panning
                        // benefits from freshness); playback's 8 fps (125 ms
                        // spacing) stays under the window, so loops never
                        // drop a frame.
                        let settle_ms = if response.dragged() { 150 } else { 100 };
                        let settling = app.radar_last_raster.elapsed()
                            >= std::time::Duration::from_millis(settle_ms);
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
                                g.reuse_for(sweep, app.state.moment, site.latitude, site.longitude)
                            });
                            if rows.is_none() {
                                let t0 = std::time::Instant::now();
                                app.radar_geom = Some(radar_render::GateGeometry::build(
                                    sweep,
                                    app.state.moment,
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
                            let color =
                                egui::ColorImage::from_rgba_premultiplied([rw, rh], &app.radar_buf);
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
                            app.radar_raster_count += 1;
                            // Churn guard: every raster is also a full
                            // texture upload, so a signature that oscillates
                            // (something flipping across a threshold — which
                            // is what a display reconfigure can leave behind)
                            // turns the most expensive op into a per-frame
                            // one. Warn rate-limited, so the log names it the
                            // next time the machine feels half-rate.
                            let now = std::time::Instant::now();
                            let rate = prune_raster_times(
                                &mut app.radar_raster_times,
                                now,
                                std::time::Duration::from_secs(1),
                            );
                            // Playback rasters once per data frame by design,
                            // so only *idle* churn is a symptom of something.
                            let playing = app.state.loop_state == crate::state::LoopState::Playing;
                            // Above the throttle's own ceiling (~11/s), so a
                            // trip means the gate was bypassed — not normal
                            // gesture traffic.
                            if !playing
                                && rate > 12
                                && now.saturating_duration_since(app.radar_churn_warn_at)
                                    >= std::time::Duration::from_secs(2)
                            {
                                app.radar_churn_warn_at = now;
                                log::warn!(
                                    "radar raster churn: {rate}/s (dragged={}, burst={burst}, motion={motion}) — a value is oscillating",
                                    response.dragged(),
                                );
                            }
                        } else {
                            app.radar_pending = Some(sig);
                        }
                    }
                    if let Some(rt) = &app.radar_tex {
                        let uv =
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                        if rt.sig == sig {
                            // Current view: draw in place (a motion raster is
                            // stretched to the viewport rect for free).
                            painter.image(rt.handle.id(), tile_resp.rect, uv, egui::Color32::WHITE);
                        } else {
                            // Stale under the gesture throttle: show it
                            // georeferenced instead of blanking. A pan is a
                            // pure translation and a zoom is a pure scale
                            // (mercator doubles per level: 256·2^zoom), so
                            // the same raster maps to screen as
                            //     p1 = s·(p0 + o0) − o1,   s = 2^(z_now − z_raster)
                            // where o is `origin_px()` in each zoom's units.
                            // At s = 1 that is exactly the old pan-shift.
                            //
                            // This is what keeps wheel-zoom from flickering:
                            // the old code skipped any raster whose zoom was
                            // stale ("translation would be scale-wrong"), so
                            // between throttle ticks the radar was simply not
                            // drawn — and zooming blanked the layer 10–46
                            // times a second. The image is LINEAR, so an
                            // intermediate frame is a touch soft — the same
                            // trade the half-res motion frame makes — and the
                            // fresh raster lands within 100 ms.
                            let s = 2f64.powf(view.zoom - rt.zoom);
                            let (ox_now, oy_now) = view.origin_px();
                            let min = egui::pos2(
                                (tile_resp.rect.min.x as f64 + s * rt.ox - ox_now) as f32,
                                (tile_resp.rect.min.y as f64 + s * rt.oy - oy_now) as f32,
                            );
                            let size = egui::vec2(
                                (tile_resp.rect.width() as f64 * s) as f32,
                                (tile_resp.rect.height() as f64 * s) as f32,
                            );
                            painter.image(
                                rt.handle.id(),
                                egui::Rect::from_min_size(min, size),
                                uv,
                                egui::Color32::WHITE,
                            );
                        }
                    }
                }
            }
        }
    }

    // --- Layer 3.5: SPC outlook *outlines* over the radar -----------------
    // Only the risk boundaries, so they stay legible across rain echoes.
    draw_outlooks(&painter, origin, &view, app, false);

    // A derived product that is still being computed (SRM follows the motion
    // sliders) says so, instead of leaving a bare basemap.
    if app.state.moment == radar_core::Moment::StormRelativeVelocity
        && app.state.display_volume().is_none()
    {
        painter.text(
            tile_resp.rect.center(),
            egui::Align2::CENTER_CENTER,
            "deriving SRM…",
            egui::FontId::proportional(13.0),
            egui::Color32::from_gray(160),
        );
    }

    // --- Mesoscale discussion areas (broad layer, under the warnings) ----
    super::reports::draw_mcds(&painter, origin, &view, app);

    // --- Layer 4: warning polygons ----------------------------------------
    draw_warnings(&painter, origin, &view, app);
    draw_archive_warnings(&painter, origin, &view, app);

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

    // --- Storm report dots (small precise targets, on top) ----------------
    super::reports::draw_lsrs(&painter, origin, &view, app);

    // --- Station totals: diamonds with their strongest number --------------
    super::totals::draw_totals(&painter, origin, &view, app);

    // --- Measure tool line + draggable blue bubble (topmost) ---------------
    if app.state.measure_mode {
        draw_measure(&painter, origin, &view, app);
    } else {
        app.state.measure_bubble_rect = None;
    }

    // --- Attribution (required by OSM/Wikimedia tile usage policies) ------
    let show_spc = app.state.show_outlook && app.state.outlook.is_some();
    draw_attribution(&painter, tile_resp.rect, show_spc);

    // --- Top-center badge row: scan mode, scan age, alerts ---------------
    draw_top_badges(ui, &painter, tile_resp.rect, app);

    // Brief "what is this" tooltips after resting for a second: warnings
    // anywhere inside, SPC risk areas only when the pointer is on the outline
    // (clicking it opens the full discussion).
    warning_hover_tooltip(ui, &response, app);
    super::reports::lsr_hover_tooltip(ui, &response, app);
    super::reports::mcd_hover_tooltip(ui, &response, app);
    super::totals::totals_hover_tooltip(ui, &response, app);
    outlook_hover_tooltip(ui, &response, app);

    // --- Hover probe: the value under the pointer --------------------------
    // The same readout the 3D pane has, for the 2D map: the displayed
    // product's value in the gate the pixel falls in, its beam height, and
    // the distance and bearing from the radar. Suppressed while dragging (the
    // value under a moving map is noise) and in the measuring modes.
    if app.state.map_probe && !response.dragged() && !app.state.measure_mode {
        if let (Some(p), Some(site), Some(volume)) = (
            response.hover_pos(),
            radar_core::sites::site(&app.state.site_id),
            app.state.display_volume(),
        ) {
            let (cx, cy) = viewport_pos(rect, p);
            let (lon, lat) = view.view_px_to_lonlat(cx, cy);
            let min_value = match app.state.moment {
                radar_core::Moment::Reflectivity | radar_core::Moment::CompositeReflectivity => {
                    app.state.min_dbz
                }
                _ => f32::NEG_INFINITY,
            };
            if let Some(read) = map_probe_at(
                &volume,
                app.state.moment,
                app.state.tilt_deg,
                site.latitude,
                site.longitude,
                lat,
                lon,
                min_value,
            ) {
                let lines = read.lines(app.state.moment);
                draw_probe_plate(
                    &painter,
                    p,
                    &lines,
                    tile_resp.rect,
                    app.sidebar_scale.max(1.0),
                );
            }
        }
    }

    // Label editor popup for the selected/new marker (drawn in its own
    // egui layer; lives here so it has the map context).
    marker_popup(ui, app);

    // Keep animating while a loop is playing (tiles may also arrive async).
    if app.state.loop_state == crate::state::LoopState::Playing {
        ui.ctx().request_repaint();
    }
}

/// What the map's hover probe reads at one point: the displayed product's
/// value in the gate the pixel falls in (raw, like the pixel — no
/// interpolation), the beam that sampled it, the ground geometry, and — for
/// reflectivity-like products, where a column maximum means a hail core or a
/// storm top rather than a mixed-unit number — the strongest cut in the
/// column.
struct MapProbe {
    /// The echo's value under the pointer (the probe only exists over one).
    value: f32,
    /// Cut the value came from (degrees).
    elev_deg: f32,
    /// Beam height of that cut at the point (km).
    height_km: f32,
    /// Ground distance from the radar (km) and bearing (degrees).
    range_km: f64,
    bearing_deg: f64,
    /// `(value, height km)` of the strongest cut in the column.
    column_max: Option<(f32, f32)>,
}

impl MapProbe {
    /// The lines the hover plate shows, in the pane's units: the value and
    /// its tilt, then beam height in k ft and distance in miles with a
    /// compass bearing, then the column maximum when there is one.
    fn lines(&self, moment: radar_core::Moment) -> Vec<String> {
        let unit = moment.unit();
        let mut out = Vec::with_capacity(3);
        out.push(format!(
            "{} {:.0} {} · {:.1}°",
            moment.label(),
            self.value,
            unit,
            self.elev_deg
        ));
        out.push(format!(
            "{:.1}k ft · {} {}",
            self.height_km * radar_render::FT_PER_KM / 1000.0,
            format_miles(self.range_km * 0.621_371),
            compass16(self.bearing_deg)
        ));
        if let Some((v, h)) = self.column_max {
            out.push(format!(
                "column max {v:.0} {unit} @ {:.1}k ft",
                h * radar_render::FT_PER_KM / 1000.0
            ));
        }
        out
    }
}

/// Read one moment at a ground point, for the map's hover probe.
///
/// `None` — no readout — unless the point is **over an echo**, by exactly the
/// rule the rasterizer draws with: a gate with data whose value clears both
/// the color table's floor and the map's Min dBZ filter and that the table
/// does not render transparent. A probe over blank ground is noise; the
/// readout only appears where a pixel actually is.
///
/// The gate is the one the pixel falls in (nearest ray, containing range
/// bin), so the readout is what the map is actually showing rather than a
/// smoothed value; the beam height comes from the 4/3-earth model at the
/// point's ground range, and the slant range used to pick the gate is that
/// height plus the ground range.
#[allow(clippy::too_many_arguments)]
fn map_probe_at(
    volume: &radar_core::Volume,
    moment: radar_core::Moment,
    tilt_deg: f32,
    site_lat: f64,
    site_lon: f64,
    lat: f64,
    lon: f64,
    min_value: f32,
) -> Option<MapProbe> {
    let range_km = radar_render::surface_distance_m(site_lon, site_lat, lon, lat) / 1000.0;
    let bearing_deg = radar_render::initial_bearing_deg(site_lon, site_lat, lon, lat);
    let si = volume.sweep_index_for_tilt_moment(tilt_deg, moment)?;
    let sweep = &volume.sweeps[si];
    let md = sweep.moment(moment)?;
    let elevation = |s: &radar_core::Sweep| {
        if s.elevation_measured_deg.is_finite() && s.elevation_measured_deg != 0.0 {
            s.elevation_measured_deg
        } else {
            s.elevation_deg
        }
    };
    // The displayed tilt's own gate.
    let elev_deg = elevation(sweep);
    let height_km = radar_core::sampling::beam_height_km(range_km as f32, elev_deg);
    let slant_km = (range_km as f32).hypot(height_km);
    let table = radar_render::ColorTable::for_moment(moment);
    let ray = radar_core::sampling::nearest_ray_index(&sweep.azimuths_deg, bearing_deg as f32)?;
    let gate = radar_core::sampling::gate_index_at_slant(md, slant_km)?;
    let value = md.value(ray, gate)?;
    // The rasterizer's rule for "this pixel is drawn": the color table's own
    // floor, the Min dBZ filter, and the table's alpha at that value. Below
    // any of them the map shows basemap, so the probe says nothing.
    let thresh = table.range().0.max(min_value);
    if !value.is_finite() || value < thresh || table.map(value)[3] == 0 {
        return None;
    }

    // The strongest cut in the column, for the products where that reads as
    // "hail core / storm top" rather than a mixed-unit number.
    let column_max = if matches!(
        moment,
        radar_core::Moment::Reflectivity | radar_core::Moment::CompositeReflectivity
    ) {
        let mut best: Option<(f32, f32)> = None;
        for sweep in &volume.sweeps {
            let Some(md) = sweep.moment(moment) else {
                continue;
            };
            let elev = elevation(sweep);
            let h = radar_core::sampling::beam_height_km(range_km as f32, elev);
            let slant = (range_km as f32).hypot(h);
            let Some(ray) =
                radar_core::sampling::nearest_ray_index(&sweep.azimuths_deg, bearing_deg as f32)
            else {
                continue;
            };
            let Some(gate) = radar_core::sampling::gate_index_at_slant(md, slant) else {
                continue;
            };
            if let Some(v) = md.value(ray, gate) {
                if best.is_none_or(|(bv, _)| v > bv) {
                    best = Some((v, h));
                }
            }
        }
        best
    } else {
        None
    };

    Some(MapProbe {
        value,
        elev_deg,
        height_km,
        range_km,
        bearing_deg,
        column_max,
    })
}

/// Hover plate: the probe's lines on a translucent black plate so they stay
/// readable over bright echoes (the same treatment the 3D probe's readout
/// gets), clamped inside the map.
fn draw_probe_plate(
    painter: &egui::Painter,
    at: egui::Pos2,
    lines: &[String],
    map_rect: egui::Rect,
    scale: f32,
) {
    let text = lines.join("\n");
    let galley = painter.layout(
        text,
        egui::FontId::proportional(11.5 * scale),
        egui::Color32::from_rgb(235, 240, 245),
        map_rect.width() - 40.0,
    );
    let pad = egui::vec2(7.0, 5.0) * scale;
    let size = galley.size() + pad * 2.0;
    let at = egui::pos2(
        (at.x + 16.0)
            .min(map_rect.max.x - size.x - 6.0)
            .max(map_rect.min.x + 6.0),
        (at.y + 14.0)
            .min(map_rect.max.y - size.y - 6.0)
            .max(map_rect.min.y + 6.0),
    );
    let plate = egui::Rect::from_min_size(at, size);
    painter.rect_filled(plate, 4.0 * scale, egui::Color32::from_black_alpha(210));
    painter.rect_stroke(
        plate,
        4.0 * scale,
        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(46)),
        egui::StrokeKind::Inside,
    );
    painter.galley(at + pad, galley, egui::Color32::WHITE);
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
    // Deriving the anchor from a probe position instead double-counts the
    // offset and sends the view far off-target.
    let half_w = cur.width() as f64 / 2.0;
    let half_h = cur.height() as f64 / 2.0;
    let (cx, cy) =
        radar_render::merc_px_to_lonlat(ax - anchor.0 + half_w, ay - anchor.1 + half_h, new_zoom);
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
    let markers: Vec<(f64, f64)> = app.state.markers.iter().map(|m| (m.lon, m.lat)).collect();
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

    // 2) Drag in progress: move the grabbed endpoint (snapping) or slide the
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

/// Ground-range rings around the selected radar — the same **50-mile
/// landmarks** the 3D pane draws, from the same [`ring_radii_mi`] helper and
/// capped by the same data range, so the two views can never disagree about
/// how far 100 miles is. (They used to be 50 *km* rings here and 50 *mile*
/// rings in 3D: same circles, two different stories.) Labels sit on the NE
/// diagonal, like the pane's.
fn draw_range_rings(painter: &egui::Painter, origin: egui::Pos2, view: &MapView, app: &StormApp) {
    let Some(site) = radar_core::sites::site(&app.state.site_id) else {
        return;
    };
    // The displayed volume's range bounds the ring set, exactly as in 3D.
    let max_range_km = app
        .state
        .current()
        .map(|f| {
            f.volume
                .sweeps
                .iter()
                .flat_map(|s| s.moments.iter())
                .map(|m| (m.range_start_m + m.gate_count as f32 * m.gate_size_m) / 1000.0)
                .fold(0.0f32, f32::max)
        })
        .unwrap_or(0.0);
    let radii = radar_render::volume3d::ring_radii_mi(max_range_km);
    let stroke = egui::Stroke::new(1.0, egui::Color32::from_white_alpha(60));
    let font = egui::FontId::proportional(10.5);
    let (cx, cy) = view.lonlat_to_view_px(site.longitude, site.latitude);
    let view_rect = egui::Rect::from_min_size(
        origin,
        egui::vec2(view.width() as f32, view.height() as f32),
    );
    for r_mi in radii {
        let km = r_mi as f32 * radar_render::volume3d::KM_PER_MILE;
        // Screen radius of this ring (one sampled point is enough).
        let (lat, lon) =
            radar_render::gate_latlon(site.latitude, site.longitude, 0.0, 0.0, km * 1000.0);
        let (px, py) = view.lonlat_to_view_px(lon, lat);
        let radius = ((px - cx).hypot(py - cy)).max(1.0);
        let center = egui::pos2(origin.x + cx as f32, origin.y + cy as f32);
        let ring_bbox = egui::Rect::from_center_size(
            center,
            egui::vec2(radius as f32 * 2.0, radius as f32 * 2.0),
        );
        if !view_rect.intersects(ring_bbox) {
            continue; // fully off-screen (zoomed in past a ring)
        }
        // Tessellate to the *screen* radius, not a fixed 96 points: at storm
        // zoom a 50-mile ring spans hundreds of pixels and a fixed count turns
        // the circle into visible straight chords that shimmer while the map
        // pans. Sagitta kept under a third of a pixel.
        let n = ((std::f64::consts::PI / (1.0 - 0.3 / radius).max(-1.0).acos()).ceil() as usize)
            .clamp(96, 2048);
        let mut pts = Vec::with_capacity(n + 1);
        for i in 0..=n {
            let az = i as f32 * 360.0 / n as f32;
            let (lat, lon) =
                radar_render::gate_latlon(site.latitude, site.longitude, az, 0.0, km * 1000.0);
            let (vx, vy) = view.lonlat_to_view_px(lon, lat);
            pts.push(egui::pos2(origin.x + vx as f32, origin.y + vy as f32));
        }
        painter.add(egui::Shape::closed_line(pts, stroke));

        // Distance label on the NE diagonal (off the storm-track arrows and
        // the measure bubble's habitual path).
        let (lat, lon) =
            radar_render::gate_latlon(site.latitude, site.longitude, 40.0, 0.0, km * 1000.0);
        let (vx, vy) = view.lonlat_to_view_px(lon, lat);
        let at = egui::pos2(origin.x + vx as f32, origin.y + vy as f32);
        if view_rect.shrink(4.0).contains(at) {
            painter.text(
                at + egui::vec2(5.0, -4.0),
                egui::Align2::LEFT_BOTTOM,
                format!("{r_mi} mi"),
                font.clone(),
                egui::Color32::from_white_alpha(110),
            );
        }
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
pub(super) fn draw_tag_label(
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
                    f.fill[0], f.fill[1], f.fill[2], fill_alpha,
                );
                if let Some(mesh) = triangulate(&rings, fill) {
                    painter.add(egui::Shape::mesh(mesh));
                }
            } else {
                // The hovered category draws bolder so pointing at the map
                // tells you which band you are in.
                let hovered = app.state.outlook_hover == Some(f.dn);
                let width = if hovered { 3.5 } else { 1.6 };
                for pts in &rings {
                    if pts.len() >= 3 {
                        painter.add(egui::Shape::closed_line(
                            pts.clone(),
                            egui::Stroke::new(width, stroke),
                        ));
                    }
                }
            }
        }
    }
}

/// Project one lon/lat ring into a screen-space ring of `Pos2`s.
pub(super) fn project_ring(
    ring: &[(f64, f64)],
    view: &MapView,
    origin: egui::Pos2,
) -> Vec<egui::Pos2> {
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
pub(super) fn triangulate(rings: &[Vec<egui::Pos2>], color: egui::Color32) -> Option<egui::Mesh> {
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
/// Whether the historical-warning overlay belongs on screen: a day is
/// loaded that matches the selected archive day, and the user is either
/// browsing that day's list or looking at that era's radar.
pub(super) fn archive_overlay_active(app: &StormApp) -> bool {
    if !app.state.show_alerts {
        return false;
    }
    let Some(day) = app.state.archive_warnings_date else {
        return false;
    };
    if day != app.state.archive_date {
        return false;
    }
    let browsing = app.state.alerts_popup_open
        && matches!(
            app.state.alerts_tab,
            crate::state::AlertsTab::Archive | crate::state::AlertsTab::ArchiveStation
        );
    browsing || app.state.mode == crate::state::Mode::Archive
}

/// Index (into `state.archive_warnings`) of the historical polygon under a
/// map point, when the overlay is active.
fn archive_click_target(
    app: &StormApp,
    view: &MapView,
    lon: f64,
    lat: f64,
    cx: f64,
    cy: f64,
) -> Option<usize> {
    if !archive_overlay_active(app) {
        return None;
    }
    let hit = super::alerts::warning_stack_at(
        super::alerts::archive_display_set(&app.state, archive_overlay_active(app)),
        lon,
        lat,
        |lo, la| view.lonlat_to_view_px(lo, la),
        cx,
        cy,
        WARNING_EDGE_PX,
    );
    let id = hit.first()?.id.as_str();
    app.state
        .archive_warning_view
        .iter()
        .position(|w| w.id == id)
}

/// The historical warnings of the selected archive day: calm outlines
/// (the live warnings own the loud styling), a hover highlight, and a
/// dwell tooltip that names the event and what a click will do.
fn draw_archive_warnings(
    painter: &egui::Painter,
    origin: egui::Pos2,
    view: &MapView,
    app: &StormApp,
) {
    if !archive_overlay_active(app) {
        return;
    }
    let now = painter.ctx().input(|i| i.time);
    let hovered_id = app.state.archive_hover.as_deref();
    let set = super::alerts::archive_display_set(&app.state, archive_overlay_active(app));
    for w in set {
        let Some(rings) = &w.polygon else { continue };
        let hovered = hovered_id == Some(w.id.as_str());
        let color = super::alerts::warning_color(w);
        let stroke = egui::Stroke::new(
            if hovered { 2.4 } else { 1.2 },
            if hovered {
                color
            } else {
                color.gamma_multiply(0.65)
            },
        );
        for ring in rings {
            let pts = project_ring(ring, view, origin);
            if pts.len() >= 3 {
                painter.add(egui::Shape::closed_line(pts, stroke));
            }
        }
    }

    // Dwell tooltip, drawn last so it sits over the outlines.
    if let Some(id) = hovered_id
        && now - app.state.archive_hover_at >= 1.0
        && let Some(w) = app.state.archive_warning_view.iter().find(|w| w.id == id)
    {
        let tz = crate::timefmt::central();
        let Some(issued) = app
            .state
            .archive_warnings
            .iter()
            .find(|h| h.id == id)
            .map(|h| {
                h.issued
                    .with_timezone(tz)
                    .format("%b %-d %H:%M")
                    .to_string()
            })
        else {
            return;
        };
        let expires = w.expires.with_timezone(tz).format("%H:%M");
        let text = format!(
            "historical · {} · WFO {} · {} → {} CDT · click: NWS text + radar",
            w.event, w.office, issued, expires
        );
        if let Some(p) = painter.ctx().pointer_hover_pos() {
            draw_tag_label(
                painter,
                p + egui::vec2(12.0, 10.0),
                egui::Align2::LEFT_TOP,
                &text,
                egui::FontId::proportional(12.0),
                super::alerts::warning_color(w),
            );
        }
    }
}

fn draw_warnings(painter: &egui::Painter, origin: egui::Pos2, view: &MapView, app: &StormApp) {
    if !app.state.show_alerts {
        return;
    }
    // Today's warnings over a past scan would be a sync lie: in archive mode
    // the archive day's polygons (time-filtered) are the warning layer.
    if !super::alerts::showing_live_radar(&app.state) {
        return;
    }
    // egui time drives the flash; request_repaint below keeps it animating.
    let now = painter.ctx().input(|i| i.time);
    let mut animating = false;

    for w in &app.state.alerts {
        let Some(rings) = &w.polygon else { continue };
        let color = super::alerts::warning_color(w);

        // Glow strength: 1.0 while the row is hovered in the alerts browser,
        // 0.8 for the warning the map pointer is on (a touch gentler —
        // pointing at the map should hint, not shout), 0.35 for the rest of a
        // stack the pointer is standing on (so overlapping warnings all light
        // up while the one a click opens is unmistakable), and a flash that
        // settles into a steady highlight for the alert last jumped to (so
        // the clicked one stays identifiable).
        let hovered_list = app.state.alerts_hover.as_deref() == Some(w.id.as_str());
        let stack_pos = app.state.alerts_map_stack.iter().position(|id| id == &w.id);
        let focus_fade = if app.state.alerts_focus.as_deref() == Some(w.id.as_str()) {
            let age = (now - app.state.alerts_focus_at).max(0.0);
            Some((1.0 - age / 3.0).clamp(0.0, 1.0))
        } else {
            None
        };
        let glow: f32 = if hovered_list {
            1.0
        } else if let Some(pos) = stack_pos {
            if pos == 0 { 0.8 } else { 0.35 }
        } else {
            focus_fade.map_or(0.0, |f| 0.35 + 0.65 * f) as f32
        };
        let pulse = (0.5 + 0.5 * (now * 5.0).sin()) as f32;
        let intensity = glow * (0.65 + 0.35 * pulse);
        if glow > 0.0 {
            animating = true;
        }

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
            // Soft outer glow: two widening, fading strokes behind the edge.
            if glow > 0.0 {
                for (width, alpha) in [(6.0, 0.30), (11.0, 0.15)] {
                    painter.add(egui::Shape::closed_line(
                        pts.clone(),
                        egui::Stroke::new(width, color.gamma_multiply(alpha * intensity)),
                    ));
                }
            }
            painter.add(egui::Shape::closed_line(
                pts.clone(),
                egui::Stroke::new(2.0 + 1.5 * intensity, color),
            ));
            // The fill stays well under the outline's weight: overlapping
            // warnings multiply their washes, and a heavy tint over the radar
            // hides the thing the map is for. The border carries the color.
            painter.add(egui::Shape::convex_polygon(
                pts,
                color.gamma_multiply(warning_fill_alpha(intensity, app.state.alerts_fill)),
                egui::Stroke::NONE,
            ));
        }

        // Name the warning on the map only while the pointer is on it (the
        // map directly, or its row in the alerts browser): the label answers
        // "which polygon is this?". A clicked alert keeps its afterglow
        // without leaving a name floating over the map once the pointer has
        // moved on.
        if hovered_list || stack_pos.is_some() {
            if let Some((lat, lon)) = w.centroid() {
                let (vx, vy) = view.lonlat_to_view_px(lon, lat);
                let at = egui::pos2(origin.x + vx as f32, origin.y + vy as f32);
                draw_tag_label(
                    painter,
                    at,
                    egui::Align2::CENTER_CENTER,
                    &w.event,
                    egui::FontId::proportional(13.0),
                    color,
                );
            }
        }
    }

    if animating {
        // Keep the pulse (and the focus flash decay) moving.
        painter.ctx().request_repaint();
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
pub(super) fn compass16(deg: f64) -> &'static str {
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
    let Some((_, tracks)) = &app.tracks_cache else {
        return;
    };
    if tracks.is_empty() {
        return;
    }
    let (w, h) = (view.width() as f64, view.height() as f64);
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
            let barb = egui::pos2(tip.x - (hx * 9.0) as f32, tip.y - (hy * 9.0) as f32);
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
/// Top-center badge row: scan mode (precip vs clear-air, with VCP), the age
/// of the newest scan, and the alerts count.
///
/// The alerts badge is the entry point to the alerts browser. When the
/// selected station has a watch or warning in force nearby, the badge
/// flashes in that alert's family color (dimmed for watches), so an issued
/// watch/warning is visible without opening anything.
fn draw_top_badges(ui: &egui::Ui, painter: &egui::Painter, rect: egui::Rect, app: &mut StormApp) {
    use radar_core::vcp::ScanMode;
    // The age readout ticks every second, so keep a 1 Hz repaint coming even
    // when the loop is paused and nothing else animates.
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_secs(1));

    let mut badges: Vec<TopBadge> = Vec::new();
    if app.state.current().is_some() {
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
        badges.push(TopBadge {
            text: mode_text,
            color: mode_color,
            kind: BadgeKind::Mode,
            pulse: false,
        });

        if let Some(lat) = app.state.latency() {
            let secs = lat.num_seconds().max(0);
            let age_color = match age_band(mode, secs) {
                AgeBand::Fresh => egui::Color32::from_white_alpha(220),
                AgeBand::Warn => egui::Color32::from_rgb(255, 209, 102), // amber
                AgeBand::Old => egui::Color32::from_rgb(255, 96, 96),    // red
            };
            badges.push(TopBadge {
                text: format!("{} old", fmt_data_age(secs)),
                color: age_color,
                kind: BadgeKind::Age,
                pulse: false,
            });
        }
    }

    // Alerts: the count is nationwide (nothing hidden); the flash is for
    // watches/warnings near the *selected station*.
    let alert_count = if app.state.all_alerts.is_empty() {
        app.state.alerts.len()
    } else {
        app.state.all_alerts.len()
    };
    // Copy out what the badge needs (event name + color): the app is mutated
    // further down for the pill's click handling.
    //
    // Two sources of "worth flashing": the fast lane (the API says the alert
    // applies to the station — this is the only way a zone-based alert, which
    // has no polygon and therefore no centroid, can flash) and the wider
    // sweep filtered by proximity. Most severe across both wins.
    let significant: Option<(String, egui::Color32)> = {
        let fast_best = app
            .state
            .alerts_fast
            .iter()
            .filter_map(|w| super::alerts::significant_priority(w).map(|p| (p, w)))
            .max_by_key(|(p, _)| *p);
        let slow_best = radar_core::sites::site(&app.state.site_id)
            .and_then(|s| {
                super::alerts::significant_near(
                    &app.state.alerts,
                    s.latitude,
                    s.longitude,
                    crate::state::STATION_ALERT_RADIUS_KM,
                )
            })
            .and_then(|w| super::alerts::significant_priority(w).map(|p| (p, w)));
        [fast_best, slow_best]
            .into_iter()
            .flatten()
            .max_by_key(|(p, _)| *p)
            .map(|(_, w)| (w.event.clone(), super::alerts::pulse_color(w)))
    };
    let alerts_color = significant
        .as_ref()
        .map_or(egui::Color32::from_white_alpha(220), |(_, c)| *c);
    badges.push(TopBadge {
        text: format!("⚠ {alert_count} alerts"),
        color: alerts_color,
        kind: BadgeKind::Alerts,
        pulse: significant.is_some(),
    });

    // Forecast for the selected station (or a picked point).
    badges.push(TopBadge {
        text: "📈 Forecast".to_string(),
        color: egui::Color32::from_white_alpha(220),
        kind: BadgeKind::Forecast,
        pulse: false,
    });

    // Back to live radar, right next to Forecast: red and pulsing while
    // browsing archive/replay (it wants the click), dim green "● LIVE" as a
    // status pill while already on the newest scan.
    let live = app.state.mode == crate::state::Mode::Live
        && app.state.playhead + 1 >= app.state.ring.len().max(1);
    badges.push(TopBadge {
        text: if live {
            "● LIVE".to_string()
        } else {
            "GO LIVE".to_string()
        },
        color: if live {
            egui::Color32::from_rgb(120, 230, 140)
        } else {
            egui::Color32::from_rgb(255, 130, 130)
        },
        kind: BadgeKind::Live,
        pulse: !live,
    });

    // Flash phase for pulsing badges (also drives the repaint below).
    let now = ui.ctx().input(|i| i.time);
    let flash = (0.55 + 0.45 * (now * 4.0).sin()) as f32;
    if badges.iter().any(|b| b.pulse) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(120));
    }

    let font = egui::FontId::proportional(15.0);
    let galleys: Vec<_> = badges
        .iter()
        .map(|b| {
            ui.ctx()
                .fonts_mut(|f| f.layout_no_wrap(b.text.clone(), font.clone(), b.color))
        })
        .collect();
    let pad = egui::vec2(24.0, 12.0);
    let gap = 8.0;
    let total_w: f32 =
        galleys.iter().map(|g| g.size().x + pad.x).sum::<f32>() + gap * (galleys.len() - 1) as f32;
    let top = rect.min.y + 8.0;
    let mut x = rect.center().x - total_w / 2.0;
    let mut rects: Vec<(BadgeKind, egui::Rect)> = Vec::new();
    for (badge, galley) in badges.iter().zip(galleys.iter()) {
        let size = galley.size();
        let bg = egui::Rect::from_min_size(
            egui::pos2(x, top),
            egui::vec2(size.x + pad.x, size.y + pad.y),
        );
        let hovered = app.top_badge_hovered == Some(badge.kind);
        painter.rect_filled(bg, 7.0, egui::Color32::from_black_alpha(200));
        if badge.pulse {
            // Flash: a glow ring whose width and opacity follow the phase.
            painter.rect_stroke(
                bg.expand(1.0 + 2.0 * flash),
                9.0,
                egui::Stroke::new(2.0, badge.color.gamma_multiply(0.35 + 0.5 * flash)),
                egui::StrokeKind::Outside,
            );
        }
        if !hovered {
            painter.rect_stroke(
                bg,
                7.0,
                egui::Stroke::new(
                    if badge.pulse { 1.8 } else { 1.0 },
                    badge.color.gamma_multiply(0.5),
                ),
                egui::StrokeKind::Outside,
            );
        }
        painter.galley(
            egui::pos2(bg.min.x + pad.x / 2.0, top + pad.y / 2.0),
            (*galley).clone(),
            badge.color,
        );
        rects.push((badge.kind, bg));
        x += size.x + pad.x + gap;
    }

    // Interactive pills: alerts opens the browser, forecast opens the window.
    let mut hovered_kind = None;
    for (kind, bg) in rects {
        if !matches!(
            kind,
            BadgeKind::Alerts | BadgeKind::Forecast | BadgeKind::Live
        ) {
            continue;
        }
        let resp = ui.interact(
            bg,
            egui::Id::new(("stormscope.topbadge", badge_id(kind))),
            egui::Sense::click(),
        );
        if resp.hovered() {
            hovered_kind = Some(kind);
        }
        let hover_text = match kind {
            BadgeKind::Alerts => match &significant {
                Some((event, _)) => format!(
                    "{event} in force near {} — click for the alerts browser",
                    app.state.site_id
                ),
                None => format!("{alert_count} active alerts — click to browse"),
            },
            BadgeKind::Forecast => {
                "NWS forecast for the selected station — click to open".to_string()
            }
            BadgeKind::Live => {
                if live {
                    "Watching the newest scan".to_string()
                } else {
                    "Leave archive/replay and snap to the newest radar (L)".to_string()
                }
            }
            _ => String::new(),
        };
        if resp.on_hover_text(hover_text).clicked() {
            match kind {
                BadgeKind::Alerts => app.state.alerts_popup_open = true,
                BadgeKind::Forecast => {
                    app.state.forecast_open = true;
                    // Fetch now if what is loaded is not what the window will
                    // show (station follow, or a stale point).
                    super::forecast::request_forecast_if_stale(app);
                }
                BadgeKind::Live => {
                    if !live {
                        app.snap_to_live();
                    }
                }
                _ => {}
            }
        }
    }
    app.top_badge_hovered = hovered_kind;
}

fn badge_id(kind: BadgeKind) -> &'static str {
    match kind {
        BadgeKind::Mode => "mode",
        BadgeKind::Age => "age",
        BadgeKind::Alerts => "alerts",
        BadgeKind::Forecast => "forecast",
        BadgeKind::Live => "live",
    }
}

/// One pill in the top-center badge row.
struct TopBadge {
    text: String,
    color: egui::Color32,
    kind: BadgeKind,
    /// Flash this pill (a watch/warning is in force near the station).
    pulse: bool,
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

/// Notches (steps) of wheel input in `events`, consuming them and any
/// pointer-unit debris (`acc`, in points) as it goes.
///
/// Working from the raw events — not egui's `smooth_scroll_delta` — is what
/// makes one notch equal one step: egui spreads a notch across frames for
/// smooth scrolling, so any threshold on the smoothed value drops most
/// notches and the wheel feels like it needs several flicks per tick.
pub(super) fn consume_wheel_steps(events: &mut Vec<egui::Event>, acc: &mut f32) -> f32 {
    let mut steps = 0.0f32;
    events.retain(|e| {
        if let egui::Event::MouseWheel { unit, delta, .. } = e {
            match unit {
                egui::MouseWheelUnit::Line | egui::MouseWheelUnit::Page => steps += delta.y,
                // Point-unit devices (touchpads) deliver fractions of a
                // notch; accumulate until they cover one (~50 points).
                egui::MouseWheelUnit::Point => *acc += delta.y,
            }
            false // consumed: nothing else (the map) should see the wheel
        } else {
            true
        }
    });
    let extra = (*acc / 50.0).trunc();
    *acc -= extra * 50.0;
    steps + extra
}

/// Tooltip for the warning polygons under the pointer, shown once the pointer
/// has rested on them for a second (a passing pointer should not pop text).
///
/// Contents are the brief form of the leading warning — event, issuer, expiry,
/// distance, affected area and the opening line of the NOAA text — plus, when
/// several warnings are stacked at that spot, the names of the ones behind it,
/// since those are exactly the ones a click can reach next.
fn warning_hover_tooltip(ui: &egui::Ui, response: &egui::Response, app: &StormApp) {
    let Some(id) = app.state.alerts_map_stack.first() else {
        return;
    };
    let dwell = ui.input(|i| i.time) - app.state.alerts_map_hover_at;
    if dwell < 1.0 {
        return;
    }
    let Some(w) = app.state.alerts.iter().find(|w| &w.id == id) else {
        return;
    };
    let behind: Vec<&str> = app
        .state
        .alerts_map_stack
        .iter()
        .skip(1)
        .filter_map(|id| {
            app.state
                .alerts
                .iter()
                .find(|w| &w.id == id)
                .map(|w| w.event.as_str())
        })
        .collect();
    let (site_lat, site_lon) = radar_core::sites::site(&app.state.site_id)
        .map(|s| (s.latitude, s.longitude))
        .unwrap_or((35.3331, -97.2778));
    egui::containers::Tooltip::for_widget(response)
        .at_pointer()
        .show(|ui| {
            // Tooltips live in their own layer, so they do not inherit the
            // per-panel scaling; give the text the same monitor-aware scale
            // as the sidebar and the windows.
            super::apply_text_scale(ui, app.sidebar_scale.max(1.0));
            super::alerts::brief_alert_ui(ui, w, site_lat, site_lon, &app.state.site_id);
            if !behind.is_empty() {
                ui.label(
                    egui::RichText::new(format!("stacked here as well: {}", behind.join(", ")))
                        .weak()
                        .small(),
                );
            }
            ui.label(
                egui::RichText::new(if behind.is_empty() {
                    "click for the full NOAA text"
                } else {
                    "click for the full NOAA text · click again for the next one"
                })
                .weak()
                .small(),
            );
        });
}

/// How close (viewport px) the pointer must be to an SPC outline for the
/// hover emphasis, the brief tooltip and the click target: all three share
/// one reach, so what lights up is what a click will open.
pub(super) const OUTLOOK_BORDER_PX: f64 = 8.0;

/// Border of the SPC risk area under the pointer: a click within
/// `tolerance_px` of an outline opens that day's discussion. Risk areas nest,
/// so the most severe one within tolerance wins (ties go to the closer edge).
pub(super) fn outlook_border_at<'a>(
    o: &'a radar_core::outlooks::Outlook,
    view: &MapView,
    lon: f64,
    lat: f64,
    tolerance_px: f64,
) -> Option<&'a radar_core::outlooks::OutlookFeature> {
    let (px, py) = view.lonlat_to_view_px(lon, lat);
    let mut best: Option<(&radar_core::outlooks::OutlookFeature, f64)> = None;
    for f in &o.features {
        if f.risk_level().is_none() && f.dn != 2 {
            continue;
        }
        let mut nearest = f64::INFINITY;
        for poly in &f.polygons {
            for ring in poly {
                if ring.len() < 2 {
                    continue;
                }
                // Walk every edge including the closing one: the last vertex
                // to the first is a real edge of the outline and clicking it
                // must work like any other.
                for i in 0..ring.len() {
                    let a = ring[i];
                    let b = ring[(i + 1) % ring.len()];
                    let (ax, ay) = view.lonlat_to_view_px(a.0, a.1);
                    let (bx, by) = view.lonlat_to_view_px(b.0, b.1);
                    nearest = nearest.min(point_segment_distance(px, py, ax, ay, bx, by));
                }
            }
        }
        if nearest > tolerance_px {
            continue;
        }
        let level = f.risk_level().unwrap_or(0);
        let better = match best {
            Some((bf, bd)) => (level, -nearest) > (bf.risk_level().unwrap_or(0), -bd),
            None => true,
        };
        if better {
            best = Some((f, nearest));
        }
    }
    best.map(|(f, _)| f)
}

/// Raster timestamps inside the churn window: push `now`, drop what fell out,
/// and report how many are left — the count behind the churn warning.
fn prune_raster_times(
    times: &mut std::collections::VecDeque<std::time::Instant>,
    now: std::time::Instant,
    window: std::time::Duration,
) -> usize {
    times.push_back(now);
    while times
        .front()
        .is_some_and(|t| now.duration_since(*t) > window)
    {
        times.pop_front();
    }
    times.len()
}

/// Distance from a point to a line segment, in the same (pixel) units.
pub(super) fn point_segment_distance(px: f64, py: f64, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    if len2 <= f64::EPSILON {
        return (px - ax).hypot(py - ay);
    }
    let t = (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0);
    (px - (ax + t * dx)).hypot(py - (ay + t * dy))
}

/// Alpha of a warning polygon's fill: the shipped wash (5% at rest, 22%
/// under the pointer) scaled by the user's fill setting, where 0% leaves the
/// outline alone on the map. The 3D pane's ground polygons share it, so a
/// warning washes out the same in both views.
pub(super) fn warning_fill_alpha(intensity: f32, user_fill: f32) -> f32 {
    (0.05 + 0.17 * intensity) * user_fill.clamp(0.0, 1.0)
}

/// How close (viewport px) the pointer must be to a warning polygon's outline
/// for the hover and the click to treat it as aimed at that polygon: the same
/// 8 px reach the SPC outlines use (`OUTLOOK_BORDER_PX`), so "what lights up is
/// what a click opens" holds for both. It is what keeps a warning drawn under
/// another one reachable where their borders cross.
pub(super) const WARNING_EDGE_PX: f64 = 8.0;

/// How close (px) two clicks must be to count as "the same spot" when walking
/// a stack of overlapping warnings.
const STACK_CLICK_PX: f32 = 6.0;

/// Which member of the warning stack under the pointer a click selects.
///
/// The first click at a spot takes the most clickable member (see
/// `warning_stack_at`); clicking the same spot again steps to the next one, so
/// every polygon in a stack can be opened even where they fully cover each
/// other. Clicking anywhere else starts over, and so does a stack that has
/// changed under the pointer — a new warning arriving must not reshuffle what
/// the next click opens.
fn cycle_pick(
    prev: Option<&(egui::Pos2, Vec<String>, usize)>,
    pos: egui::Pos2,
    ids: &[String],
) -> usize {
    if ids.is_empty() {
        return 0;
    }
    match prev {
        Some((at, prev_ids, idx)) if at.distance(pos) <= STACK_CLICK_PX && prev_ids == ids => {
            (idx + 1) % ids.len()
        }
        _ => 0,
    }
}

/// Tooltip for the SPC outline under the pointer: what that risk level means
/// in the NWS's wording, after the same one-second dwell the warnings use.
/// Fires only on the border (see the hover state), so it never covers the map
/// while the pointer is merely inside a risk area.
fn outlook_hover_tooltip(ui: &egui::Ui, response: &egui::Response, app: &StormApp) {
    let Some(dn) = app.state.outlook_hover else {
        return;
    };
    if ui.input(|i| i.time) - app.state.outlook_hover_at < 1.0 {
        return;
    }
    let Some(o) = app.state.outlook.as_ref() else {
        return;
    };
    let Some(f) = o.features.iter().find(|f| f.dn == dn) else {
        return;
    };
    let color = egui::Color32::from_rgb(f.stroke[0], f.stroke[1], f.stroke[2]);
    egui::containers::Tooltip::for_widget(response)
        .at_pointer()
        .show(|ui| {
            super::apply_text_scale(ui, app.sidebar_scale.max(1.0));
            let prob = o.ty.is_probability();
            ui.horizontal(|ui| {
                ui.colored_label(color, "●");
                // headline()/description() are categorical wording; a
                // probability feature's identity is SPC's LABEL2 ("2% Tornado
                // Risk") and its meaning is the 25-mile definition below.
                ui.strong(if prob {
                    if f.name.is_empty() {
                        f.probability_text()
                    } else {
                        f.name.clone()
                    }
                } else {
                    f.headline()
                });
            });
            if prob {
                let def = f
                    .label
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|v| (0.0..=1.0).contains(v))
                    .map(|v| {
                        format!(
                            "{:.0}% chance of {} within 25 miles of a point",
                            v * 100.0,
                            o.ty.noun()
                        )
                    });
                ui.label(egui::RichText::new(def.unwrap_or_else(|| f.label.clone())).small());
            } else {
                ui.label(egui::RichText::new(f.description()).small());
            }
            let mut meta = vec![if prob {
                format!("SPC Day {} {} probability outlook", o.day, o.ty.label())
            } else {
                format!("SPC Day {} categorical outlook", o.day)
            }];
            if !o.forecaster.is_empty() {
                meta.push(o.forecaster.clone());
            }
            if let Some(expire) = o.expire {
                meta.push(format!(
                    "valid until {}",
                    crate::timefmt::fmt_central(&expire)
                ));
            }
            ui.label(egui::RichText::new(meta.join(" · ")).weak().small());
            ui.label(
                egui::RichText::new("click the outline for the full discussion")
                    .weak()
                    .small(),
            );
        });
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
fn draw_measure_pin(painter: &egui::Painter, at: egui::Pos2, fill: egui::Color32, snapped: bool) {
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
    let w = galleys.iter().map(|g| g.size().x).fold(0.0, f32::max);
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
    let Some(idx) = app.state.pending_marker else {
        return;
    };
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

    egui::Window::new(if is_new {
        "Name this marker"
    } else {
        "Edit marker"
    })
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
            if ui
                .add_enabled(typed_ok, egui::Button::new("Save"))
                .clicked()
            {
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

    /// The map's hover probe reads the gate the pixel is in, reports the beam
    /// height and the geometry, and finds the column maximum for
    /// reflectivity-like products.
    #[test]
    fn hover_probe_reads_the_displayed_gate() {
        use radar_core::{MomentCalibration, MomentData, Sweep, Volume, VolumeMeta};
        let cal = MomentCalibration {
            scale: 1.0,
            offset: 2.0,
            threshold: 0.0,
        };
        // 50 km of 250 m gates: a 45 dBZ core inside 40 km, 10 beyond (and
        // 55 dBZ on the upper cut, to have a column maximum to find).
        let mk = |elev: f32, inner: f32| {
            let rays: Vec<f32> = (0..360).map(|i| i as f32).collect();
            let mut data = vec![0i16; 360 * 200];
            for r in 0..360 {
                for g in 0..200 {
                    let range_km = (g as f32 + 0.5) * 0.25;
                    let dbz = if range_km < 40.0 { inner } else { 10.0 };
                    data[r * 200 + g] = (dbz + 2.0) as i16;
                }
            }
            Sweep {
                elevation_deg: elev,
                elevation_measured_deg: elev,
                azimuth_width_deg: 1.0,
                azimuths_deg: rays,
                moments: vec![MomentData {
                    kind: radar_core::Moment::Reflectivity,
                    calibration: cal,
                    range_start_m: 0.0,
                    gate_size_m: 250.0,
                    gate_count: 200,
                    ray_count: 360,
                    data,
                }],
            }
        };
        let vol = Volume {
            meta: VolumeMeta {
                site_id: "KTWX".into(),
                volume_start: chrono::Utc::now(),
                vcp: None,
                sequence_number: None,
                sweep_count: 2,
                moments: vec![radar_core::Moment::Reflectivity],
            },
            sweeps: vec![mk(0.5, 45.0), mk(1.5, 55.0)],
        };
        let (slat, slon) = (39.0f64, -96.2f64);
        let at = |km_north: f64| slat + km_north / 111.0;

        let read = map_probe_at(
            &vol,
            radar_core::Moment::Reflectivity,
            0.5,
            slat,
            slon,
            at(30.0),
            slon,
            0.0,
        )
        .expect("a readout over the core");
        assert!(
            (read.range_km - 30.0).abs() < 1.0,
            "range {}",
            read.range_km
        );
        assert!(
            read.bearing_deg.abs() < 1.0 || (read.bearing_deg - 360.0).abs() < 1.0,
            "bearing {}",
            read.bearing_deg
        );
        assert_eq!(read.value, 45.0, "the pixel's own gate");
        assert!(
            (read.height_km - radar_core::sampling::beam_height_km(30.0, 0.5)).abs() < 0.05,
            "beam height {}",
            read.height_km
        );
        let (v, h) = read.column_max.expect("column maximum");
        assert_eq!(v, 55.0, "the strongest cut");
        assert!(h > read.height_km, "it comes from higher up");

        // Weak returns past the core: the value says so, it is not "nothing".
        let far = map_probe_at(
            &vol,
            radar_core::Moment::Reflectivity,
            0.5,
            slat,
            slon,
            at(45.0),
            slon,
            0.0,
        )
        .unwrap();
        assert_eq!(far.value, 10.0);

        // No echo under the pointer, no readout: past the volume's range...
        assert!(
            map_probe_at(
                &vol,
                radar_core::Moment::Reflectivity,
                0.5,
                slat,
                slon,
                at(555.0),
                slon,
                0.0,
            )
            .is_none(),
            "beyond the data"
        );
        // ...and where the Min dBZ filter hides the echo the map draws.
        assert!(
            map_probe_at(
                &vol,
                radar_core::Moment::Reflectivity,
                0.5,
                slat,
                slon,
                at(45.0),
                slon,
                20.0,
            )
            .is_none(),
            "below the map's own filter"
        );

        // The lines are in the pane's units (k ft, miles, compass).
        let lines = read.lines(radar_core::Moment::Reflectivity);
        assert!(lines[0].starts_with("REF 45 dBZ"), "{lines:?}");
        assert!(
            lines[1].contains("k ft") && lines[1].contains("mi "),
            "{lines:?}"
        );
        assert!(lines[2].starts_with("column max 55"), "{lines:?}");

        // A product this volume does not carry: no readout at all.
        assert!(
            map_probe_at(
                &vol,
                radar_core::Moment::Velocity,
                0.5,
                slat,
                slon,
                at(30.0),
                slon,
                0.0,
            )
            .is_none()
        );
    }

    fn view(zoom: f64) -> MapView {
        MapView {
            center_lat: 35.3331,
            center_lon: -97.2778,
            zoom,
            width_px: 1200.0,
            height_px: 800.0,
        }
    }

    /// Clicking an SPC outline: near an edge hits (most severe band wins),
    /// deep inside or well outside does not — the border is the target.
    #[test]
    fn outlook_border_click_hits_the_outline_only() {
        use radar_core::outlooks::{Outlook, OutlookFeature};
        let outer = vec![vec![vec![
            (-98.0, 34.0),
            (-96.0, 34.0),
            (-96.0, 36.0),
            (-98.0, 36.0),
        ]]];
        let inner = vec![vec![vec![
            (-97.4, 34.6),
            (-96.6, 34.6),
            (-96.6, 35.4),
            (-97.4, 35.4),
        ]]];
        let feature = |label: &str, dn: u16, polygons: Vec<Vec<Vec<(f64, f64)>>>| OutlookFeature {
            dn,
            label: label.to_string(),
            name: format!("{label} Risk"),
            stroke: [255, 255, 255],
            fill: [0, 0, 0],
            polygons,
        };
        let o = Outlook {
            day: 1,
            ty: radar_core::outlooks::OutlookType::Categorical,
            issue: None,
            valid: None,
            expire: None,
            forecaster: "SPC".into(),
            features: vec![feature("SLGT", 4, outer), feature("ENH", 5, inner)],
        };
        let view = MapView {
            center_lat: 35.0,
            center_lon: -97.0,
            zoom: 8.0,
            width_px: 800.0,
            height_px: 600.0,
        };
        // On the ENH edge (the inner box's west side at -97.4).
        let hit = outlook_border_at(&o, &view, -97.4, 35.0, 8.0).expect("border hit");
        assert_eq!(hit.label, "ENH");
        // On the outer edge, away from the inner band.
        let hit = outlook_border_at(&o, &view, -98.0, 35.0, 8.0).expect("outer border");
        assert_eq!(hit.label, "SLGT");
        // Inside the ENH box, far from every edge: no hit.
        assert!(outlook_border_at(&o, &view, -97.0, 35.0, 8.0).is_none());
        // Far outside: no hit.
        assert!(outlook_border_at(&o, &view, -90.0, 40.0, 8.0).is_none());
    }

    /// Segment distance is the primitive the border test is built on.
    #[test]
    fn point_segment_distance_is_clamped_to_the_segment() {
        // Perpendicular distance to a horizontal segment.
        assert!((point_segment_distance(5.0, 3.0, 0.0, 0.0, 10.0, 0.0) - 3.0).abs() < 1e-9);
        // Beyond the end: distance to the endpoint, not the infinite line.
        assert!((point_segment_distance(13.0, 4.0, 0.0, 0.0, 10.0, 0.0) - 5.0).abs() < 1e-9);
        // Degenerate segment.
        assert!((point_segment_distance(3.0, 4.0, 0.0, 0.0, 0.0, 0.0) - 5.0).abs() < 1e-9);
    }

    /// The churn window ages rasters out, so the count is per-second and a
    /// one-off burst does not warn forever.
    #[test]
    fn raster_churn_window_is_one_second() {
        use std::time::{Duration, Instant};
        let base = Instant::now();
        let mut q = std::collections::VecDeque::new();
        assert_eq!(prune_raster_times(&mut q, base, Duration::from_secs(1)), 1);
        assert_eq!(
            prune_raster_times(
                &mut q,
                base + Duration::from_millis(200),
                Duration::from_secs(1)
            ),
            2
        );
        // Two seconds later the early ones have aged out: only the new one.
        assert_eq!(
            prune_raster_times(
                &mut q,
                base + Duration::from_millis(2200),
                Duration::from_secs(1)
            ),
            1
        );
    }

    /// The polygon fill follows the user's slider, and 0% means no wash at
    /// all — overlapping warnings then show only their outlines.
    #[test]
    fn warning_fill_alpha_follows_the_slider() {
        assert!(
            (warning_fill_alpha(0.0, 1.0) - 0.05).abs() < 1e-6,
            "at rest"
        );
        assert!(
            (warning_fill_alpha(1.0, 1.0) - 0.22).abs() < 1e-6,
            "under the pointer"
        );
        assert!((warning_fill_alpha(0.0, 0.5) - 0.025).abs() < 1e-6);
        assert_eq!(warning_fill_alpha(1.0, 0.0), 0.0, "outlines only");
        // Out-of-range values clamp instead of misbehaving.
        assert!((warning_fill_alpha(0.0, 3.0) - 0.05).abs() < 1e-6);
        assert_eq!(warning_fill_alpha(0.0, -1.0), 0.0);
    }

    /// Clicking a stack of overlapping warnings: the first click takes the
    /// most clickable one, the next click on the same spot steps to the one
    /// behind it and wraps, and a click anywhere else — or a stack that has
    /// changed — starts from the top again.
    #[test]
    fn warning_stack_clicks_walk_the_stack() {
        let ids: Vec<String> = ["small", "big"].iter().map(|s| s.to_string()).collect();
        let at = egui::pos2(100.0, 50.0);
        assert_eq!(cycle_pick(None, at, &ids), 0, "first click: most clickable");
        let first = Some((at, ids.clone(), 0));
        assert_eq!(cycle_pick(first.as_ref(), at, &ids), 1, "same spot: next");
        let second = Some((at, ids.clone(), 1));
        assert_eq!(cycle_pick(second.as_ref(), at, &ids), 0, "wraps around");
        // A few pixels of jitter are still "the same spot".
        assert_eq!(cycle_pick(first.as_ref(), egui::pos2(103.0, 52.0), &ids), 1);
        // Elsewhere, or with the stack changed, start over.
        assert_eq!(
            cycle_pick(first.as_ref(), egui::pos2(300.0, 200.0), &ids),
            0
        );
        let replaced: Vec<String> = vec!["new".to_string()];
        assert_eq!(cycle_pick(first.as_ref(), at, &replaced), 0);
        assert_eq!(
            cycle_pick(first.as_ref(), at, &[]),
            0,
            "empty stack is safe"
        );
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
    /// leak outside it — which is why earcut is used here).
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
        assert!(
            mesh.indices.len() >= 3,
            "must produce at least one triangle"
        );
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
    fn wheel_notches_map_to_steps_one_for_one() {
        use egui::{Event, MouseWheelUnit, Vec2};
        let mut acc = 0.0f32;
        // One line notch = exactly one step (the "have to scroll a few times"
        // bug was a smoothed-delta threshold that dropped most notches).
        let mut ev = vec![Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: Vec2::new(0.0, 1.0),
            phase: egui::TouchPhase::Move,
            modifiers: Default::default(),
        }];
        assert_eq!(consume_wheel_steps(&mut ev, &mut acc), 1.0);
        assert!(ev.is_empty(), "wheel events must be consumed");
        // Up is negative.
        let mut ev = vec![Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: Vec2::new(0.0, -2.0),
            phase: egui::TouchPhase::Move,
            modifiers: Default::default(),
        }];
        assert_eq!(consume_wheel_steps(&mut ev, &mut acc), -2.0);
        // Touchpad points accumulate: 30 + 30 points = one step, 10 left over.
        let mk = |pts: f32| {
            vec![Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: Vec2::new(0.0, pts),
                phase: egui::TouchPhase::Move,
                modifiers: Default::default(),
            }]
        };
        let mut ev = mk(30.0);
        assert_eq!(consume_wheel_steps(&mut ev, &mut acc), 0.0);
        let mut ev = mk(30.0);
        assert_eq!(consume_wheel_steps(&mut ev, &mut acc), 1.0);
        assert!(
            (acc - 10.0).abs() < 1e-3,
            "residue kept for next time: {acc}"
        );
        // Non-wheel events pass through untouched.
        let mut ev = vec![Event::Key {
            key: egui::Key::Space,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }];
        assert_eq!(consume_wheel_steps(&mut ev, &mut acc), 0.0);
        assert_eq!(ev.len(), 1);
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
