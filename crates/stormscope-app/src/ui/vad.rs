//! VAD wind-profile window: a radar-derived vertical wind profile for the
//! current volume.

use crate::app::StormApp;
use std::sync::Arc;

pub fn vad_window(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    if !app.state.vad_open {
        return;
    }
    let Some(volume) = app.state.current().map(|f| Arc::clone(&f.volume)) else {
        return;
    };
    let key = volume.meta.volume_start.timestamp_millis() as u64;
    let needs = app.vad_cache.as_ref().map(|(k, _)| *k) != Some(key);
    if needs {
        let profile = radar_core::vad::vad_profile(&volume);
        app.vad_cache = Some((key, profile));
    }
    let Some((_, profile)) = &app.vad_cache else {
        return;
    };

    let fill = cfg!(target_os = "android");
    let scale = super::big_text_scale(app);
    let screen = ctx.content_rect().size();
    let size = super::popup_size(ctx, scale, 460.0, 480.0);
    let max_h = super::popup_max_height(ctx, scale);
    let mut open = app.state.vad_open;
    let mut window = egui::Window::new("VAD wind profile")
        .id(egui::Id::new("stormscope.vad"))
        .default_pos(super::centered_popup_pos(ctx, size))
        .max_width((screen.x - 16.0).max(200.0))
        .min_size(egui::vec2(360.0 * scale, 220.0 * scale));
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
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut close = false;
    window.open(&mut open).show(ctx, |ui| {
        #[cfg(target_os = "android")]
        if fill {
            close |= super::phone_header(ui, "VAD wind profile");
        }
        super::apply_text_scale(ui, scale);
        ui.label(
            egui::RichText::new("Radar-derived wind profile · current volume")
                .weak()
                .small(),
        )
        .on_hover_text(
            "Velocity azimuth display: a least-squares fit of radial velocity around a range \
             ring. One wind level per elevation cut, not a model sounding.",
        );
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Height").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new("Wind").strong());
            });
        });
        egui::ScrollArea::vertical()
            .max_height((max_h - 110.0 * scale).max(140.0))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if profile.levels.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "No usable VAD ring in this volume. The lowest tilts need enough \
                             valid velocity gates around 20 km.",
                        )
                        .weak(),
                    );
                    return;
                }
                for level in profile.levels.iter().rev() {
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "{:.1} kft · {:.0} m",
                            level.height_m * 3.280_84 / 1000.0,
                            level.height_m
                        ));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(format!(
                                "{:.0} mph from {}°",
                                level.speed_ms * 2.236_94,
                                level.dir_deg.round() as u16
                            ));
                        });
                    });
                    ui.label(
                        egui::RichText::new(format!(
                            "u {:+.1} m/s · v {:+.1} m/s · rms {:.1} m/s · {} gates",
                            level.u_ms, level.v_ms, level.rms_ms, level.gates
                        ))
                        .weak()
                        .small(),
                    );
                }
            });
        ui.separator();
        ui.label(
            egui::RichText::new(
                "VAD gives one horizontal wind per elevation. It can miss curved or storm-relative \
                 flow; use it with the velocity product.",
            )
            .weak()
            .small(),
        );
    });
    if close {
        open = false;
    }
    app.state.vad_open = open;
}
