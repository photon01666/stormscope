//! Multi-pane 2D radar layout. Each pane is a complete independent radar app:
//! its own site, data thread, ring, alerts, map layers and controls. The main
//! pane keeps the timeline and app-wide windows; secondary panes expose their
//! own site/product/tilt and map Controls. Desktop allows up to four panes,
//! Android two.

use crate::app::StormApp;
use crate::state::max_radar_panes;
use egui::{Color32, Rect};

/// Draw the active multi-pane layout. Returns true when it handled the central
/// area, so the caller skips the normal single-map/3D layout.
pub fn show(ui: &mut egui::Ui, app: &mut StormApp) -> bool {
    let count = app.state.radar_panes.clamp(1, max_radar_panes());
    if count < 2 {
        return false;
    }
    app.state.ensure_radar_panes();

    let full = ui.available_rect_before_wrap();
    let scale = app.sidebar_scale.max(1.0);
    let gap = (3.0 * scale).max(2.0);
    let rects = pane_rects(full, count, gap);
    ui.painter().rect_filled(full, 0.0, Color32::from_gray(30));

    // Pane 1 keeps the complete app map (warnings, tracks, markers, controls).
    ui.scope_builder(egui::UiBuilder::new().max_rect(rects[0]), |ui| {
        ui.set_clip_rect(rects[0]);
        crate::ui::map::map(ui, app);
    });
    app.map_rect_at = Some(rects[0]);

    for slot in 0..count.saturating_sub(1) {
        if let Some(rect) = rects.get(slot + 1) {
            show_secondary(ui, app, slot, *rect, full, scale);
        }
    }

    // The scan/mode/age/alerts/Forecast/FutureCast rail belongs to the app,
    // not to any one pane. Draw it once across the whole central area.
    let rail_painter = ui.painter_at(full);
    crate::ui::map::draw_top_badges(ui, &rail_painter, full, app);

    // Per-pane Controls button, then the app-level buttons (Settings, GPS,
    // AI) once in fixed screen corners.
    draw_pane_controls(ui, app, rects[0], scale);
    draw_global_chrome(ui, app, full, scale);

    // Analysis windows opened from a secondary pane's Controls must paint
    // above the app rail and global buttons.
    for slot in 0..count.saturating_sub(1) {
        if let Some(mut pane_app) = app.pane_apps.get_mut(slot).and_then(Option::take) {
            crate::ui::secondary_analysis_windows(ui.ctx(), pane_app.as_mut(), full);
            if let Some(slot_app) = app.pane_apps.get_mut(slot) {
                *slot_app = Some(pane_app);
            }
        }
    }

    let painter = ui.painter();
    for rect in &rects {
        painter.rect_stroke(
            rect.shrink(0.5),
            0.0,
            egui::Stroke::new(1.0, Color32::from_white_alpha(38)),
            egui::StrokeKind::Inside,
        );
    }
    true
}

/// Split the central rect into panes. Android portrait stacks two panes;
/// desktop uses columns for two/three and a 2x2 grid for four.
fn pane_rects(full: Rect, count: usize, gap: f32) -> Vec<Rect> {
    let mut out = Vec::with_capacity(count);
    match count {
        0 | 1 => out.push(full),
        2 if cfg!(target_os = "android") && full.height() > full.width() => {
            let h = ((full.height() - gap) / 2.0).max(1.0);
            out.push(Rect::from_min_size(full.min, egui::vec2(full.width(), h)));
            out.push(Rect::from_min_size(
                egui::pos2(full.min.x, full.min.y + h + gap),
                egui::vec2(full.width(), h),
            ));
        }
        2 => {
            let w = ((full.width() - gap) / 2.0).max(1.0);
            out.push(Rect::from_min_size(full.min, egui::vec2(w, full.height())));
            out.push(Rect::from_min_size(
                egui::pos2(full.min.x + w + gap, full.min.y),
                egui::vec2(w, full.height()),
            ));
        }
        3 => {
            let w = ((full.width() - 2.0 * gap) / 3.0).max(1.0);
            for i in 0..3 {
                out.push(Rect::from_min_size(
                    egui::pos2(full.min.x + i as f32 * (w + gap), full.min.y),
                    egui::vec2(w, full.height()),
                ));
            }
        }
        _ => {
            let w = ((full.width() - gap) / 2.0).max(1.0);
            let h = ((full.height() - gap) / 2.0).max(1.0);
            for i in 0..4 {
                let col = i % 2;
                let row = i / 2;
                out.push(Rect::from_min_size(
                    egui::pos2(
                        full.min.x + col as f32 * (w + gap),
                        full.min.y + row as f32 * (h + gap),
                    ),
                    egui::vec2(w, h),
                ));
            }
        }
    }
    out
}

