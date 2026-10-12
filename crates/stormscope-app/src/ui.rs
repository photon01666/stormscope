//! Top-level layout: side panel (controls), central map, bottom timeline.

use crate::app::StormApp;
use crate::state::Mode;
use radar_core::Moment;

mod map;

pub fn layout(ui: &mut egui::Ui, app: &mut StormApp) {
    // Top status bar.
    egui::Panel::top("status").show(ui, |ui| {
        status_bar(ui, app);
    });

    // Bottom timeline scrubber.
    egui::Panel::bottom("timeline").show(ui, |ui| {
        crate::timeline::timeline(ui, app);
    });

    // Left controls.
    egui::Panel::left("controls")
        .resizable(true)
        .default_size(220.0)
        .show(ui, |ui| {
            controls(ui, app);
        });

    // Central map canvas.
    egui::CentralPanel::default().show(ui, |ui| {
        crate::ui::map::map(ui, app);
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
    ui.heading("Controls");
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
