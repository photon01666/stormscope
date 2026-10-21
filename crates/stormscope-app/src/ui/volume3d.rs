//! The 3D radar pane: all elevation cuts of the current volume as one
//! rotatable scene, shown beside the 2D map (or maximized, via the pane
//! chips drawn by the layout).
//!
//! The split exists because the two views answer different questions: the
//! 2D map is for finding the point (pan/zoom, warnings, reports), the 3D
//! pane is for seeing the vertical structure there (overhang, hail core,
//! mesocyclone depth). Both follow the same selection — site, moment, and
//! the timeline playhead — and playback animates both: meshes build on a
//! worker thread, so the 3D view follows the loop at full detail without
//! ever stalling the UI (it lags a frame when builds can't keep up).
//! The ground is the same basemap the 2D map draws, resolved through the
//! tile layer's exact tiles; custom markers rise from it as pins.
//!
//! Units: this pane speaks **miles and feet only** — range rings every 50 mi,
//! altitudes in feet — the way US radar displays do.
//!
//! Interaction: drag to orbit, right/middle-drag to pan the orbit center
//! (free movement), wheel to zoom, double-click to re-fit. The
//! display sliders (echo floor, opacity, height scale) float at the bottom
//! center, mirroring the map's Min-dBZ bar; the sidebar carries the rest
//! (tilt range, altitude slab, detail). Camera and settings live in
//! [`AppState`], so nothing resets when the pane is maximized away and back.

use crate::app::StormApp;
use crate::state::AppState;
use egui::{Align2, Color32, FontId, Rect, Sense, Stroke, Vec2};
use radar_render::section::{SectionParams, build_cross_section, probe_ray};
use radar_render::volume3d::{Camera, FT_PER_KM, KM_PER_MILE, MeshFilter, ring_radii_mi};

/// Camera pitch limits: below the horizon the ground grid reads as a wall,
/// straight down degenerates the look-at.
const PITCH_MIN: f32 = 8.0;
const PITCH_MAX: f32 = 88.0;
/// Auto-fit distance as a multiple of the longest cut range (a floor; the
/// exaggerated echo column can demand more).
const FIT_FACTOR: f32 = 1.35;
/// The echo column the framing budgets for (physical km), and the fraction of
/// its exaggerated height the camera aims at. With the ground scale matched to
/// the map, aiming at the column's top left the pane half empty above the
/// site; a fifth of it keeps the ground in frame with room for the towers.
const FIT_ECHO_TOP_KM: f32 = 13.0;
const FIT_AIM_FRAC: f32 = 0.18;
/// Marker pin height, physical km (exaggerated like the volume).
const MARKER_PIN_KM: f32 = 3.0;
/// Pan sensitivity: how many screen-km of camera movement per dragged km.
/// 1.0 is geometrically exact; a bit over that is what "moves with the
/// pointer" feels like on a scene this wide.
const PAN_GAIN: f32 = 2.2;
/// WASD pan speed, scene-widths per second (÷ the camera distance, so it
/// feels the same zoomed in or out). Shift multiplies it.
const WASD_SPEED: f32 = 1.2;

/// Alert-style yellow for marker pins and their labels.
const MARKER_COLOR: Color32 = Color32::from_rgb(255, 214, 90);

/// Reset the camera on the next frame: yaw north-up, a 38° look-down, and
/// `dist < 1` re-triggers the fit against the scene's actual extent.
pub fn fit_view(state: &mut AppState) {
    state.view3d_yaw = 0.0;
    state.view3d_pitch = 38.0;
    state.view3d_dist_km = 0.0;
}

