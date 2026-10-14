//! Top-level layout: auto-hiding controls sidebar, central map, bottom timeline.

use crate::app::StormApp;
use crate::state::Mode;
use radar_core::Moment;

mod map;

/// Fixed sidebar width (screen px) used by both the slide offset and the
/// hover hysteresis below — keep the two in sync.
const SIDEBAR_WIDTH: f32 = 250.0;
/// Pointer distance from the left edge that peeks the sidebar open.
const SIDEBAR_EDGE_PX: f32 = 16.0;
/// Extra x-range past the sidebar that still counts as "inside" so popups,
/// sliders and combo boxes don't slam it shut mid-interaction.
const SIDEBAR_CLOSE_SLOP: f32 = 32.0;
/// Slide animation time (seconds).
const SIDEBAR_ANIM_SECS: f32 = 0.18;

pub fn layout(ui: &mut egui::Ui, app: &mut StormApp) {
    // Top status bar.
    egui::Panel::top("status").show(ui, |ui| {
        status_bar(ui, app);
    });

    // Bottom timeline scrubber.
    egui::Panel::bottom("timeline").show(ui, |ui| {
        crate::timeline::timeline(ui, app);
    });

    // Middle rect (between top/bottom bars) before CentralPanel consumes it —
    // the overlay sidebar is positioned in these screen coords so it never
    // covers the status bar or timeline.
    let middle = ui.available_rect_before_wrap();

    // Central map canvas takes the full middle width; the sidebar floats
    // above it instead of reserving space, so peeking it in never resizes
    // the map (and never triggers a radar re-raster).
    egui::CentralPanel::default().show(ui, |ui| {
        crate::ui::map::map(ui, app);
    });

    // Floating auto-hide sidebar, drawn over the map.
    sidebar_overlay(ui.ctx(), app, middle);

    // "Really clear …?" confirmation, drawn over everything. Rendered after
    // the panels so it isn't clipped by them; the Settings menu closes
    // itself before opening this.
    confirm_clear_dialog(ui.ctx(), app);
}

/// Extra text/element scale for the sidebar on high-res monitors.
///
/// Keyed off the *monitor's physical height* (`monitor points × native
/// scale`), not the window size: a small window on a 4K screen still gets
/// the boost. The boost tops the sidebar up so its total on-screen size
/// matches a 1080p baseline — i.e. `extra = (phys / 1080) / os_scale`:
///
/// * 4K @1x OS scale → 2.5x sidebar text (capped)
/// * 4K @1.5x OS scale (e.g. Hyprland 4K) → ~1.67x on top of the 1.5x
/// * 4K @2x OS scale → 1.0 (the OS already doubled everything)
/// * 1080p → 1.0
///
/// Unknown monitor falls back to the window's physical height. Boost is
/// doubled past the baseline and capped at 2.5x.
fn hidpi_text_scale(
    monitor_points_h: Option<f32>,
    native_ppp: Option<f32>,
    pixels_per_point: f32,
    window_logical_h: f32,
) -> f32 {
    let phys_h = match (monitor_points_h, native_ppp) {
        (Some(mh), Some(nppp)) => mh * nppp,
        _ => window_logical_h * pixels_per_point,
    };
    let raw = (phys_h / 1080.0) / pixels_per_point.max(0.5);
    (1.0 + (raw - 1.0) * 2.0).clamp(1.0, 2.5)
}

/// Enlarge sidebar fonts + spacing for the high-res `scale` above. Applied
/// to the sidebar Area's root Ui each frame (a fresh style copy per frame,
/// so sizes never compound). Combo/menu popups live in their own layers and
/// keep context style.
fn apply_sidebar_text_scale(ui: &mut egui::Ui, scale: f32) {
    if (scale - 1.0).abs() < 0.01 {
        return;
    }
    let style = ui.style_mut();
    for font in style.text_styles.values_mut() {
        font.size = (font.size * scale).clamp(9.0, 64.0);
    }
    style.spacing.item_spacing *= scale;
    style.spacing.button_padding *= scale;
    style.spacing.indent *= scale;
    style.spacing.slider_width *= scale;
    style.spacing.combo_width *= scale;
    style.spacing.text_edit_width *= scale;
}

