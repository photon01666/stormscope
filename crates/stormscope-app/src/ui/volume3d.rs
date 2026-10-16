//! The 3D radar pane: all elevation cuts of the current volume as one
//! rotatable scene, shown beside the 2D map (or maximized, via the pane
//! chips drawn by the layout).
//!
//! The split exists because the two views answer different questions: the
//! 2D map is for finding the point (pan/zoom, warnings, reports), the 3D
//! pane is for seeing the vertical structure there (overhang, hail core,
//! mesocyclone depth). Both follow the same selection — site, moment, and
//! the timeline playhead — so they never disagree about what is on screen.
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
use radar_render::volume3d::{Camera, FT_PER_KM, KM_PER_MILE, MeshFilter, ring_radii_mi};

/// Camera pitch limits: below the horizon the ground grid reads as a wall,
/// straight down degenerates the look-at.
const PITCH_MIN: f32 = 8.0;
const PITCH_MAX: f32 = 88.0;
/// Auto-fit distance as a multiple of the longest cut range.
const FIT_FACTOR: f32 = 1.35;
/// Physical height (km) the camera aims at — mid-troposphere, where the
/// interesting echo is. Applied *before* exaggeration, so the aim rises with
/// the height slider (aiming at a fixed altitude made tall storms look
/// flat-footed as the exaggeration went up).
const FIT_TARGET_KM: f32 = 4.5;
/// Marker pin height, physical km (exaggerated like the volume).
const MARKER_PIN_KM: f32 = 3.0;

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
    let Some(volume) = app.state.current().map(|f| f.volume.clone()) else {
        center_text("waiting for the first volume scan…");
        return;
    };
    let Some(site) = radar_core::sites::site(&app.state.site_id) else {
        center_text("no site selected");
        return;
    };

    let moment = app.state.moment;
    let filter = MeshFilter {
        moment,
        detail: app.state.view3d_detail,
        elev_min_deg: app.state.view3d_tilt_lo,
        elev_max_deg: app.state.view3d_tilt_hi,
    };
    let v3 = app.view3d.get_or_insert_with(|| {
        crate::render3d::Volume3d::new(&render_state.device, &render_state.queue)
    });

    // Scene identity: a new scan, a different site/moment/detail, or a tilt
    // range change all mean new geometry. (The playhead's volume is used, so
    // scrubbing the timeline re-renders 3D too. The altitude slab is
    // shader-side and does not appear here on purpose.)
    let sig = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        app.state.site_id.hash(&mut h);
        volume.meta.volume_start.timestamp_millis().hash(&mut h);
        moment.hash(&mut h);
        filter.detail.hash(&mut h);
        filter.elev_min_deg.map(f32::to_bits).hash(&mut h);
        filter.elev_max_deg.map(f32::to_bits).hash(&mut h);
        h.finish()
    };
    v3.update_scene(
        &render_state.device,
        &render_state.queue,
        &volume,
        &filter,
        sig,
    );

    // Threshold lives in the moment's units; keep it inside the table range
    // when the moment changes (a 70 dBZ threshold makes no sense for CC).
    let (lo, hi) = radar_render::ColorTable::for_moment(moment).range();
    if !(lo..=hi).contains(&app.state.view3d_threshold) {
        app.state.view3d_threshold = lo;
    }
    // Fit the camera once there is a scene (and whenever "Fit" was asked):
    // distance from the range, orbit center back at the site.
    if app.state.view3d_dist_km < 1.0 && v3.max_range_km() > 1.0 {
        app.state.view3d_dist_km = (v3.max_range_km() * FIT_FACTOR).max(30.0);
        app.state.view3d_target_x_km = 0.0;
        app.state.view3d_target_y_km = 0.0;
        app.state.view3d_target_z_km = FIT_TARGET_KM * app.state.view3d_ve.max(1.0);
    }
    let ve = app.state.view3d_ve.max(1.0);
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

    // Render + present as an egui image. Feet → km for the altitude slab.
    let alt_lo_km = app.state.view3d_alt_lo_ft / FT_PER_KM;
    let alt_hi_km = app.state.view3d_alt_hi_ft / FT_PER_KM;
    let ppp = ui.ctx().pixels_per_point();
    let size = [
        (rect.width() * ppp).round().clamp(64.0, 4096.0) as u32,
        (rect.height() * ppp).round().clamp(64.0, 4096.0) as u32,
    ];
    v3.render(
        &render_state.device,
        &render_state.queue,
        size,
        &camera,
        ve,
        app.state.view3d_alpha,
        app.state.view3d_threshold,
        alt_lo_km,
        alt_hi_km,
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
            // the screen plane (scale-corrected), so any point of the volume
            // can be brought to the middle.
            let off = camera.pan_offset_km([d.x, d.y], rect.height());
            let limit = max_r.max(20.0) * 3.0;
            app.state.view3d_target_x_km =
                (app.state.view3d_target_x_km + off[0]).clamp(-limit, limit);
            app.state.view3d_target_y_km =
                (app.state.view3d_target_y_km + off[1]).clamp(-limit, limit);
            app.state.view3d_target_z_km = (app.state.view3d_target_z_km + off[2]).clamp(0.0, 60.0);
        } else {
            app.state.view3d_yaw = (app.state.view3d_yaw - d.x * 0.35).rem_euclid(360.0);
            app.state.view3d_pitch =
                (app.state.view3d_pitch - d.y * 0.25).clamp(PITCH_MIN, PITCH_MAX);
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    if resp.hovered() {
        // The slider bar floats over the pane and owns the wheel there.
        let over_bar = match (resp.hover_pos(), app.view3d_bar_rect) {
            (Some(p), Some(bar)) => bar.contains(p),
            _ => false,
        };
        let scroll = ui.ctx().input(|i| i.smooth_scroll_delta.y);
        if !over_bar && scroll != 0.0 {
            let max_r_f = f64::from(max_r.max(20.0));
            let factor = (-f64::from(scroll) * 0.0015).exp();
            let d = (f64::from(app.state.view3d_dist_km).max(1.0) * factor)
                .clamp(max_r_f * 0.08, max_r_f * 8.0);
            app.state.view3d_dist_km = d as f32;
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
    if app.state.view3d_alt_lo_ft > 0.0 || app.state.view3d_alt_hi_ft < 65_000.0 {
        line2.push_str(&format!(
            " · alt {:.0}k–{:.0}k ft",
            app.state.view3d_alt_lo_ft / 1000.0,
            app.state.view3d_alt_hi_ft / 1000.0,
        ));
    }
    let line1 = format!(
        "{} tilts ({:.1}–{:.1}°) · {:.0}k tris",
        v3.tilt_count(),
        v3.elev_lo_deg(),
        v3.elev_hi_deg(),
        v3.triangles() as f32 / 1000.0,
    );
    for (i, text) in [line1, line2].iter().enumerate() {
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
    display_slider_bar(ui, app, rect, scale, (lo, hi), moment);
}

/// The pane's floating slider bar: echo floor, opacity, height scale — the
/// three things one monkey with while watching structure form. Mirrors the
/// map's Min-dBZ bar (same idiom: floating `Area`, wheel owns adjustment).
fn display_slider_bar(
    ui: &mut egui::Ui,
    app: &mut StormApp,
    pane_rect: Rect,
    scale: f32,
    value_range: (f32, f32),
    moment: radar_core::Moment,
) {
    let width = (pane_rect.width() * 0.92).min(760.0 * scale);
    let pos = egui::pos2(
        pane_rect.center().x - width / 2.0,
        pane_rect.bottom() - 66.0 * scale,
    );
    let (lo, hi) = value_range;
    let unit = moment.unit();

    // Slider rects for wheel targeting (filled inside the frame).
    let mut slider_rects: Vec<(u8, Rect)> = Vec::new();
    let area = egui::Area::new(egui::Id::new("stormscope.volume3d.bar"))
        .fixed_pos(pos)
        .movable(false)
        .interactable(true)
        .constrain(false)
        .fade_in(false)
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style())
                .fill(Color32::from_black_alpha(225))
                .show(ui, |ui| {
                    ui.set_min_width(width - 24.0);
                    ui.spacing_mut().slider_width = 118.0 * scale;
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new("Echo ≥")
                                .small()
                                .color(Color32::from_gray(200)),
                        );
                        let r = ui.add(
                            egui::Slider::new(&mut app.state.view3d_threshold, lo..=hi)
                                .step_by(1.0)
                                .suffix(format!(" {unit}")),
                        );
                        slider_rects.push((0, r.rect));
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("Opacity")
                                .small()
                                .color(Color32::from_gray(200)),
                        );
                        let r = ui.add(
                            egui::Slider::new(&mut app.state.view3d_alpha, 0.1..=1.0).step_by(0.05),
                        );
                        slider_rects.push((1, r.rect));
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("Height ×")
                                .small()
                                .color(Color32::from_gray(200)),
                        );
                        let r = ui.add(
                            egui::Slider::new(&mut app.state.view3d_ve, 1.0..=20.0)
                                .step_by(0.5)
                                .suffix("×"),
                        );
                        slider_rects.push((2, r.rect));
                    });
                });
        });
    let rect = area.response.rect;
    app.view3d_bar_rect = Some(rect);

    // Wheel over the bar adjusts the slider under the pointer (and is
    // consumed so the pane underneath cannot zoom). Raw events, one notch
    // one step — same as the map's Min-dBZ bar.
    let hovering = ui
        .ctx()
        .pointer_hover_pos()
        .is_some_and(|p| rect.contains(p));
    if hovering {
        let mut acc = 0.0f32;
        let steps = ui
            .ctx()
            .input_mut(|i| super::map::consume_wheel_steps(&mut i.events, &mut acc));
        if steps != 0.0 {
            ui.ctx()
                .input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
            let target = ui.ctx().pointer_hover_pos().and_then(|p| {
                slider_rects
                    .iter()
                    .find(|(_, r)| r.contains(p))
                    .map(|(k, _)| *k)
            });
            match target {
                Some(0) => {
                    app.state.view3d_threshold =
                        (app.state.view3d_threshold + 2.0 * steps).clamp(lo, hi);
                }
                Some(1) => {
                    app.state.view3d_alpha =
                        (app.state.view3d_alpha + 0.05 * steps).clamp(0.1, 1.0);
                }
                Some(2) => {
                    app.state.view3d_ve = (app.state.view3d_ve + 0.5 * steps).clamp(1.0, 20.0);
                }
                _ => {}
            }
        }
    }
}
