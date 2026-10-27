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

        let rows = obs_rows(&station);
        if rows.is_empty() {
            ui.label("No observation data available.");
        } else {
            egui::Grid::new("stormscope.synoptic.grid")
                .num_columns(2)
                .spacing((8.0, 4.0))
                .show(ui, |ui| {
                    for (label, value) in &rows {
                        ui.label(egui::RichText::new(label).strong());
                        ui.label(value);
                        ui.end_row();
                    }
                });
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

/// Flatten a station's observations into display rows (label, "value unit").
/// Shared by the full popup and the hover tooltip.
pub(super) fn obs_rows(st: &radar_data::synoptic::SynopticStation) -> Vec<(String, String)> {
    let Some(obj) = st.obs.as_object() else {
        return Vec::new();
    };
    let mut rows = Vec::with_capacity(obj.len());
    for (k, v) in obj {
        let base = base_var(k);
        let label = humanize_key(base);
        let unit = st.units.get(base).and_then(|u| u.as_str()).unwrap_or("");
        let value = format_obs(v);
        rows.push((
            label,
            if unit.is_empty() {
                value
            } else {
                format!("{value} {unit}")
            },
        ));
    }
    rows
}

/// Strip Synoptic's `_value_N` / `_set_N` suffix so `air_temp_value_1`
/// becomes `air_temp` (the key used by the station's `UNITS` map).
fn base_var(k: &str) -> &str {
    for suffix in ["_value_", "_set_"] {
        if let Some(i) = k.find(suffix) {
            return &k[..i];
        }
    }
    k
}

fn humanize_key(k: &str) -> String {
    let spaced = k.replace('_', " ");
    // Title-case each word.
    spaced
        .split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_obs(v: &serde_json::Value) -> String {
    if let Some(nested) = v.get("value") {
        return format_obs(nested);
    }
    match v {
        serde_json::Value::Null => "—".into(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => {
            // Trim needless trailing zeros while keeping useful precision.
            if let Some(f) = n.as_f64() {
                let s = format!("{f:.2}");
                s.trim_end_matches('0').trim_end_matches('.').to_string()
            } else {
                n.to_string()
            }
        }
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