/// Pure open/close decision for the auto-hide sidebar (kept free of egui
/// input plumbing so it can be unit-tested).
///
/// * `pinned` — user asked to keep the sidebar open.
/// * `was_open` — the target from last frame (drives the hysteresis: a
///   narrow edge zone opens it, a wider zone keeps it open).
/// * `pressing` — any pointer button held: suppresses peeking open so a map
///   pan ending at the left edge doesn't pop the sidebar mid-drag.
/// * `pointer` — latest pointer position in screen coords (`None` when the
///   pointer left the window / touch without hover).
/// * `middle` — the rect between the top status bar and bottom timeline.
fn sidebar_want_open(
    pinned: bool,
    was_open: bool,
    pressing: bool,
    pointer: Option<egui::Pos2>,
    middle: egui::Rect,
    panel_w: f32,
) -> bool {
    if pinned {
        return true;
    }
    let Some(p) = pointer else {
        return false;
    };
    if !middle.y_range().contains(p.y) {
        return false;
    }
    // Narrow zone at the left edge peeks it open (never mid-gesture).
    if !pressing && p.x - middle.min.x < SIDEBAR_EDGE_PX {
        return true;
    }
    // Wide zone keeps it open while interacting (sliders, combos, popups).
    was_open && p.x < middle.min.x + panel_w + SIDEBAR_CLOSE_SLOP
}

/// Full-height target for the sidebar panel: the middle rect minus a small
/// margin, floored so it stays usable on short windows.
fn sidebar_panel_height(middle: egui::Rect) -> f32 {
    (middle.height() - 16.0).max(120.0)
}

/// Auto-hiding controls sidebar: slides in from the left when the pointer
/// touches the left edge (or when pinned), slides back out once the pointer
/// leaves the sidebar zone.
fn sidebar_overlay(ctx: &egui::Context, app: &mut StormApp, middle: egui::Rect) {
    // High-res auto-scale from the *monitor*: larger panel + text on tall
    // physical displays (e.g. 4K), topping up whatever OS scaling exists.
    let ppp = ctx.pixels_per_point();
    let (mon_h, nppp) = ctx.input(|i| {
        (
            i.viewport().monitor_size.map(|s| s.y),
            i.viewport().native_pixels_per_point,
        )
    });
    let scale = hidpi_text_scale(mon_h, nppp, ppp, middle.height());
    app.sidebar_scale = scale;
    let panel_w = SIDEBAR_WIDTH * scale;

    let pointer = ctx.pointer_latest_pos();
    // Don't peek open mid-gesture: a map pan ending at the left edge must
    // not pop the sidebar over the map mid-drag.
    let pressing = ctx.input(|i| i.pointer.any_down());
    let want_open = sidebar_want_open(
        app.state.sidebar_pinned,
        app.sidebar_open,
        pressing,
        pointer,
        middle,
        panel_w,
    );
    app.sidebar_open = want_open;

    let frac = ctx.animate_bool_with_time(
        egui::Id::new("stormscope.sidebar"),
        want_open,
        SIDEBAR_ANIM_SECS,
    );

    if frac <= 0.001 {
        edge_hint(ctx, middle, scale);
        return;
    }
    if frac < 1.0 {
        ctx.request_repaint();
    }

    // Slide from fully off-screen (left of the middle rect) to docked.
    let x_off = -(panel_w + 12.0) * (1.0 - frac);
    let pos = egui::pos2(middle.min.x + x_off, middle.min.y);

    egui::Area::new(egui::Id::new("stormscope.sidebar.area"))
        .fixed_pos(pos)
        .movable(false)
        .interactable(true)
        .constrain(false)
        .fade_in(false)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            apply_sidebar_text_scale(ui, scale);
            let frame = egui::Frame::side_top_panel(ui.style());
            frame.show(ui, |ui| {
                ui.set_min_width(panel_w);
                ui.set_max_width(panel_w);
                // Pin the full middle height: a bare Area sizes to its
                // content, which left the sidebar as a short floating box.
                // With the height fixed, long control lists scroll inside
                // instead of spilling over the timeline.
                let h = sidebar_panel_height(middle);
                ui.set_min_height(h);
                ui.set_max_height(h);
                egui::ScrollArea::vertical()
                    .max_height(h)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        controls(ui, app);
                    });
            });
        });
}