/// Draw the 3D volume pane into `rect`.
pub fn show_pane(ui: &mut egui::Ui, app: &mut StormApp, rect: Rect) {
    if !app.state.view3d_open || rect.width() < 24.0 || rect.height() < 24.0 {
        return;
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::BLACK);
    let scale = app.sidebar_scale.max(1.0);
    let center_text = |msg: &str| {
        ui.painter_at(rect).text(
            rect.center(),
            Align2::CENTER_CENTER,
            msg,
            FontId::proportional(13.0 * scale),
            Color32::from_gray(150),
        );
    };

    let Some(render_state) = app.render_state.clone() else {
        center_text("3D view needs the wgpu renderer");
        return;
    };
    let moment = app.state.view3d_moment;
    // Derived products are read through the display volume: the column
    // products (VIL/TOPS/MEHS) ride in the frame's volume, SRM comes from
    // the motion-keyed cache and may still be building.
    let Some(volume) = app.state.display_volume_for(moment) else {
        if moment == radar_core::Moment::StormRelativeVelocity {
            center_text(&format!("deriving {}…", moment.label()));
        } else {
            center_text("waiting for the first volume scan…");
        }
        return;
    };
    let Some(site) = radar_core::sites::site(&app.state.site_id) else {
        center_text("no site selected");
        return;
    };
    // Warning sets for this frame. Live warnings are only about live radar:
    // in archive mode today's alerts would be a sync lie over a past scan, so
    // the archive day's polygons (coverage- and time-filtered) take over.
    // The archive gate mirrors `map::archive_overlay_active` minus the map's
    // own alerts switch — the 3D pane's layer has its own switch, so turning
    // the map's off must not empty the 3D scene.
    let archive_overlays = app.state.archive_warnings_date == Some(app.state.archive_date)
        && (app.state.mode == crate::state::Mode::Archive
            || app.state.alerts_popup_open
                && matches!(
                    app.state.alerts_tab,
                    crate::state::AlertsTab::Archive | crate::state::AlertsTab::ArchiveStation
                ));
    let filter = MeshFilter {
        moment,
        detail: app.state.view3d_detail,
        elev_min_deg: app.state.view3d_tilt_lo,
        elev_max_deg: app.state.view3d_tilt_hi,
    };
    let playing = app.state.loop_state == crate::state::LoopState::Playing;
    let v3 = app.view3d.get_or_insert_with(|| {
        crate::render3d::Volume3d::new(&render_state.device, &render_state.queue)
    });

    // Isosurfaces the pane wants (dBZ shells): only reflectivity-like
    // products have a meaningful dBZ scale, so other moments show none.
    let ref_like = matches!(
        moment,
        radar_core::Moment::Reflectivity | radar_core::Moment::CompositeReflectivity
    );
    let iso: Vec<f32> = if ref_like {
        app.state
            .view3d_iso
            .iter()
            .zip(app.state.view3d_shell_preset.dbz())
            .filter(|(on, _)| **on)
            .map(|(_, t)| t)
            .collect()
    } else {
        Vec::new()
    };

    // Scene identity: a new scan, a different site/moment/detail, a tilt
    // range change, another derived volume (SRM follows the motion sliders —
    // the Arc identity carries that), or different echo shells all mean new
    // geometry. (The playhead's volume is used, so scrubbing the timeline
    // re-renders 3D too. The altitude slab is shader-side and does not appear
    // here on purpose.)
    let sig = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        app.state.site_id.hash(&mut h);
        volume.meta.volume_start.timestamp_millis().hash(&mut h);
        std::sync::Arc::as_ptr(&volume).hash(&mut h);
        moment.hash(&mut h);
        filter.detail.hash(&mut h);
        filter.elev_min_deg.map(f32::to_bits).hash(&mut h);
        filter.elev_max_deg.map(f32::to_bits).hash(&mut h);
        for t in &iso {
            t.to_bits().hash(&mut h);
        }
        h.finish()
    };
    // Scenes build on the pane's worker thread: playback (and fast scrubbing)
    // ask for the frame they need, keep drawing the last one, and swap the
    // moment the build lands — at full (Fine) detail, because the UI thread
    // never pays for the build. If ticks outpace builds, the 3D view lags a
    // frame instead of stalling anything.
    v3.pump_scene(&render_state.device, &render_state.queue);
    v3.request_scene(volume.clone(), filter, iso.clone(), sig);
    // Waking only on the playback tick made completions wait up to a full
    // tick to be shown (and drove the old discard behavior). Ask for a
    // repaint shortly while a build is in flight so the finished mesh lands
    // the moment it is ready; the 3D pass itself stays on-demand.
    if v3.has_pending() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(30));
    }

    // Threshold lives in the moment's units; keep it inside the table range
    // when the moment changes (a 70 dBZ threshold makes no sense for CC).
    let (lo, hi) = radar_render::ColorTable::for_moment(moment).range();
    if !(lo..=hi).contains(&app.state.view3d_threshold) {
        app.state.view3d_threshold = lo;
    }
    // Fit the camera once there is a scene (and whenever "Fit" was asked).
    //
    // The distance is chosen to give the scene the **same ground scale as the
    // 2D map** (the same metres per point), so a 50-mile ring is the same size
    // in both panes and the geography lines up when you look from one to the
    // other. The old fit was driven by the radar's range and the enlarged
    // echo column instead, which ignored the map's zoom entirely — with the
    // map zoomed to a storm the 3D pane sat ~1.6× further out and read as
    // "everything closer together".
    //
    // The aim sits a little above the ground rather than at the top of the
    // exaggerated column, so the pane is not half empty sky above the site.
    let ve = app.state.view3d_ve.max(1.0);
    if app.state.view3d_dist_km < 1.0 && v3.max_range_km() > 1.0 {
        let half_fov = (radar_render::volume3d::FOV_Y_DEG.to_radians() / 2.0).tan();
        let range = v3.max_range_km();
        let column = FIT_ECHO_TOP_KM * ve;
        let mpp_km =
            radar_render::meters_per_pixel(app.state.view.center_lat, app.state.view.zoom) / 1000.0;
        let map_matched = app.map_rect_at.map(|r| {
            // Points per km in the 3D pane = pane points / the km the camera
            // sees vertically; set equal to the map's 1 / mpp.
            let d = (r.height() as f64 * mpp_km) / (2.0 * half_fov as f64);
            d as f32
        });
        app.state.view3d_dist_km = map_matched
            .map(|d| d.clamp(range * 0.35, range * 4.0))
            .unwrap_or_else(|| (range * FIT_FACTOR).max(column * 0.5 / half_fov))
            .max(30.0);
        app.state.view3d_target_x_km = 0.0;
        app.state.view3d_target_y_km = 0.0;
        app.state.view3d_target_z_km = column * FIT_AIM_FRAC;
        // The 2D/3D scale question, in numbers: metres per point in the map
        // vs kilometres per point in this pane. They are matched by
        // construction when the map rect is known; a mismatch in these two
        // numbers is a framing bug, not a perspective illusion.
        let pane_km_per_pt = 2.0 * app.state.view3d_dist_km * half_fov / rect.height().max(1.0);
        log::debug!(
            "3D fit: ve ×{ve:.1} · camera {:.0} km · map {:.3} km/pt ({:.0} km across) vs 3D {:.3} km/pt ({:.0} km across)",
            app.state.view3d_dist_km,
            mpp_km,
            mpp_km * app.map_rect_at.map(|r| r.width()).unwrap_or(0.0) as f64,
            pane_km_per_pt,
            pane_km_per_pt * rect.width(),
        );
    }
    let max_r = v3.max_range_km();
    let camera = Camera {
        yaw_deg: app.state.view3d_yaw,
        pitch_deg: app.state.view3d_pitch.clamp(PITCH_MIN, PITCH_MAX),
        distance_km: app.state.view3d_dist_km.max(5.0),
        target_km: [
            app.state.view3d_target_x_km,
            app.state.view3d_target_y_km,
            app.state.view3d_target_z_km,
        ],
    };

    // Projection helpers, shared by the warning layer, the labels and the
    // probe: scene km (ENU) → NDC → pane pixels.
    let aspect = rect.width() / rect.height().max(1.0);
    let ndc_to_screen = |ndc: [f32; 2]| {
        rect.center() + Vec2::new(ndc[0] * rect.width() * 0.5, -ndc[1] * rect.height() * 0.5)
    };
    let project = |world: [f32; 3]| camera.project(aspect, max_r.max(1.0), world);

    // --- Ground: the same basemap tiles the 2D map shows -------------------
    // Two layers: the detailed tiles around the radar, and a coarse surround
    // (two zooms out, over twice the radius) just underneath. The surround
    // runs past the radar's edge and fades into the void at its own edge, so
    // the pane no longer ends in a hard black band where the tiles stopped.
    let fine_radius = max_r.max(50.0);
    let fine_keys =
        radar_render::volume3d::ground_tile_keys(site.latitude, site.longitude, fine_radius);
    let coarse_radius = (fine_radius * 2.2).max(600.0);
    let coarse_zoom = fine_keys.first().map_or(5, |k| k.0.saturating_sub(2));
    let coarse_keys = radar_render::volume3d::ground_tile_keys_at(
        site.latitude,
        site.longitude,
        coarse_radius,
        coarse_zoom,
    );
    // One list: detail first at z = 0, the surround just below it.
    let layered: Vec<((u8, u32, u32), f32)> = fine_keys
        .iter()
        .copied()
        .map(|k| (k, 0.0))
        .chain(coarse_keys.iter().copied().map(|k| (k, -0.08)))
        .collect();
    let key_objs: Vec<radar_render::TileKey> = layered
        .iter()
        .map(|((z, x, y), _)| radar_render::TileKey {
            z: *z,
            x: *x,
            y: *y,
        })
        .collect();
    let ids = app.tiles.ensure_tiles(ui, &key_objs);
    let mut tiles: Vec<(egui::TextureId, [f32; 4], f32)> = Vec::with_capacity(layered.len());
    for ((key, z_km), id) in layered.iter().zip(ids) {
        if let Some(id) = id {
            tiles.push((
                id,
                radar_render::volume3d::ground_tile_bounds_km(*key, site.latitude, site.longitude),
                *z_km,
            ));
        }
    }
    v3.set_ground_tiles(
        &render_state.device,
        &render_state.queue,
        &render_state,
        &tiles,
        coarse_radius,
    );

    // --- Cross-section curtain (RHI along one azimuth) ---------------------
    // One prepared profile serves the whole plane, so rebuilding while the
    // azimuth slider moves costs a couple of milliseconds on the UI thread —
    // no worker round trip needed.
    let section_key = if app.state.view3d_section {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::sync::Arc::as_ptr(&volume).hash(&mut h);
        moment.hash(&mut h);
        app.state.view3d_section_az.to_bits().hash(&mut h);
        max_r.to_bits().hash(&mut h);
        Some(h.finish())
    } else {
        None
    };
    if app.view3d_section_key != section_key {
        app.view3d_section_key = section_key;
        if section_key.is_some() {
            let max_range = max_r.max(50.0);
            let params = SectionParams {
                azimuth_deg: app.state.view3d_section_az,
                max_range_km: max_range,
                top_km: 20.0,
                // ~320 columns across the width, whatever the range: the
                // data's own gate spacing is the real detail limit.
                range_step_km: (max_range / 320.0).clamp(0.4, 2.0),
                height_step_km: 0.3,
            };
            let (verts, idx) = build_cross_section(&volume, moment, &params);
            v3.set_cross_section(&render_state.device, &render_state.queue, &verts, &idx);
        } else {
            v3.set_cross_section(&render_state.device, &render_state.queue, &[], &[]);
        }
    }

    // Render + present as an egui image. Feet → km for the altitude slab.
    let alt_lo_km = app.state.view3d_alt_lo_ft / FT_PER_KM;
    let alt_hi_km = app.state.view3d_alt_hi_ft / FT_PER_KM;
    // The pane hands over its *display* size; the renderer supersamples the
    // settled frame (1.6x, filtered back down by egui's LINEAR sampling) and
    // renders exactly 1x while the camera moves — never below display
    // resolution, because sub-pixel grid lines flicker as their coverage
    // flips frame to frame.
    const MIN_LINE_HALF_PX: f32 = 0.75; // 1.5 px rings/spokes on screen
    let ppp = ui.ctx().pixels_per_point();
    let screen_px = [
        (rect.width() * ppp).round().clamp(64.0, 8192.0) as u32,
        (rect.height() * ppp).round().clamp(64.0, 8192.0) as u32,
    ];
    let ground_key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        tiles.len().hash(&mut h);
        for (id, b, z) in &tiles {
            id.hash(&mut h);
            for v in b {
                v.to_bits().hash(&mut h);
            }
            z.to_bits().hash(&mut h);
        }
        h.finish()
    };
    v3.render_if_needed(
        &render_state.device,
        &render_state.queue,
        screen_px,
        &camera,
        ve,
        app.state.view3d_alpha,
        app.state.view3d_threshold,
        alt_lo_km,
        alt_hi_km,
        ground_key,
        MIN_LINE_HALF_PX,
    );
    let tex = v3.texture_id(&render_state);
    painter.image(
        tex,
        rect,
        Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        Color32::WHITE,
    );

    // --- Input: orbit / zoom / re-fit -------------------------------------
    let resp = ui.interact(
        rect,
        egui::Id::new("stormscope.volume3d.pane"),
        Sense::click_and_drag(),
    );
    if resp.dragged() {
        let d = resp.drag_delta();
        let panning = resp.dragged_by(egui::PointerButton::Secondary)
            || resp.dragged_by(egui::PointerButton::Middle);
        if panning {
            // Free movement: right/middle drag slides the orbit center in
            // the screen plane (scale-corrected; the gain is tuned until the
            // scene visibly follows the pointer), so any point of the volume
            // can be brought to the middle.
            let off = camera.pan_offset_km([d.x, d.y], rect.height());
            let limit = max_r.max(20.0) * 3.0;
            let z_limit = FIT_ECHO_TOP_KM * ve + 40.0;
            app.state.view3d_target_x_km =
                (app.state.view3d_target_x_km + off[0] * PAN_GAIN).clamp(-limit, limit);
            app.state.view3d_target_y_km =
                (app.state.view3d_target_y_km + off[1] * PAN_GAIN).clamp(-limit, limit);
            app.state.view3d_target_z_km =
                (app.state.view3d_target_z_km + off[2] * PAN_GAIN).clamp(0.0, z_limit);
        } else {
            app.state.view3d_yaw = (app.state.view3d_yaw - d.x * 0.35).rem_euclid(360.0);
            app.state.view3d_pitch =
                (app.state.view3d_pitch - d.y * 0.25).clamp(PITCH_MIN, PITCH_MAX);
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        app.view3d_interact_at = std::time::Instant::now();
    }

    // WASD free movement while the pointer rests on the pane (never while a
    // text field owns the keyboard): W/S along the view's ground axis, A/D
    // across it, Q/E down/up. Speed follows the camera distance, so it feels
    // identical zoomed in or out; Shift is the sprint key.
    if resp.hovered() && !ui.ctx().egui_wants_keyboard_input() {
        let (dt, keys) = ui.ctx().input(|i| {
            (
                i.stable_dt.clamp(0.0, 0.1),
                (
                    i.key_down(egui::Key::W),
                    i.key_down(egui::Key::S),
                    i.key_down(egui::Key::A),
                    i.key_down(egui::Key::D),
                    i.key_down(egui::Key::Q),
                    i.key_down(egui::Key::E),
                    i.modifiers.shift,
                ),
            )
        });
        let (w, s, a, d, q, e, shift) = keys;
        if w || s || a || d || q || e {
            let yaw = app.state.view3d_yaw.to_radians();
            let (sy, cy) = yaw.sin_cos();
            // Ground-plane forward (away from the camera) and its right.
            let (fx, fy) = (sy, cy);
            let (rx, ry) = (cy, -sy);
            let speed = camera.distance_km * WASD_SPEED * if shift { 3.0 } else { 1.0 } * dt;
            let (mut dx, mut dy, mut dz) = (0.0f32, 0.0f32, 0.0f32);
            if w {
                dx += fx * speed;
                dy += fy * speed;
            }
            if s {
                dx -= fx * speed;
                dy -= fy * speed;
            }
            if d {
                dx += rx * speed;
                dy += ry * speed;
            }
            if a {
                dx -= rx * speed;
                dy -= ry * speed;
            }
            if e {
                dz += speed;
            }
            if q {
                dz -= speed;
            }
            let limit = max_r.max(20.0) * 3.0;
            let z_limit = FIT_ECHO_TOP_KM * ve + 40.0;
            app.state.view3d_target_x_km = (app.state.view3d_target_x_km + dx).clamp(-limit, limit);
            app.state.view3d_target_y_km = (app.state.view3d_target_y_km + dy).clamp(-limit, limit);
            app.state.view3d_target_z_km = (app.state.view3d_target_z_km + dz).clamp(0.0, z_limit);
            app.view3d_interact_at = std::time::Instant::now();
            ui.ctx().request_repaint();
        }
    }
    if resp.hovered() {
        // The display sliders live in the pane's Controls window now; the
        // wheel always zooms the camera.
        let scroll = ui.ctx().input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            let max_r_f = f64::from(max_r.max(20.0));
            let factor = (-f64::from(scroll) * 0.0015).exp();
            let d = (f64::from(app.state.view3d_dist_km).max(1.0) * factor)
                .clamp(max_r_f * 0.08, max_r_f * 8.0);
            app.state.view3d_dist_km = d as f32;
            app.view3d_interact_at = std::time::Instant::now();
        }
    }
    if resp.double_clicked() {
        fit_view(&mut app.state);
    }

    // --- NWS warning polygons, draped on the scene ground ------------------
    // The 2D map's polygons, projected onto the 3D ground around the storm:
    // outline + translucent wash in the event color, painted over the
    // rendered scene like the markers, so a warning is never buried inside
    // the volume. Hovering one names it; clicking opens its NOAA text.
    // Warning sets for this frame: live warnings only when the radar on
    // screen is live (in archive mode today's alerts are not about the scan
    // being shown), plus the archive day's polygons when its overlay is on,
    // filtered to what the displayed scan could show (coverage + in-effect
    // time) so the polygons follow the timeline like the echoes do.
    let live_warnings: &[radar_core::warnings::Warning] =
        if super::alerts::showing_live_radar(&app.state) {
            &app.state.alerts
        } else {
            &[]
        };
    let archive_warnings = super::alerts::archive_display_set(&app.state, archive_overlays);
    let mut warnings_drawn: Option<usize> = None;
    if app.state.view3d_alerts && max_r > 1.0 {
        let cos_lat = site.latitude.to_radians().cos();
        let to_km = move |lon: f64, lat: f64| {
            [
                ((lon - site.longitude) * 111.32 * cos_lat) as f32,
                ((lat - site.latitude) * 110.574) as f32,
                0.0,
            ]
        };
        // Project every polygon once. A ring with a vertex behind the camera
        // is dropped: a partially projected polygon would smear across the
        // scene, which reads as a glitch, not a warning.
        // The wall height is set in feet (like the rest of the pane) and the
        // scene's Z is exaggerated, so the projection takes display km.
        let top_km = (app.state.view3d_alerts_top_kft.max(0.0) * 1000.0 / FT_PER_KM) * ve;
        let mut polys: Vec<GroundPoly> = Vec::new();
        // Live warnings near the station, plus the selected archive day's
        // historical polygons when that overlay is on (the 2D map draws the
        // same pair) — so breaking-weather warnings and archive browsing look
        // the same in both views.
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for (w, is_historical) in live_warnings
            .iter()
            .map(|w| (w, false))
            .chain(archive_warnings.iter().copied().map(|w| (w, true)))
        {
            if !seen.insert(w.id.as_str()) {
                continue;
            }
            let project = |lon: f64, lat: f64, z_km: f32| {
                let mut p = to_km(lon, lat);
                p[2] = z_km;
                project(p).map(ndc_to_screen)
            };
            if let Some(poly) = polygon_prism(w, project, top_km, &rect, is_historical) {
                polys.push(poly);
            }
        }

        // What is the pointer on? Closest outline first (8 px reach, like the
        // map's click target), then the smallest polygon containing it — the
        // same rule, so what lights up in 3D is what a click opens.
        let hovered_id: Option<String> = if resp.hovered() {
            ui.ctx()
                .pointer_hover_pos()
                .filter(|p| rect.contains(*p))
                .and_then(|p| warning_under_pointer(&polys, p).map(str::to_string))
        } else {
            None
        };
        // Clicking one opens its NOAA detail popup — the same window the 2D
        // map's click opens (and the same afterglow highlight).
        if let Some(id) = hovered_id.as_ref().filter(|_| resp.clicked()) {
            let historical = polys
                .iter()
                .find(|p| p.id == *id)
                .is_some_and(|p| p.historical);
            if historical {
                // The archive overlay's polygons open their archived NWS
                // product text (fetched once, on demand) — the same window
                // the map's click opens.
                app.state.archive_detail = Some(id.clone());
                app.state.alerts_focus = Some(id.clone());
                app.state.alerts_focus_at = ui.ctx().input(|i| i.time);
            } else {
                app.state.alert_detail = Some(id.clone());
                app.state.alerts_focus = Some(id.clone());
                app.state.alerts_focus_at = ui.ctx().input(|i| i.time);
            }
        }

        warnings_drawn = Some(polys.len());
        let now = ui.ctx().input(|i| i.time);
        let mut animating = false;
        for poly in &polys {
            let list_hover = app.state.alerts_hover.as_deref() == Some(poly.id.as_str());
            let on_it = hovered_id.as_deref() == Some(poly.id.as_str());
            let glow: f32 = if list_hover || on_it { 1.0 } else { 0.0 };
            let pulse: f32 = 0.5 + 0.5 * ((now as f32) * 5.0).sin();
            let intensity = glow * (0.65 + 0.35 * pulse);
            if glow > 0.0 {
                animating = true;
            }
            // The 3D pane's own wash strength (0 = outlines only), read
            // through the same helper the map uses so a percentage means the
            // same thing in both views. Walls wash a little lighter than the
            // ground fill: there are a lot more of them per warning.
            let wash = super::map::warning_fill_alpha(intensity, app.state.view3d_alerts_fill);
            // Historical polygons are calm: thinner, dimmer, a lighter wash —
            // the live warnings own the loud styling, as on the 2D map.
            let calm = if poly.historical { 0.55 } else { 1.0 };
            let fill = poly.color.gamma_multiply(wash * calm);
            let wall_fill = poly.color.gamma_multiply(wash * 0.6 * calm);

            // Extruded walls first, so the polygon wraps the storm and the
            // outlines still read on top of them.
            if let Some(top) = &poly.top {
                for i in 0..poly.base.len() {
                    let j = (i + 1) % poly.base.len();
                    painter.add(egui::Shape::convex_polygon(
                        vec![poly.base[i], poly.base[j], top[j], top[i]],
                        wall_fill,
                        Stroke::NONE,
                    ));
                }
                // Vertical corners: without them a translucent wall of one
                // color reads as a haze, not a box around the storm.
                for (b, t) in poly.base.iter().zip(top) {
                    painter.line_segment(
                        [*b, *t],
                        Stroke::new(
                            scale,
                            poly.color.gamma_multiply((0.5 + 0.4 * intensity) * calm),
                        ),
                    );
                }
                if glow > 0.0 {
                    for (width, alpha) in [(4.5, 0.30), (8.5, 0.15)] {
                        painter.add(egui::Shape::closed_line(
                            top.clone(),
                            Stroke::new(
                                width * scale,
                                poly.color.gamma_multiply(alpha * intensity),
                            ),
                        ));
                    }
                }
                // The rim: where the wall ends up in the air.
                painter.add(egui::Shape::closed_line(
                    top.clone(),
                    Stroke::new(
                        (1.2 * scale + 0.9 * scale * intensity) * calm.max(0.6),
                        poly.color.gamma_multiply(0.85 * calm),
                    ),
                ));
            }

            if glow > 0.0 {
                for (width, alpha) in [(4.5, 0.30), (8.5, 0.15)] {
                    painter.add(egui::Shape::closed_line(
                        poly.base.clone(),
                        Stroke::new(width * scale, poly.color.gamma_multiply(alpha * intensity)),
                    ));
                }
            }
            painter.add(egui::Shape::convex_polygon(
                poly.base.clone(),
                fill,
                Stroke::NONE,
            ));
            painter.add(egui::Shape::closed_line(
                poly.base.clone(),
                Stroke::new(
                    (1.8 * scale + 1.2 * scale * intensity) * calm.max(0.6),
                    poly.color.gamma_multiply(calm),
                ),
            ));
            if glow > 0.0 {
                super::map::draw_tag_label(
                    &painter,
                    poly.center,
                    Align2::CENTER_CENTER,
                    &poly.event,
                    FontId::proportional(13.0 * scale),
                    poly.color,
                );
            }
        }
        if animating {
            ui.ctx().request_repaint();
        }
    }

    // --- Labels: cardinals, mile rings, altitude ticks, markers -----------
    // (`aspect` / `ndc_to_screen` / `project` are hoisted above the input
    // block: the warning layer needs the same projection.)
    if max_r > 1.0 {
        let radii = ring_radii_mi(max_r);
        let outer_mi = radii.last().copied().unwrap_or(100);
        let outer_km = outer_mi as f32 * KM_PER_MILE;
        for (text, dx, dy) in [
            ("N", 0.0, 1.0),
            ("E", 1.0, 0.0),
            ("S", 0.0, -1.0),
            ("W", -1.0, 0.0),
        ] {
            let at = (outer_km + 16.0) * dx;
            let bt = (outer_km + 16.0) * dy;
            if let Some(ndc) = project([at, bt, 0.0]) {
                let p = ndc_to_screen(ndc);
                if rect.contains(p) {
                    painter.text(
                        p,
                        Align2::CENTER_CENTER,
                        text,
                        FontId::proportional(11.0 * scale),
                        Color32::from_gray(190),
                    );
                }
            }
        }
        // Ring distances along the NE diagonal (off the cardinal labels).
        for r_mi in &radii {
            let r = *r_mi as f32 * KM_PER_MILE;
            let a = 40.0f32.to_radians();
            if *r_mi == outer_mi {
                continue; // the outermost label just adds clutter
            }
            if let Some(ndc) = project([r * a.sin(), r * a.cos(), 0.0]) {
                let p = ndc_to_screen(ndc);
                if rect.contains(p) {
                    painter.text(
                        p,
                        Align2::CENTER_CENTER,
                        format!("{r_mi} mi"),
                        FontId::proportional(10.0 * scale),
                        Color32::from_gray(150),
                    );
                }
            }
        }
        // Altitude ticks up the west edge: 10/20/30 kft at the exaggerated
        // heights, so the vertical scale is readable while orbiting.
        for ft in [10_000.0f32, 20_000.0, 30_000.0] {
            let z = ft / FT_PER_KM;
            if let Some(ndc) = project([-(outer_km + 14.0), 0.0, z * ve]) {
                let p = ndc_to_screen(ndc);
                if rect.contains(p) {
                    painter.line_segment(
                        [p - Vec2::new(6.0, 0.0), p + Vec2::new(6.0, 0.0)],
                        Stroke::new(1.0, Color32::from_gray(140)),
                    );
                    painter.text(
                        p + Vec2::new(9.0, 0.0),
                        Align2::LEFT_CENTER,
                        format!("{:.0}k ft", ft / 1000.0),
                        FontId::proportional(10.0 * scale),
                        Color32::from_gray(150),
                    );
                }
            }
        }
    }

    // Custom markers: a pin from the ground at the marker's position, so the
    // points picked on the 2D map read in 3D (placefile style).
    let cos_lat = site.latitude.to_radians().cos();
    for m in &app.state.markers {
        let x = ((m.lon - site.longitude) * 111.32 * cos_lat) as f32;
        let y = ((m.lat - site.latitude) * 110.574) as f32;
        if (x * x + y * y).sqrt() > max_r.max(1.0) * 1.1 {
            continue;
        }
        let (Some(p0), Some(p1)) = (project([x, y, 0.0]), project([x, y, MARKER_PIN_KM * ve]))
        else {
            continue;
        };
        let (a, b) = (ndc_to_screen(p0), ndc_to_screen(p1));
        if !rect.contains(a) && !rect.contains(b) {
            continue;
        }
        painter.line_segment([a, b], Stroke::new(1.5, MARKER_COLOR));
        painter.circle_filled(a, 3.0, MARKER_COLOR);
        painter.text(
            b + Vec2::new(4.0, -2.0),
            Align2::LEFT_BOTTOM,
            m.display(),
            FontId::proportional(11.0 * scale),
            MARKER_COLOR,
        );
    }

    // --- AI marks: the ground ring, plus an altitude pole in 3D -----------
    // A suspected tornado is a ground circle; a hail core or updraft top gets
    // a pole to its `alt_kft`, which is exactly the thing 2D cannot show.
    for annotation in &app.state.ai_annotations {
        let x = ((annotation.lon - site.longitude) * 111.32 * cos_lat) as f32;
        let y = ((annotation.lat - site.latitude) * 110.574) as f32;
        if (x * x + y * y).sqrt() > max_r.max(1.0) * 1.2 {
            continue;
        }
        let color = Color32::from_rgb(
            annotation.kind.rgb()[0],
            annotation.kind.rgb()[1],
            annotation.kind.rgb()[2],
        );
        let radius_km = annotation.radius_mi as f32 * KM_PER_MILE;
        let mut ring = Vec::with_capacity(25);
        for step in 0..=24 {
            let angle = step as f32 / 24.0 * std::f32::consts::TAU;
            let world = [
                x + radius_km * angle.sin(),
                y + radius_km * angle.cos(),
                0.0,
            ];
            match project(world) {
                Some(ndc) => ring.push(ndc_to_screen(ndc)),
                None => {
                    ring.clear();
                    break;
                }
            }
        }
        if ring.len() >= 4 {
            painter.add(egui::Shape::closed_line(ring, Stroke::new(1.6, color)));
        }
        let Some(ground_ndc) = project([x, y, 0.0]) else {
            continue;
        };
        let ground = ndc_to_screen(ground_ndc);
        let top_km = annotation.alt_kft.max(0.0) / 3.280_84;
        let top = if top_km > 0.05 {
            project([x, y, top_km * ve]).map(ndc_to_screen)
        } else {
            None
        };
        if let Some(top) = top {
            painter.line_segment([ground, top], Stroke::new(1.8, color));
            painter.circle_stroke(top, 4.5, Stroke::new(1.6, color));
            painter.circle_filled(top, 1.8, color);
            painter.text(
                top + Vec2::new(7.0, -4.0),
                Align2::LEFT_BOTTOM,
                &annotation.label,
                FontId::proportional(12.0 * scale),
                color,
            );
        } else {
            painter.circle_filled(ground, 3.0, color);
            painter.text(
                ground + Vec2::new(7.0, -4.0),
                Align2::LEFT_BOTTOM,
                &annotation.label,
                FontId::proportional(12.0 * scale),
                color,
            );
        }
    }

    // --- Data probe: the echo under the cursor ----------------------------
    // The camera works in *display* space (Z exaggerated), so the ray is
    // un-exaggerated before marching the physical volume; the hit is
    // re-exaggerated for its screen position.
    let probe = if app.state.view3d_probe {
        resp.hovered()
            .then(|| ui.ctx().pointer_hover_pos())
            .flatten()
            .and_then(|pos| {
                let ndc = [
                    ((pos.x - rect.center().x) / (rect.width() * 0.5)).clamp(-1.0, 1.0),
                    (-(pos.y - rect.center().y) / (rect.height() * 0.5)).clamp(-1.0, 1.0),
                ];
                let (eye_d, dir_d) = camera.ray(aspect, ndc);
                let origin = [eye_d[0], eye_d[1], eye_d[2] / ve];
                let dir = [dir_d[0], dir_d[1], dir_d[2] / ve];
                let sampler = radar_render::VolumeSampler::new(&volume, moment);
                if sampler.cut_count() == 0 {
                    return None;
                }
                // The camera can sit well outside the volume, so the march
                // covers the distance to the scene plus the volume's reach.
                let t_max = (camera.distance_km + max_r.max(50.0)) * 1.5;
                let report = probe_ray(&sampler, origin, dir, t_max, app.state.view3d_threshold);
                Some((pos, report))
            })
    } else {
        None
    };
    // Nothing under the ray: no readout at all (the cursor is on empty sky or
    // bare ground — a plate saying so is noise).
    if let Some((pos, report)) = probe.filter(|(_, r)| r.max.is_some()) {
        let unit = moment.unit();
        // Categorical products (HCA) read by name; numeric ones by value.
        let value_text = |v: f32| match moment.value_label(v) {
            Some(label) => format!("{} {label}", moment.label()),
            None => format!("{} {:.0} {}", moment.label(), v, unit),
        };
        let marker = |hit: &radar_render::ProbeHit, color: Color32| -> Option<egui::Pos2> {
            let p = project([hit.pos_km[0], hit.pos_km[1], hit.pos_km[2] * ve])?;
            let g = project([hit.pos_km[0], hit.pos_km[1], 0.0])?;
            let (sp, sg) = (ndc_to_screen(p), ndc_to_screen(g));
            painter.line_segment([sg, sp], Stroke::new(1.0, color.gamma_multiply(0.8)));
            painter.circle_stroke(sp, 3.5, Stroke::new(1.4, color));
            Some(sp)
        };
        let mut lines: Vec<String> = Vec::new();
        let mut anchor = pos;
        if let Some(first) = &report.first {
            // The probe reads in miles and feet, like the rest of the pane.
            lines.push(format!(
                "first {} @ {:.1}k ft · {:.0} mi {}",
                value_text(first.value),
                first.pos_km[2] * FT_PER_KM / 1000.0,
                first.range_km / KM_PER_MILE,
                compass16(first.bearing_deg),
            ));
            if let Some(p) = marker(first, MARKER_COLOR) {
                anchor = p;
            }
        }
        if let Some(max) = &report.max {
            lines.push(format!(
                "max {} @ {:.1}k ft · {:.0} mi {}",
                value_text(max.value),
                max.pos_km[2] * FT_PER_KM / 1000.0,
                max.range_km / KM_PER_MILE,
                compass16(max.bearing_deg),
            ));
            if report.first.is_none() {
                if let Some(p) = marker(max, Color32::from_gray(170)) {
                    anchor = p;
                }
            } else {
                marker(max, Color32::from_gray(150));
            }
        }
        if lines.is_empty() {
            return;
        }
        // Tooltip beside the hit, on a black plate so it stays readable over
        // bright echo cores, and clamped inside the pane.
        let text = lines.join("\n");
        let galley = painter.layout(
            text,
            FontId::proportional(11.0 * scale),
            Color32::from_rgb(235, 240, 245),
            rect.width() - 40.0,
        );
        let pad = Vec2::new(7.0, 5.0) * scale;
        let size = galley.size() + pad * 2.0;
        let at = egui::pos2(
            (anchor.x + 12.0)
                .min(rect.max.x - size.x - 6.0)
                .max(rect.min.x + 6.0),
            (anchor.y - size.y - 10.0)
                .min(rect.max.y - size.y - 6.0)
                .max(rect.min.y + 6.0),
        );
        let plate = Rect::from_min_size(at, size);
        painter.rect_filled(plate, 4.0 * scale, Color32::from_black_alpha(210));
        painter.rect_stroke(
            plate,
            4.0 * scale,
            Stroke::new(1.0, Color32::from_white_alpha(46)),
            egui::StrokeKind::Inside,
        );
        painter.galley(at + pad, galley, Color32::WHITE);
    }

    // Title + stats, top-left (the chips sit top-right; the slider bar owns
    // the bottom center, so nothing here can collide with it).
    painter.text(
        rect.left_top() + Vec2::new(9.0 * scale, 8.0 * scale),
        Align2::LEFT_TOP,
        format!("3D radar — {}", site.id),
        FontId::proportional(12.0 * scale),
        Color32::from_gray(205),
    );
    let mut line2 = format!(
        "{} · height ×{:.1} · {} detail",
        moment.label(),
        ve,
        filter.detail.label(),
    );
    if !iso.is_empty() {
        line2.push_str(&format!(
            " · shells {} dBZ",
            iso.iter()
                .map(|t| format!("{t:.0}"))
                .collect::<Vec<_>>()
                .join("/")
        ));
    }
    if app.state.view3d_section {
        line2.push_str(&format!(" · cut {:.0}°", app.state.view3d_section_az));
    }
    match warnings_drawn {
        Some(0) => line2.push_str(" · no warning polygons in range"),
        Some(1) => line2.push_str(" · 1 warning polygon"),
        Some(n) => line2.push_str(&format!(" · {n} warning polygons")),
        None => {}
    }
    if app.state.view3d_alt_lo_ft > 0.0 || app.state.view3d_alt_hi_ft < 65_000.0 {
        line2.push_str(&format!(
            " · alt {:.0}k–{:.0}k ft",
            app.state.view3d_alt_lo_ft / 1000.0,
            app.state.view3d_alt_hi_ft / 1000.0,
        ));
    }
    let extras = v3.iso_triangles() + v3.section_triangles();
    let line1 = format!(
        "{} tilts ({:.1}–{:.1}°) · {:.0}k tris{}",
        v3.tilt_count(),
        v3.elev_lo_deg(),
        v3.elev_hi_deg(),
        (v3.triangles() + extras) as f32 / 1000.0,
        if extras > 0 {
            format!(" (shells/cut {:.0}k)", extras as f32 / 1000.0)
        } else {
            String::new()
        },
    );
    let line3 = if playing {
        "playback running — the 3D scene follows at full detail (meshes build off-thread)"
            .to_string()
    } else {
        "orbit: drag · pan: right-drag or W/A/S/D · zoom: wheel · fit: double-click · probe: hover"
            .to_string()
    };
    for (i, text) in [line1, line2, line3].iter().enumerate() {
        painter.text(
            rect.left_top()
                + Vec2::new(9.0 * scale, 8.0 * scale + (15.0 + i as f32 * 13.0) * scale),
            Align2::LEFT_TOP,
            text.clone(),
            FontId::proportional(10.5 * scale),
            Color32::from_gray(150),
        );
    }

    // --- The three display sliders, bottom center --------------------------
}