fn draw_pane_controls(ui: &mut egui::Ui, app: &mut StormApp, rect: Rect, scale: f32) {
    let controls = super::controls_chip(
        ui,
        super::bottom_right(rect, scale),
        scale,
        "Map display controls (Min dBZ, opacity)",
    );
    if controls.clicked() {
        let opening = !app.state.map_controls_open;
        app.state.map_controls_open = opening;
        #[cfg(target_os = "android")]
        if opening {
            for pane in app.pane_apps.iter_mut().flatten() {
                pane.state.map_controls_open = false;
            }
        }
    }
}

fn draw_global_chrome(ui: &mut egui::Ui, app: &mut StormApp, full: Rect, scale: f32) {
    #[cfg(target_os = "android")]
    {
        let side = super::map_chip_side(scale);
        let _settings = super::settings_chip(
            ui,
            egui::pos2(
                full.min.x + 8.0 * scale + side,
                super::bottom_right(full, scale).y,
            ),
            app,
            scale,
        );
        // GPS sits above the bottom-right Controls slot in both single and
        // multipane layouts.
        super::gps_chip(
            ui,
            egui::pos2(
                full.max.x - 8.0 * scale,
                full.max.y - 8.0 * scale - side - 10.0 * scale,
            ),
            app,
            scale,
        );
    }
    super::ai_chip(ui, full, app, scale);
}

fn show_secondary(
    ui: &mut egui::Ui,
    app: &mut StormApp,
    slot: usize,
    rect: Rect,
    full: Rect,
    scale: f32,
) {
    draw_secondary_pane(ui, app, slot, rect, full, scale, true);
}

/// Fullscreen multi-pane mode: both maps remain, with no app rail, timeline,
/// toolbars or control chips.
pub fn show_fullscreen(ui: &mut egui::Ui, app: &mut StormApp) {
    let count = app.state.radar_panes.clamp(1, max_radar_panes());
    let full = ui.available_rect_before_wrap();
    let gap = (2.0 * app.sidebar_scale.max(1.0)).max(1.0);
    let rects = pane_rects(full, count, gap);
    ui.painter().rect_filled(full, 0.0, Color32::BLACK);
    ui.scope_builder(egui::UiBuilder::new().max_rect(rects[0]), |ui| {
        ui.set_clip_rect(rects[0]);
        crate::ui::map::map(ui, app);
    });
    app.map_rect_at = Some(rects[0]);
    for slot in 0..count.saturating_sub(1) {
        if let Some(rect) = rects.get(slot + 1) {
            draw_secondary_pane(
                ui,
                app,
                slot,
                *rect,
                full,
                app.sidebar_scale.max(1.0),
                false,
            );
        }
    }
    let painter = ui.painter();
    for rect in &rects {
        painter.rect_stroke(
            rect.shrink(0.5),
            0.0,
            egui::Stroke::new(1.0, Color32::from_white_alpha(30)),
            egui::StrokeKind::Inside,
        );
    }
    // Fullscreen has no badge rail, but a long-resume catch-up still gets
    // its strip: the frame under it is stale and nothing else says so.
    crate::ui::map::draw_resume_catchup_in_fullscreen(ui, full, app);
}