/// Slim "›" tab at the left edge while the sidebar is hidden — a visual cue
/// that controls live there. Non-interactive (clicks pass through to the
/// map); hovering it already peeks the sidebar via the edge zone.
fn edge_hint(ctx: &egui::Context, middle: egui::Rect, scale: f32) {
    let cy = middle.center().y;
    let pos = egui::pos2(middle.min.x + 3.0, cy - 26.0);
    egui::Area::new(egui::Id::new("stormscope.sidebar.hint"))
        .fixed_pos(pos)
        .movable(false)
        .interactable(false)
        .constrain(false)
        .fade_in(false)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(egui::Color32::from_black_alpha(150))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(5, 10))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new("❯")
                            .color(egui::Color32::from_white_alpha(200))
                            .size(14.0 * scale),
                    );
                });
        });
}

fn status_bar(ui: &mut egui::Ui, app: &mut StormApp) {
    ui.horizontal(|ui| {
        ui.strong("stormscope");
        ui.separator();
        ui.label(format!("site {}", app.state.site_id));
        ui.label(format!("{}", app.state.moment.label()));
        ui.label(format!("{:.1}°", app.state.tilt_deg));
        if let Some(n) = app.state.newest_start() {
            ui.label(format!("scan {}", crate::timefmt::fmt_central(&n)));
        }
        if let Some(lat) = app.state.latency() {
            ui.label(format!("+{}m", lat.num_minutes()));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(format!("{} alerts", app.state.alerts.len()));
            ui.label(app.state.status_text.clone());
        });
    });
}

