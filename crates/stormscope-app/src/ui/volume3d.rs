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
/// Top of the echo column (km on paper) the framing budgets for, and the
/// fraction of it the camera aims at. Applied *after* exaggeration: at ×20 a
/// 13 km storm is 260 km of visual, so aiming at a fixed altitude made tall
/// storms float half out of frame.
const FIT_ECHO_TOP_KM: f32 = 13.0;
const FIT_AIM_FRAC: f32 = 0.42;
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
    if rect.width() < 24.0 || rect.height() < 24.0 {
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
    let moment = app.state.moment;
    // Derived products are read through the display volume: the column
    // products (VIL/TOPS/MEHS) ride in the frame's volume, SRM comes from
    // the motion-keyed cache and may still be building.
    let Some(volume) = app.state.display_volume() else {
        if app.state.moment == radar_core::Moment::StormRelativeVelocity {
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
            .zip([30.0f32, 50.0, 65.0])
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
    // Fit the camera once there is a scene (and whenever "Fit" was asked):
    // distance from the range (with a floor for the exaggerated column),
    // orbit center back at the site, aimed at the echo the height scale
    // produces.
    let ve = app.state.view3d_ve.max(1.0);
    if app.state.view3d_dist_km < 1.0 && v3.max_range_km() > 1.0 {
        let half_h = FIT_ECHO_TOP_KM * ve / 2.0;
        let column_dist =
            half_h / (radar_render::volume3d::FOV_Y_DEG.to_radians() / 2.0).tan() * 1.4;
        app.state.view3d_dist_km = (v3.max_range_km() * FIT_FACTOR).max(column_dist).max(30.0);
        app.state.view3d_target_x_km = 0.0;
        app.state.view3d_target_y_km = 0.0;
        app.state.view3d_target_z_km = FIT_ECHO_TOP_KM * ve * FIT_AIM_FRAC;
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

    // --- Ground: the same basemap tiles the 2D map shows -------------------
    let keys =
        radar_render::volume3d::ground_tile_keys(site.latitude, site.longitude, max_r.max(50.0));
    let key_objs: Vec<radar_render::TileKey> = keys
        .iter()
        .map(|(z, x, y)| radar_render::TileKey {
            z: *z,
            x: *x,
            y: *y,
        })
        .collect();
    let ids = app.tiles.ensure_tiles(ui, &key_objs);
    let mut tiles: Vec<(egui::TextureId, [f32; 4])> = Vec::with_capacity(keys.len());
    for (key, id) in keys.iter().zip(ids) {
        if let Some(id) = id {
            tiles.push((
                id,
                radar_render::volume3d::ground_tile_bounds_km(*key, site.latitude, site.longitude),
            ));
        }
    }
    v3.set_ground_tiles(
        &render_state.device,
        &render_state.queue,
        &render_state,
        &tiles,
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
        for (id, b) in &tiles {
            id.hash(&mut h);
            for v in b {
                v.to_bits().hash(&mut h);
            }
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

    // --- Labels: cardinals, mile rings, altitude ticks, markers -----------
    let aspect = rect.width() / rect.height().max(1.0);
    let ndc_to_screen = |ndc: [f32; 2]| {
        rect.center() + Vec2::new(ndc[0] * rect.width() * 0.5, -ndc[1] * rect.height() * 0.5)
    };
    let project = |world: [f32; 3]| camera.project(aspect, max_r.max(1.0), world);
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
    if let Some((pos, report)) = probe {
        let unit = moment.unit();
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
                "first {:.0} {} @ {:.1}k ft · {:.0} mi {}",
                first.value,
                unit,
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
                "max {:.0} {} @ {:.1}k ft · {:.0} mi {}",
                max.value,
                unit,
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
            lines.push("no echo under the cursor".to_string());
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

/// 16-point compass name for a bearing in degrees (the probe's "31 mi NNE").
fn compass16(deg: f32) -> &'static str {
    const NAMES: [&str; 16] = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW",
        "NW", "NNW",
    ];
    let idx = (deg.rem_euclid(360.0) / 22.5).round() as usize % 16;
    NAMES[idx]
}
