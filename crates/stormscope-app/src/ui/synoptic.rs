//! Synoptic (Mesowest) station popup.
//!
//! Shows the latest observation for one station. Mirrors the forecast window's
//! platform split: on Android it fills the map pane edge to edge (no title bar,
//! no drag/resize); on desktop it is a resizable, movable, centred dialog.

use crate::app::StormApp;

pub fn synoptic_window(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    if !app.state.synoptic_open {
        return;
    }
    let fill = cfg!(target_os = "android");
    let scale = super::big_text_scale(app);
    let screen = ctx.content_rect().size();
    let size = super::popup_size(ctx, scale, 560.0, 560.0);
    let max_h = super::popup_max_height(ctx, scale);
    let min_size = egui::vec2(
        (400.0 * scale).min((screen.x - 24.0).max(200.0)),
        (200.0 * scale).min((screen.y - 90.0).max(160.0)),
    );
    let mut open = app.state.synoptic_open;
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut close_requested = false;

    let mut window = egui::Window::new("Station observation")
        .id(egui::Id::new("stormscope.synoptic"))
        .default_pos(super::centered_popup_pos(ctx, size))
        .max_width((screen.x - 16.0).max(200.0))
        .min_size(min_size);
    #[cfg(target_os = "android")]
    if fill {
        window = window.title_bar(false);
    }
    window = if fill {
        let inset = area.shrink2(egui::vec2(4.0, 8.0));
        window
            .fixed_rect(inset)
            .movable(false)
            .resizable(false)
            .constrain_to(inset)
    } else {
        window.default_size(size).max_height(max_h).resizable(true)
    };
    let window = window.open(&mut open);
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut _cr = false;
    window.show(ctx, |ui| {
        #[cfg(target_os = "android")]
        if fill {
            close_requested |= super::phone_header(ui, "Station observation");
        }
        super::apply_text_scale(ui, scale);

        let station = app.state.synoptic_selected.as_ref().and_then(|stid| {
            app.state
                .synoptic_stations
                .iter()
                .find(|s| &s.stid == stid)
                .cloned()
        });

        let Some(station) = station else {
            ui.label("No station selected.");
            return;
        };

        ui.label(egui::RichText::new(station.label()).strong());
        if !station.network.is_empty() {
            ui.label(egui::RichText::new(&station.network).weak().small());
        }
        ui.label(egui::RichText::new(&station.stid).weak().small());
        if let Some(e) = station.elevation_m {
            ui.label(
                egui::RichText::new(format!("elev {:.0} m", e))
                    .weak()
                    .small(),
            );
        }
        ui.separator();

        if let Some(obj) = station.obs.as_object() {
            egui::Grid::new("stormscope.synoptic.grid")
                .num_columns(2)
                .spacing((8.0, 4.0))
                .show(ui, |ui| {
                    for (k, v) in obj {
                        ui.label(egui::RichText::new(humanize_key(k)).strong());
                        ui.label(format_value(v));
                        ui.end_row();
                    }
                });
        } else {
            ui.label("No observation data available.");
        }

        if app.state.synoptic_pending {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading…");
            });
        }
        if let Some(e) = &app.state.synoptic_error {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(e)
                    .small()
                    .color(egui::Color32::from_rgb(255, 120, 120)),
            );
        }
    });
    if close_requested {
        open = false;
    }
    app.state.synoptic_open = open;
}

fn humanize_key(k: &str) -> String {
    let mut out = String::new();
    for (i, ch) in k.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    // snake_case → spaced
    out.replace('_', " ")
}

fn format_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "—".into(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