/// Pointer reach (pane pixels) for the 3D warning polygons: the closest
/// outline within this counts as aimed at, matching the map's 8 px.
const WARNING_HIT_PX: f32 = 8.0;

/// One warning polygon projected onto the pane's screen space: the rings
/// (already in pixels), the color/name the 2D map uses, the centroid for the
/// name tag, and the screen area — the tiebreak when the pointer is inside
/// several polygons at once.
struct GroundPoly {
    id: String,
    event: String,
    color: Color32,
    /// The ground ring, projected (pane pixels).
    base: Vec<egui::Pos2>,
    /// The same ring at the wall height, when the polygons are extruded.
    top: Option<Vec<egui::Pos2>>,
    center: egui::Pos2,
    area: f32,
    /// A historical (archive day) warning: drawn calm — the live warnings
    /// own the loud styling, exactly as on the 2D map.
    historical: bool,
}

impl GroundPoly {
    /// Rings used for hit testing: the ground ring plus the wall's top ring
    /// when extruded, so the whole prism is a target — what wraps the storm
    /// is what a click opens.
    fn hit_rings(&self) -> impl Iterator<Item = &[egui::Pos2]> {
        std::iter::once(self.base.as_slice()).chain(self.top.as_deref())
    }
}

/// Project one warning's rings into pane screen space: the ground ring, and
/// the same ring at `top_km` above it when the polygons are extruded (the
/// warning then wraps the storm instead of lying flat on the ground).
///
/// `None` when the warning has no polygon, when a vertex falls behind the
/// camera (a partially projected polygon would smear across the scene, which
/// reads as a glitch, not a warning), or when nothing lands in (or just
/// outside) the pane. `project` takes `(lon, lat, height_km)` — the caller
/// hands in *display* heights, because the scene's Z is exaggerated.
fn polygon_prism(
    w: &radar_core::warnings::Warning,
    project: impl Fn(f64, f64, f32) -> Option<egui::Pos2>,
    top_km: f32,
    visible: &egui::Rect,
    historical: bool,
) -> Option<GroundPoly> {
    let rings = w.polygon.as_ref()?;
    let walls = top_km > 0.01;
    let mut base: Vec<egui::Pos2> = Vec::new();
    let mut top: Option<Vec<egui::Pos2>> = None;
    let mut area = 0.0f32;
    let mut in_view = false;
    for (i, ring) in rings.iter().enumerate() {
        if ring.len() < 3 {
            continue;
        }
        let mut bpts = Vec::with_capacity(ring.len());
        let mut tpts = Vec::with_capacity(ring.len());
        let mut ok = true;
        for &(lon, lat) in ring {
            match project(lon, lat, 0.0) {
                Some(p) => bpts.push(p),
                None => {
                    ok = false;
                    break;
                }
            }
            if walls {
                match project(lon, lat, top_km) {
                    Some(p) => tpts.push(p),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
        }
        if !ok || bpts.len() < 3 {
            continue;
        }
        in_view |=
            bpts.iter().any(|p| visible.contains(*p)) || tpts.iter().any(|p| visible.contains(*p));
        area += screen_ring_area(&bpts);
        // The exterior ring (the first) is what gets drawn and hit-tested: a
        // warning with holes is rare enough to keep the outline only.
        if i == 0 {
            base = bpts;
            top = walls.then_some(tpts);
        }
    }
    if !in_view || base.is_empty() {
        return None;
    }
    let n = base.len() as f32;
    let base_center = base.iter().fold(Vec2::ZERO, |acc, p| acc + p.to_vec2()) / n;
    // The name sits at the wall's midpoint, so it reads with the prism
    // instead of being buried under the volume at the ground center.
    let center = match &top {
        Some(t) if !t.is_empty() => {
            let top_center = t.iter().fold(Vec2::ZERO, |acc, p| acc + p.to_vec2()) / n;
            ((base_center + top_center) * 0.5).to_pos2()
        }
        _ => base_center.to_pos2(),
    };
    Some(GroundPoly {
        id: w.id.clone(),
        event: w.event.clone(),
        color: super::alerts::warning_color(w),
        base,
        top,
        center,
        area: area.max(1.0),
        historical,
    })
}

/// Which warning a pointer at `p` is on: the closest outline within
/// [`WARNING_HIT_PX`] first, then the smallest polygon containing it — the
/// same "what lights up is what a click opens" rule the 2D map uses, so a
/// stack of overlapping warnings is reachable in both views.
fn warning_under_pointer(polys: &[GroundPoly], p: egui::Pos2) -> Option<&str> {
    let mut best: Option<(u8, f32, &str)> = None;
    for poly in polys {
        let mut edge = f32::INFINITY;
        for pts in poly.hit_rings() {
            for i in 0..pts.len() {
                let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                edge = edge.min(super::map::point_segment_distance(
                    p.x as f64, p.y as f64, a.x as f64, a.y as f64, b.x as f64, b.y as f64,
                ) as f32);
            }
        }
        let (rank, key) = if edge <= WARNING_HIT_PX {
            (0u8, edge)
        } else if poly.hit_rings().any(|pts| polygon_contains_screen(pts, p)) {
            (1, poly.area)
        } else {
            continue;
        };
        if best.is_none_or(|(br, bk, _)| (rank, key.to_bits()) < (br, bk.to_bits())) {
            best = Some((rank, key, poly.id.as_str()));
        }
    }
    best.map(|(_, _, id)| id)
}

/// Screen-space ray cast: is `p` inside the projected ring? Perspective turns
/// a ground polygon into an arbitrary screen polygon, so the hit test runs on
/// the projection, not on lon/lat.
fn polygon_contains_screen(pts: &[egui::Pos2], p: egui::Pos2) -> bool {
    if pts.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = pts.len() - 1;
    for i in 0..pts.len() {
        let (a, b) = (pts[i], pts[j]);
        if (a.y > p.y) != (b.y > p.y) {
            let t = (p.y - a.y) / (b.y - a.y);
            if p.x < a.x + t * (b.x - a.x) {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Shoelace area of a projected ring (square pixels) — the tiebreak when the
/// pointer is inside several polygons: the smallest wins, like the map.
fn screen_ring_area(pts: &[egui::Pos2]) -> f32 {
    if pts.len() < 3 {
        return 0.0;
    }
    let mut sum = 0.0f32;
    let mut j = pts.len() - 1;
    for i in 0..pts.len() {
        sum += (pts[j].x + pts[i].x) * (pts[j].y - pts[i].y);
        j = i;
    }
    (sum * 0.5).abs()
}

/// 16-point compass name for a bearing in degrees (the probe's "31 mi NNE").
fn compass16(deg: f32) -> &'static str {
    const NAMES: [&str; 16] = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW",
        "NW", "NNW",
    ];
    let idx = (deg.rem_euclid(360.0) / 22.5).round() as usize % 16;
    NAMES[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(cx: f32, cy: f32, r: f32) -> Vec<egui::Pos2> {
        vec![
            egui::pos2(cx - r, cy - r),
            egui::pos2(cx + r, cy - r),
            egui::pos2(cx + r, cy + r),
            egui::pos2(cx - r, cy + r),
        ]
    }

    /// The 3D warning hit test runs on the projected ring: containment and
    /// area both behave for a plain screen polygon.
    #[test]
    fn screen_polygon_hit_test() {
        let s = square(100.0, 100.0, 20.0);
        assert!(polygon_contains_screen(&s, egui::pos2(100.0, 100.0)));
        assert!(polygon_contains_screen(&s, egui::pos2(115.0, 80.0)));
        assert!(!polygon_contains_screen(&s, egui::pos2(100.0, 130.0)));
        assert!(!polygon_contains_screen(&s, egui::pos2(79.0, 79.0)));
        assert!(!polygon_contains_screen(&s, egui::pos2(100.0, 121.0)));
        // Corner-adjacent: inside the bounding box but outside the edges.
        assert!(polygon_contains_screen(&s, egui::pos2(81.0, 81.0)));
        // A concave ("L") polygon must not count the missing notch as inside.
        let l = vec![
            egui::pos2(0.0, 0.0),
            egui::pos2(100.0, 0.0),
            egui::pos2(100.0, 40.0),
            egui::pos2(40.0, 40.0),
            egui::pos2(40.0, 100.0),
            egui::pos2(0.0, 100.0),
        ];
        assert!(polygon_contains_screen(&l, egui::pos2(20.0, 80.0)));
        assert!(!polygon_contains_screen(&l, egui::pos2(80.0, 80.0)));
        // Too few points: never inside, zero area.
        assert!(!polygon_contains_screen(&s[..2], egui::pos2(100.0, 100.0)));
        assert_eq!(screen_ring_area(&s[..2]), 0.0);
        // Area: the 40x40 square is 1600 px², and a bigger one outranks it
        // (the smallest-containing-polygon rule).
        assert!((screen_ring_area(&s) - 1600.0).abs() < 0.01);
        assert!(screen_ring_area(&square(0.0, 0.0, 50.0)) > screen_ring_area(&s));
    }

    fn poly(id: &str, base: Vec<egui::Pos2>) -> GroundPoly {
        let n = base.len() as f32;
        let sum = base.iter().fold(Vec2::ZERO, |a, p| a + p.to_vec2());
        GroundPoly {
            id: id.to_string(),
            event: id.to_string(),
            color: Color32::WHITE,
            area: screen_ring_area(&base),
            center: (sum / n).to_pos2(),
            base,
            top: None,
            historical: false,
        }
    }

    fn warning(id: &str, ring: &[(f64, f64)]) -> radar_core::warnings::Warning {
        radar_core::warnings::Warning {
            id: id.into(),
            event: "Severe Thunderstorm Warning".into(),
            severity: "Severe".into(),
            urgency: "Expected".into(),
            expires: chrono::Utc::now(),
            office: "OUN".into(),
            headline: String::new(),
            description: String::new(),
            instruction: String::new(),
            area: String::new(),
            polygon: Some(vec![ring.to_vec()]),
        }
    }

    /// Projection: a ground ring lands in the pane as a polygon with a real
    /// center and area; a warning with a vertex behind the camera or wholly
    /// off-pane is dropped rather than smeared.
    #[test]
    fn polygon_prism_projects_extrudes_and_rejects() {
        let pane = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        // A 0.4° × 0.4° square around (100, 40), 1000 px per degree; height
        // is 1 px per display km, up (screen y = −lat − z).
        let ring = [(-100.2, 39.8), (-99.8, 39.8), (-99.8, 40.2), (-100.2, 40.2)];
        let project = |lon: f64, lat: f64, z: f32| {
            Some(egui::pos2(
                ((lon + 100.0) * 1000.0) as f32,
                ((40.0 - lat) * 1000.0 + 300.0) as f32 - z,
            ))
        };
        let flat =
            polygon_prism(&warning("a", &ring), project, 0.0, &pane, false).expect("projected");
        assert_eq!(flat.base.len(), 4);
        assert!(flat.top.is_none(), "flat when no wall height is set");
        // 0.4° × 1000 px/° on each side → 400×400 px.
        assert!((screen_ring_area(&flat.base) - 160_000.0).abs() < 1.0);
        assert!(pane.contains(flat.center), "center {:?}", flat.center);
        assert_eq!(flat.event, "Severe Thunderstorm Warning");
        assert_eq!(
            flat.color,
            super::super::alerts::warning_color(&warning("a", &ring))
        );

        // Extruded: the top ring exists, sits above the base with the same
        // footprint, and the name rides the wall's midpoint.
        let tall =
            polygon_prism(&warning("a", &ring), project, 120.0, &pane, true).expect("projected");
        let top = tall.top.as_ref().expect("top ring");
        assert_eq!(top.len(), 4);
        assert!(top.iter().all(|p| p.y < tall.base[0].y), "top above base");
        assert!(tall.center.y < flat.center.y, "name rides up the wall");
        assert!(
            (screen_ring_area(top) - 160_000.0).abs() < 1.0,
            "same footprint"
        );
        let mean_y = |pts: &[egui::Pos2]| pts.iter().map(|p| p.y).sum::<f32>() / pts.len() as f32;
        assert!(
            (tall.center.y - (mean_y(&tall.base) + mean_y(top)) * 0.5).abs() < 0.01,
            "the name sits midway up the wall"
        );

        // One vertex behind the camera: the whole polygon is dropped.
        let behind = |lon: f64, lat: f64, z: f32| {
            if lon < -100.0 {
                None
            } else {
                project(lon, lat, z)
            }
        };
        assert!(polygon_prism(&warning("b", &ring), behind, 120.0, &pane, false).is_none());

        // Far off-pane: nothing to paint.
        let away = |lon: f64, lat: f64, z: f32| {
            project(lon, lat, z).map(|p| p + egui::vec2(4000.0, 4000.0))
        };
        assert!(polygon_prism(&warning("c", &ring), away, 120.0, &pane, false).is_none());

        // No polygon at all (zone-based alert): nothing to draw.
        let mut zone = warning("d", &ring);
        zone.polygon = None;
        assert!(polygon_prism(&zone, project, 120.0, &pane, false).is_none());
    }

    /// The 3D warning pointer rule: outline reach beats containment, and
    /// among containing polygons the smallest wins — the map's rule, so a
    /// stack is reachable identically in both views.
    #[test]
    fn warning_under_pointer_prefers_outline_then_smallest() {
        // Two nested squares: the pointer deep inside the big one and just
        // inside the small one hits the small one (smallest wins).
        let big = poly("big", square(100.0, 100.0, 80.0));
        let small = poly("small", square(100.0, 100.0, 15.0));
        let polys = vec![big, small];
        assert_eq!(
            warning_under_pointer(&polys, egui::pos2(105.0, 105.0)),
            Some("small")
        );
        // Between the two outlines: inside the big one only.
        assert_eq!(
            warning_under_pointer(&polys, egui::pos2(160.0, 100.0)),
            Some("big")
        );
        // On the big polygon's outline but outside every fill — the outline
        // reach (8 px) still catches it, ahead of containment.
        assert_eq!(
            warning_under_pointer(&polys, egui::pos2(181.0, 100.0)),
            Some("big")
        );
        // Well outside everything: nothing.
        assert_eq!(
            warning_under_pointer(&polys, egui::pos2(400.0, 400.0)),
            None
        );
        // Two overlapping outlines: the closer edge wins.
        let a = poly("a", square(100.0, 100.0, 40.0));
        let b = poly("b", square(104.0, 100.0, 40.0));
        assert_eq!(
            warning_under_pointer(&[a, b], egui::pos2(62.0, 100.0)),
            Some("a"),
            "the near edge wins"
        );
    }

    /// Compass names for the probe's bearings wrap the full circle.
    #[test]
    fn compass_names_cover_the_circle() {
        assert_eq!(compass16(0.0), "N");
        assert_eq!(compass16(359.0), "N");
        assert_eq!(compass16(90.0), "E");
        assert_eq!(compass16(180.0), "S");
        assert_eq!(compass16(270.0), "W");
        assert_eq!(compass16(22.5), "NNE");
        assert_eq!(compass16(-90.0), "W");
    }
}
