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
            ui.label(format!("scan {}", n.format("%H:%M:%S UTC")));
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
        }
        let mut arch = app.state.mode == Mode::Archive;
        if ui.selectable_value(&mut arch, true, "Archive").changed() {
            app.state.mode = Mode::Archive;
        }
    });

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

    // Alerts toggle.
    ui.add_space(6.0);
    ui.checkbox(&mut app.state.show_alerts, "Show warnings");
}