fn controls(ui: &mut egui::Ui, app: &mut StormApp) {
    ui.horizontal(|ui| {
        ui.heading("Controls");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            settings_menu(ui, app);
            ui.checkbox(&mut app.state.sidebar_pinned, "Pin")
                .on_hover_text("Keep the sidebar open instead of auto-hiding");
        });
    });
    if !app.state.sidebar_pinned {
        ui.label(
            egui::RichText::new("hover the left edge to peek · Pin to keep open")
                .weak()
                .small(),
        );
    }
    if app.sidebar_scale > 1.01 {
        ui.label(
            egui::RichText::new(format!(
                "high-res display · sidebar ×{:.2}",
                app.sidebar_scale
            ))
            .weak()
            .small(),
        );
    }
    ui.add_space(6.0);

    // Mode toggle.
    ui.horizontal(|ui| {
        let mut live = app.state.mode == Mode::Live;
        if ui.selectable_value(&mut live, true, "Live").changed() {
            app.state.mode = Mode::Live;
            let _ = app.tx.send(crate::app::DataCmd::EnterLive);
        }
        let mut arch = app.state.mode == Mode::Archive;
        if ui.selectable_value(&mut arch, true, "Archive").changed() {
            app.state.mode = Mode::Archive;
        }
    });

    // Loop the last hour — loads the newest ~13 scans into the ring and
    // switches to Archive so the playhead doesn't auto-jump as live frames
    // keep arriving.
    ui.add_space(4.0);
    if ui
        .button("⟳ Loop last hour")
        .on_hover_text("Load the newest hour of scans and loop them")
        .clicked()
    {
        app.state.mode = Mode::Archive;
        let _ = app.tx.send(crate::app::DataCmd::LoadLastHour(
            app.state.site_id.clone(),
        ));
    }

    // Archive date picker — only meaningful in Archive mode.
    if app.state.mode == Mode::Archive {
        ui.add_space(6.0);
        ui.label("Archive day (Central)");
        ui.horizontal(|ui| {
            if ui.button("◀").clicked() {
                app.state.archive_date = app.state.archive_date
                    .pred_opt()
                    .unwrap_or(app.state.archive_date);
            }
            ui.label(
                egui::RichText::new(app.state.archive_date.format("%a %b %-d, %Y").to_string())
                    .strong(),
            );
            if ui.button("▶").clicked() {
                let today = chrono::Utc::now()
                    .with_timezone(crate::timefmt::central())
                    .date_naive();
                if app.state.archive_date < today {
                    app.state.archive_date = app.state.archive_date
                        .succ_opt()
                        .unwrap_or(app.state.archive_date);
                }
            }
        });
        if ui.button("Load day").clicked() {
            let _ = app.tx.send(crate::app::DataCmd::LoadArchiveDate(
                app.state.site_id.clone(),
                app.state.archive_date,
            ));
        }
        // Window paging: the day is listed newest-first; a window of
        // RING_CAPACITY scans is loaded at a time. Page earlier/later through
        // the whole day.
        if app.state.archive_total > 0 {
            ui.add_space(4.0);
            let first = app.state.archive_start + 1;
            let last = app.state.archive_start + app.state.archive_len;
            ui.horizontal(|ui| {
                let at_oldest = app.state.archive_start == 0;
                if ui
                    .add_enabled(at_oldest, egui::Button::new("◀ earlier"))
                    .clicked()
                {
                    let _ = app.tx.send(crate::app::DataCmd::ArchiveWindowShift(-1));
                }
                if ui
                    .add_enabled(
                        last < app.state.archive_total,
                        egui::Button::new("later ▶"),
                    )
                    .clicked()
                {
                    let _ = app.tx.send(crate::app::DataCmd::ArchiveWindowShift(1));
                }
            });
            ui.label(
                egui::RichText::new(format!(
                    "scans {}–{} of {} (newest first)",
                    first, last, app.state.archive_total
                ))
                .weak()
                .small(),
            );
        } else {
            ui.label(
                egui::RichText::new("loads that day's scans oldest→newest; then use Play")
                    .weak()
                    .small(),
            );
        }
    }

    // Site selector.
    ui.add_space(6.0);
    ui.label("Radar site");
    let sites: Vec<&str> = app
        .data
        .lock()
        .unwrap()
        .sites
        .iter()
        .map(|s| s.id)
        .collect();
    egui::ComboBox::from_id_salt("site")
        .selected_text(app.state.site_id.clone())
        .show_ui(ui, |ui| {
            for id in sites {
                ui.selectable_value(&mut app.state.site_id, id.to_string(), id);
            }
        });
    if ui.button("Apply site").clicked() {
        let _ = app.tx.send(crate::app::DataCmd::SetSite(app.state.site_id.clone()));
    }

    // Product (moment) selector.
    ui.add_space(6.0);
    ui.label("Product");
    let moments = [
        Moment::Reflectivity,
        Moment::Velocity,
        Moment::SpectrumWidth,
        Moment::DifferentialReflectivity,
        Moment::CorrelationCoefficient,
        Moment::SpecificDifferentialPhase,
        Moment::DifferentialPhase,
    ];
    ui.horizontal_wrapped(|ui| {
        for m in moments {
            let selected = app.state.moment == m;
            if ui.selectable_label(selected, m.label()).clicked() {
                app.state.moment = m;
            }
        }
    });

    // Tilt selector.
    ui.add_space(6.0);
    let tilts = app.data.lock().unwrap().tilts.clone();
    ui.label("Tilt (°)");
    let mut tilt_idx = tilts
        .iter()
        .enumerate()
        .min_by(|a, b| {
            (a.1 - app.state.tilt_deg)
                .abs()
                .partial_cmp(&(b.1 - app.state.tilt_deg))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(0);
    egui::ComboBox::from_id_salt("tilt")
        .selected_text(format!("{:.1}°", tilts.get(tilt_idx).copied().unwrap_or(0.5)))
        .show_ui(ui, |ui| {
            for (i, t) in tilts.iter().enumerate() {
                ui.selectable_value(&mut tilt_idx, i, format!("{t:.1}°"));
            }
        });
    if let Some(t) = tilts.get(tilt_idx) {
        app.state.tilt_deg = *t;
    }

    // Opacity.
    ui.add_space(6.0);
    ui.label("Opacity");
    ui.add(egui::Slider::new(&mut app.state.opacity, 0.0..=1.0));

    // Min dBZ filter — suppresses weak returns (clear-air clutter/noise).
    // Only affects Reflectivity-family products.
    ui.add_space(6.0);
    ui.label("Min dBZ (REF)");
    ui.add(egui::Slider::new(&mut app.state.min_dbz, 0.0..=75.0).step_by(5.0));
    ui.label(
        egui::RichText::new("filters weak returns on REF products")
            .weak()
            .small(),
    );

    // Alerts toggle.
    ui.add_space(6.0);
    ui.checkbox(&mut app.state.show_alerts, "Show warnings");
    ui.checkbox(&mut app.state.show_tracks, "Show storm tracks");

    // --- SPC convective outlook overlay ------------------------------------
    ui.add_space(6.0);
    let toggled = ui.checkbox(&mut app.state.show_outlook, "Show SPC outlook").changed();
    if toggled && app.state.show_outlook {
        // Fetch the selected day fresh when the layer is switched on.
        let _ = app.tx.send(crate::app::DataCmd::LoadOutlook(app.state.outlook_day));
    }
    if app.state.show_outlook {
        ui.horizontal(|ui| {
            for d in 1u8..=3 {
                if ui
                    .selectable_label(app.state.outlook_day == d, format!("Day {d}"))
                    .clicked()
                {
                    app.state.outlook_day = d;
                    let _ = app.tx.send(crate::app::DataCmd::LoadOutlook(d));
                }
            }
        });
        match &app.state.outlook {
            Some(o) if o.day == app.state.outlook_day => {
                let issued = o
                    .issue
                    .map(|t| crate::timefmt::fmt_central(&t))
                    .unwrap_or_else(|| "?".into());
                ui.label(
                    egui::RichText::new(format!("{} — issued {issued}", o.forecaster))
                        .weak()
                        .small(),
                );
            }
            _ => {
                ui.label(
                    egui::RichText::new(format!("loading day {}…", app.state.outlook_day))
                        .weak()
                        .small(),
                );
            }
        }
        ui.label("Outlook fill");
        ui.add(egui::Slider::new(&mut app.state.outlook_opacity, 0.0..=1.0));
        ui.label(
            egui::RichText::new("risk fills sit under the radar; outlines on top")
                .weak()
                .small(),
        );
    }

    // --- Measure tool (RadarScope-style distance line + blue bubble) -----
    ui.add_space(8.0);
    ui.label("Measure");
    ui.horizontal(|ui| {
        let label = if app.state.measure_mode {
            "✕ Stop measuring"
        } else {
            "📏 Measure distance"
        };
        if ui
            .button(label)
            .on_hover_text("Drag on the map to measure — ends snap to pins")
            .clicked()
        {
            app.state.measure_mode = !app.state.measure_mode;
            app.state.measure_drag = None;
        }
        if (app.state.measure_a.is_some() || app.state.measure_b.is_some())
            && ui
                .button("Clear")
                .on_hover_text("Clear the measurement")
                .clicked()
        {
            app.state.measure_a = None;
            app.state.measure_b = None;
            app.state.measure_drag = None;
            app.state.measure_bubble_rect = None;
        }
    });
    match (app.state.measure_mode, app.state.measure_a, app.state.measure_b) {
        (true, Some(a), Some(b)) => {
            let m = radar_render::surface_distance_m(a.0, a.1, b.0, b.1);
            ui.label(
                egui::RichText::new(format!(
                    "{} · tap bubble for km/brg",
                    crate::ui::map::format_miles(m / 1609.344)
                ))
                .strong(),
            );
        }
        (true, _, _) => {
            ui.label(
                egui::RichText::new("drag on the map — ends snap to pins")
                    .weak()
                    .small(),
            );
        }
        (false, _, _) => {}
    }

    // --- User-placed map markers ------------------------------------------
    ui.add_space(8.0);
    ui.label("Markers");
    ui.horizontal(|ui| {
        let label = if app.state.add_marker_mode {
            "✕ Cancel placement"
        } else {
            "＋ Place marker"
        };
        if ui
            .button(label)
            .on_hover_text("Then click the map (Shift+click works too)")
            .clicked()
        {
            app.state.add_marker_mode = !app.state.add_marker_mode;
        }
        if !app.state.markers.is_empty()
            && ui
                .button("Clear all")
                .on_hover_text("Remove every marker")
                .clicked()
        {
            app.state.markers.clear();
            app.state.pending_marker = None;
            app.state.pending_marker_label.clear();
            app.state.persist_markers();
        }
    });

    // Rows: click = pan to it, ✎ = rename, ✕ = delete.
    let mut pan_to: Option<(f64, f64)> = None;
    let mut edit: Option<usize> = None;
    let mut del: Option<usize> = None;
    for (i, m) in app.state.markers.iter().enumerate() {
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::new(m.display()).wrap())
                .on_hover_text(format!(
                    "{:.4}, {:.4} — click to center the map",
                    m.lat, m.lon
                ))
                .clicked()
            {
                pan_to = Some((m.lon, m.lat));
            }
            if ui.small_button("✎").on_hover_text("Rename").clicked() {
                edit = Some(i);
            }
            if ui.small_button("✕").on_hover_text("Delete").clicked() {
                del = Some(i);
            }
        });
        // Incoming-cell ETA under the row, when the tracker has one.
        if let Some(eta) = app.marker_etas.get(i).and_then(|e| e.as_ref()) {
            ui.label(
                egui::RichText::new(format!(
                    "⛈ ~{} · {:.0} mph {}",
                    crate::ui::map::fmt_eta(eta.minutes),
                    eta.speed_mph,
                    eta.compass
                ))
                .weak()
                .small(),
            );
        }
    }
    if app.state.markers.is_empty() {
        ui.label(
            egui::RichText::new("Shift+click the map to drop a pin")
                .weak()
                .small(),
        );
    }
    if let Some((lon, lat)) = pan_to {
        app.state.view.center_lon = lon;
        app.state.view.center_lat = lat;
    }
    if let Some(i) = edit {
        // Seed the persistent draft the popup edits (immediate-mode TextEdit).
        app.state.pending_marker_label =
            app.state.markers.get(i).map(|m| m.label.clone()).unwrap_or_default();
        app.state.pending_marker = Some(i);
    }
    if let Some(i) = del {
        if i < app.state.markers.len() {
            app.state.markers.remove(i);
        }
        if app.state.pending_marker == Some(i) {
            app.state.pending_marker = None;
            app.state.pending_marker_label.clear();
        }
        app.state.persist_markers();
    }
}