fn draw_secondary_pane(
    ui: &mut egui::Ui,
    app: &mut StormApp,
    slot: usize,
    rect: Rect,
    _full: Rect,
    scale: f32,
    chrome: bool,
) {
    let mut desired = app
        .state
        .secondary_radar_panes
        .get(slot)
        .map(|pane| pane.site_id.clone())
        .unwrap_or_default();
    // Site clicks happen inside the pane app itself. Adopt that new site
    // before the mismatch check below, otherwise the outer metadata would
    // rebuild the pane with the old site and snap it back.
    if let Some(existing) = app.pane_apps.get(slot).and_then(Option::as_ref)
        && !existing.state.site_id.is_empty()
        && existing.state.site_id != desired
    {
        desired = existing.state.site_id.clone();
        if let Some(pane) = app.state.secondary_radar_panes.get_mut(slot) {
            pane.site_id = desired.clone();
        }
    }
    if desired.is_empty() {
        return;
    }
    if app.pane_apps.len() <= slot {
        app.pane_apps.resize_with(slot + 1, || None);
    }
    let default_product = app
        .state
        .secondary_radar_panes
        .get(slot)
        .and_then(|pane| pane.product);
    let stale = app.pane_apps[slot]
        .as_ref()
        .is_none_or(|pane_app| pane_app.state.site_id != desired);
    if stale {
        app.pane_apps[slot] = Some(Box::new(StormApp::new_secondary(
            ui.ctx(),
            app.render_state.clone(),
            &desired,
        )));
        if let (Some(product), Some(pane_app)) = (default_product, app.pane_apps[slot].as_mut()) {
            pane_app.state.moment = product;
        }
        // A new pane joins a timeline that may already hold two hours of
        // scans. Load the same window for its site, so the bottom scrub bar
        // can move it with the others from the start instead of being stuck
        // on the one newest scan.
        if app.state.ring.len() >= 8
            && let Some(pane_app) = app.pane_apps[slot].as_mut()
        {
            let _ = pane_app.tx.send(crate::app::DataCmd::LoadLastTwoHours(
                pane_app.state.site_id.clone(),
                app.state.newest_start(),
            ));
        }
    }

    // Take the app out while it is driven: the map needs `&mut StormApp`
    // while the main app still owns the slot.
    let Some(mut pane_app) = app.pane_apps[slot].take() else {
        return;
    };
    let pane = pane_app.as_mut();
    pane.pump();
    // The bottom scrub bar and Play button belong to the app, so they drive
    // every pane. Follow the primary's selected scan by *time* — frame indices
    // differ between sites and cadences — so scrubbing to 4:26 AM shows 4:26
    // AM everywhere instead of leaving the secondary panes on their old frame.
    let followed_scan = app.state.current().map(|frame| frame.volume_start);
    if let Some(at) = followed_scan
        && let Some(index) = pane.state.frame_nearest(at)
    {
        pane.state.playhead = index;
    }
    // The scrub flag drives the raster cadence (30 fps while dragging the
    // timeline). Without it the secondary maps repaint at the idle 10 fps and
    // visibly lag behind pane 1 during a scrub. Play state rides along so a
    // running loop repaints at the same rhythm.
    pane.state.timeline_scrubbing = app.state.timeline_scrubbing;
    pane.state.loop_state = app.state.loop_state;
    // Pane 1 may already hold a two-hour window (the user asked for it, or
    // the live feed has been running). A pane with only the newest scan
    // cannot follow the scrub bar into that history, so seed it with the
    // same window. Only when the primary's ring is genuinely long, and at
    // most every five minutes per pane.
    let primary_span = match (app.state.ring.front(), app.state.ring.back()) {
        (Some(first), Some(last)) => last.volume_start - first.volume_start,
        _ => chrono::Duration::zero(),
    };
    let pane_oldest = pane.state.ring.front().map(|frame| frame.volume_start);
    let needs_history = primary_span > chrono::Duration::minutes(90)
        && match (app.state.ring.front(), pane_oldest) {
            (Some(primary_oldest), Some(pane_oldest)) => {
                primary_oldest.volume_start < pane_oldest - chrono::Duration::minutes(20)
            }
            (Some(_), None) => true,
            _ => false,
        };
    let seed_due = pane
        .state
        .window_requested_at
        .is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(300));
    if needs_history && seed_due {
        pane.state.window_requested_at = Some(std::time::Instant::now());
        let anchor = app.state.newest_start();
        let _ = pane.tx.send(crate::app::DataCmd::LoadLastTwoHours(
            pane.state.site_id.clone(),
            anchor,
        ));
    }
    // User pins and the device GPS fix are app-level landmarks: show the same
    // ones in every radar pane, not just pane 1.
    pane.state.markers = app.state.markers.clone();
    pane.state.gps_fix = app.state.gps_fix;
    pane.sidebar_scale = app.sidebar_scale;
    // Station totals live in the app-wide Settings drawer, so the switch must
    // apply to every pane. Without this, turning it off hid pane 1's layer
    // only and the other panes kept drawing their diamonds.
    pane.state.show_totals = app.state.show_totals;
    // The bottom dBZ button is app-wide: turn the inspection crosshair on for
    // every visible radar pane, each with its own map position.
    pane.state.dbz_crosshair = app.state.dbz_crosshair;
    pane.state.view.width_px = rect.width().max(1.0);
    pane.state.view.height_px = rect.height().max(1.0);
    if pane.state.dbz_crosshair && pane.state.dbz_crosshair_pos.is_none() {
        pane.state.dbz_crosshair_pos =
            Some((pane.state.view.center_lat, pane.state.view.center_lon));
    }

    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.set_clip_rect(rect);
        crate::ui::map::map(ui, pane);
    });
    pane.map_rect_at = Some(rect);

    // Long-press on any radar pane toggles the main app's fullscreen. The
    // secondary map intentionally does not toggle its own chrome.
    secondary_long_press(ui.ctx(), app, rect);

    if chrome {
        let controls = super::controls_chip(
            ui,
            super::bottom_right(rect, scale),
            scale,
            "Pane display controls",
        );
        if controls.clicked() {
            let opening = !pane.state.map_controls_open;
            pane.state.map_controls_open = opening;
            #[cfg(target_os = "android")]
            if opening {
                // One full-screen Controls card at a time on the phone/tablet.
                app.state.map_controls_open = false;
                for (other_slot, other) in app.pane_apps.iter_mut().enumerate() {
                    if other_slot != slot
                        && let Some(other) = other
                    {
                        other.state.map_controls_open = false;
                    }
                }
            }
        }
    }

    let product = pane.state.moment;
    let product_changed = app
        .state
        .secondary_radar_panes
        .get(slot)
        .is_some_and(|pane| pane.product != Some(product));
    if product_changed && let Some(meta) = app.state.secondary_radar_panes.get_mut(slot) {
        meta.product = Some(product);
    }
    app.pane_apps[slot] = Some(pane_app);
    if product_changed {
        app.state.persist_pane_defaults();
    }
}

