//! Bottom timeline scrubber: play/pause, per-frame ticks with scan times,
//! click-to-seek, and an fps control.

use crate::app::StormApp;
use crate::state::{AppState, LoopState};

pub fn timeline(ui: &mut egui::Ui, app: &mut StormApp) {
    ui.horizontal(|ui| {
        // Play/pause.
        let playing = app.state.loop_state == LoopState::Playing;
        let label = if playing { "Pause" } else { "Play" };
        if ui.button(label).clicked() {
            app.state.loop_state = if playing { LoopState::Paused } else { LoopState::Playing };
        }
        if ui.button("⏮").clicked() {
            app.state.playhead = 0;
        }
        if ui.button("⏭").clicked() {
            app.state.playhead = app.state.ring.len().saturating_sub(1);
        }

        ui.separator();
        ui.label("Loop speed");
        ui.add(
            egui::Slider::new(&mut app.state.fps, 0.25..=30.0)
                .logarithmic(true)
                .fixed_decimals(2),
        );
        ui.label(
            egui::RichText::new(crate::timeline::frame_time_label(app.state.fps))
                .weak()
                .small(),
        );

        ui.separator();
        if let Some(f) = app.state.current() {
            ui.label(crate::timefmt::fmt_central(&f.volume_start));
        } else {
            ui.label("no frames");
        }
    });

    // Scrub strip: one tick per retained frame.
    let n = app.state.ring.len();
    if n == 0 {
        return;
    }
    let height = 18.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, egui::Color32::from_rgb(30, 32, 40));

    let slot = rect.width() / n as f32;
    for (i, _frame) in app.state.ring.iter().enumerate() {
        let x0 = rect.left() + i as f32 * slot;
        let cx = x0 + slot / 2.0;
        let selected = i == app.state.playhead;
        let color = if selected {
            egui::Color32::from_rgb(90, 180, 255)
        } else {
            egui::Color32::from_rgb(70, 74, 86)
        };
        let bar = egui::Rect::from_center_size(
            egui::pos2(cx, rect.center().y),
            egui::vec2((slot * 0.5).max(2.0), if selected { height - 4.0 } else { height - 10.0 }),
        );
        painter.rect_filled(bar, 1.0, color);
    }

    if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let idx = ((pos.x - rect.left()) / slot).floor();
            app.state.playhead = (idx as usize).min(n - 1);
        }
    }
}

/// Compute a human frame-time label for a given fps.
pub fn frame_time_label(fps: f32) -> String {
    format!("{:.0} fps · {:.2}s/frame", fps, 1.0 / fps.max(1.0))
}

#[allow(dead_code)]
fn _unused(_s: &AppState) {}