/// App-level "Settings" popup: live per-section cache usage and one clear
/// button per section (each gated behind [`confirm_clear_dialog`]).
fn settings_menu(ui: &mut egui::Ui, app: &mut StormApp) {
    ui.menu_button("Settings", |ui| {
        ui.set_min_width(260.0);
        ui.label(egui::RichText::new("Disk cache").strong());
        ui.add_space(4.0);
        cache_row(
            ui,
            "Radar data",
            "Downloaded radar volumes",
            app.state.cache_radar_bytes,
            crate::app::ClearTarget::Radar,
            app,
        );
        ui.separator();
        cache_row(
            ui,
            "Map data",
            "Cached basemap tiles",
            app.state.cache_tile_bytes,
            crate::app::ClearTarget::Tiles,
            app,
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("cap 10 GB across both — oldest evicted first")
                .weak()
                .small(),
        );
    });
}

/// One cache section: label, live size, and its "Clear …" button.
fn cache_row(
    ui: &mut egui::Ui,
    label: &str,
    hint: &str,
    bytes: Option<u64>,
    target: crate::app::ClearTarget,
    app: &mut StormApp,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.label(egui::RichText::new(hint).weak().small());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(bytes.map(human_bytes).unwrap_or_else(|| "…".into()))
                    .monospace(),
            );
        });
    });
    let (verb, hover) = match target {
        crate::app::ClearTarget::Radar => (
            "Clear radar data",
            "Delete every downloaded radar volume from disk",
        ),
        crate::app::ClearTarget::Tiles => (
            "Clear map data",
            "Delete every cached basemap tile from disk and memory",
        ),
    };
    if ui.button(verb).on_hover_text(hover).clicked() {
        app.confirm_clear = Some(target);
        ui.close(); // dismiss the menu so the dialog stands alone
    }
}