/// Long-press detection for a secondary pane. Unlike the primary map, this
/// toggles the *main* app's fullscreen, so a long press works on either pane.
fn secondary_long_press(ctx: &egui::Context, app: &mut StormApp, rect: Rect) {
    let down = ctx.input(|i| i.pointer.any_down());
    if !down {
        app.state.fullscreen_press_handled = false;
        return;
    }
    if app.state.fullscreen_press_handled {
        return;
    }
    let long = ctx.input(|i| {
        let p = &i.pointer;
        p.latest_pos().is_some_and(|pos| rect.contains(pos))
            && !p.is_decidedly_dragging()
            && p.press_start_time().is_some_and(|t| i.time - t > 0.6)
    });
    if long {
        app.state.fullscreen_press_handled = true;
        crate::ui::set_fullscreen(ctx, app);
    }
}

/// Settings section: pane count and per-pane site selection.
pub fn settings_controls(ui: &mut egui::Ui, app: &mut StormApp) {
    ui.add_space(6.0);
    ui.label("2D radar panes");
    let max = max_radar_panes();
    ui.horizontal(|ui| {
        for n in 1..=max {
            let selected = app.state.radar_panes == n;
            if ui
                .selectable_label(selected, n.to_string())
                .on_hover_text(if n == 1 {
                    "Single map".to_string()
                } else {
                    format!("{n} independent radar views, each with its own site and map controls")
                })
                .clicked()
            {
                app.state.radar_panes = n;
                // Shrinking the layout must also stop the hidden pane apps'
                // background threads, or they keep using S3 while invisible.
                app.pane_apps.truncate(n.saturating_sub(1));
                app.state.ensure_radar_panes();
                if n > 1 {
                    app.state.view3d_open = false;
                }
                app.state.persist_pane_defaults();
            }
        }
    });
    if app.state.radar_panes <= 1 {
        ui.label(
            egui::RichText::new(if cfg!(target_os = "android") {
                "Up to 2 panes on Android."
            } else {
                "Up to 4 independent radar panes."
            })
            .weak()
            .small(),
        );
        return;
    }

    let sites: Vec<String> = app
        .data
        .lock()
        .unwrap()
        .sites
        .iter()
        .map(|site| site.id.to_string())
        .collect();
    for slot in 0..app.state.radar_panes.saturating_sub(1) {
        let Some(pane) = app.state.secondary_radar_panes.get(slot) else {
            continue;
        };
        let mut selected = pane.site_id.clone();
        ui.horizontal(|ui| {
            ui.label(format!("Pane {}", slot + 2));
            egui::ComboBox::from_id_salt(("radar-pane-site", slot))
                .selected_text(if selected.is_empty() {
                    "Select site".to_string()
                } else {
                    selected.clone()
                })
                .show_ui(ui, |ui| {
                    for site in &sites {
                        ui.selectable_value(&mut selected, site.clone(), site);
                    }
                });
        });
        if selected != app.state.secondary_radar_panes[slot].site_id {
            app.state.set_secondary_site(slot, &selected);
            if let Some(pane_app) = app.pane_apps.get_mut(slot) {
                *pane_app = None;
            }
            app.state.persist_pane_defaults();
        }
    }
    ui.label(
        egui::RichText::new(
            "Pane 1 owns the app-wide timeline. Every pane has its own radar site, layers, product and tilt.",
        )
        .weak()
        .small(),
    );
}