/// Modal "are you sure?" for the Settings clear buttons. Shows only while
/// `app.confirm_clear` is set; the accepted branch sends the matching
/// command (deletion runs off-thread in the coordinator) and, for map data,
/// also drops the UI's in-memory tile textures.
fn confirm_clear_dialog(ctx: &egui::Context, app: &mut StormApp) {
    let Some(target) = app.confirm_clear else {
        return;
    };
    let (title, body, verb, cmd) = match target {
        crate::app::ClearTarget::Radar => (
            "Clear radar data?",
            "Delete every downloaded radar volume from disk.\n\n\
             Scans already on screen stay until you reload or switch \
             station; future scans re-download on demand.",
            "Clear radar data",
            crate::app::DataCmd::ClearRadar,
        ),
        crate::app::ClearTarget::Tiles => (
            "Clear map data?",
            "Delete every cached basemap tile from disk and from memory.\n\n\
             The map re-downloads tiles as you pan and zoom.",
            "Clear map data",
            crate::app::DataCmd::ClearTiles,
        ),
    };

    let mut accepted = false;
    let mut dismissed = false;
    let res = egui::containers::Modal::new(egui::Id::new("stormscope.confirm-clear"))
        .show(ctx, |ui| {
            ui.set_min_width(360.0);
            ui.heading(title);
            ui.add_space(8.0);
            ui.label(body);
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    dismissed = true;
                }
                if ui
                    .add(
                        egui::Button::new(verb)
                            .fill(egui::Color32::from_rgb(150, 52, 52)),
                    )
                    .clicked()
                {
                    accepted = true;
                }
            });
        });
    if res.should_close() {
        // Backdrop click or Esc.
        dismissed = true;
    }

    if accepted {
        let _ = app.tx.send(cmd);
        if matches!(target, crate::app::ClearTarget::Tiles) {
            app.tiles.clear_in_memory();
        }
        dismissed = true;
    }
    if dismissed {
        app.confirm_clear = None;
    }
}

/// Human-readable byte size (B/KiB/MiB/GiB) for the Settings menu.
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn middle() -> egui::Rect {
        // Screen-like coords: status bar occupies y 0..30, timeline the
        // bottom 60 px of an 800 px window.
        egui::Rect::from_min_size(egui::pos2(0.0, 30.0), egui::vec2(1280.0, 710.0))
    }

    fn at(x: f32, y: f32) -> Option<egui::Pos2> {
        Some(egui::pos2(x, y))
    }

    fn open(
        pinned: bool,
        was_open: bool,
        pressing: bool,
        pointer: Option<egui::Pos2>,
        middle: egui::Rect,
    ) -> bool {
        sidebar_want_open(pinned, was_open, pressing, pointer, middle, SIDEBAR_WIDTH)
    }

    #[test]
    fn sidebar_opens_on_left_edge_hover() {
        assert!(open(false, false, false, at(5.0, 400.0), middle()));
    }

    #[test]
    fn sidebar_stays_shut_away_from_edge() {
        assert!(!open(false, false, false, at(500.0, 400.0), middle()));
    }

    #[test]
    fn sidebar_suppressed_mid_gesture() {
        // A map pan ending at the left edge must not pop the sidebar.
        assert!(!open(false, false, true, at(5.0, 400.0), middle()));
    }

    #[test]
    fn sidebar_stays_open_inside_wide_zone() {
        // Pointer over the open sidebar (or its popups/sliders) keeps it up.
        assert!(open(false, true, false, at(200.0, 400.0), middle()));
        // Held buttons (e.g. dragging a slider) don't close it either.
        assert!(open(false, true, true, at(200.0, 400.0), middle()));
    }

    #[test]
    fn sidebar_wide_zone_follows_scaled_panel() {
        // A 2x-scaled panel keeps the pointer at x=400 inside its zone.
        assert!(sidebar_want_open(
            false,
            true,
            false,
            at(400.0, 400.0),
            middle(),
            SIDEBAR_WIDTH * 2.0,
        ));
        assert!(!open(false, true, false, at(400.0, 400.0), middle()));
    }

    #[test]
    fn sidebar_closes_when_pointer_leaves_for_map() {
        assert!(!open(false, true, false, at(600.0, 400.0), middle()));
    }

    #[test]
    fn sidebar_ignores_pointer_over_bars_or_off_window() {
        // Over the status bar / timeline: not the sidebar's business.
        assert!(!open(false, false, false, at(5.0, 5.0), middle()));
        assert!(!open(false, true, false, at(5.0, 780.0), middle()));
        // Pointer gone (touch / left window): shut unless pinned.
        assert!(!open(false, true, false, None, middle()));
        assert!(open(true, false, false, None, middle()));
    }

    #[test]
    fn sidebar_pinned_stays_open() {
        assert!(open(true, false, false, at(900.0, 400.0), middle()));
    }

    #[test]
    fn hidpi_scale_tops_up_os_scaling_to_1080p_equivalent() {
        // Plain 1080p: no change.
        assert_eq!(hidpi_text_scale(Some(1080.0), Some(1.0), 1.0, 800.0), 1.0);
        // 4K, no OS scaling: doubled boost, capped at 2.5x.
        assert_eq!(hidpi_text_scale(Some(2160.0), Some(1.0), 1.0, 800.0), 2.5);
        // This box: 4K monitor (1440pt @1.5x Hyprland scale) → ~1.67x extra.
        let s = hidpi_text_scale(Some(1440.0), Some(1.5), 1.5, 700.0);
        assert!((s - (1.0 + (2.0 / 1.5 - 1.0) * 2.0)).abs() < 1e-6, "got {s}");
        // 4K at 200% OS scale: the OS already doubled everything.
        assert_eq!(hidpi_text_scale(Some(1080.0), Some(2.0), 2.0, 700.0), 1.0);
        // 1440p monitor, no OS scaling: ~1.67x.
        let s = hidpi_text_scale(Some(1440.0), Some(1.0), 1.0, 800.0);
        assert!((s - (1.0 + (1440.0 / 1080.0 - 1.0) * 2.0)).abs() < 1e-6, "got {s}");
        // Beyond the cap: clamps at 2.5x.
        assert_eq!(hidpi_text_scale(Some(4320.0), Some(1.0), 1.0, 800.0), 2.5);
        // Unknown monitor: falls back to window physical height.
        assert_eq!(hidpi_text_scale(None, None, 1.0, 500.0), 1.0);
        let s = hidpi_text_scale(None, None, 1.0, 1500.0);
        assert!(
            (s - (1.0 + (1500.0 / 1080.0 - 1.0) * 2.0)).abs() < 1e-6,
            "got {s}"
        );
    }

    #[test]
    fn default_min_dbz_filters_clear_air_clutter() {
        assert_eq!(crate::state::AppState::default().min_dbz, 30.0);
    }

    #[test]
    fn sidebar_panel_height_fills_middle_rect() {
        let h = sidebar_panel_height(middle());
        assert!((h - (710.0 - 16.0)).abs() < 1e-6, "should span the middle rect, got {h}");
    }

    #[test]
    fn sidebar_panel_height_floored_on_short_windows() {
        let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 30.0), egui::vec2(1280.0, 50.0));
        assert_eq!(sidebar_panel_height(tiny), 120.0);
    }
}
