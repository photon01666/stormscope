//! Minimal on-screen keyboard for Android.
//!
//! winit's Android backend does not forward IME/soft-keyboard text input, so
//! typed text never reaches egui. This keyboard is drawn in egui and injects
//! the events the focused TextEdit would normally receive. Only letters,
//! digits, space, backspace and enter are provided — enough to name a marker
//! or ask the AI a question on a phone.

use crate::app::StormApp;

const ROWS: [&[&str]; 4] = [
    &["q", "w", "e", "r", "t", "y", "u", "i", "o", "p"],
    &["a", "s", "d", "f", "g", "h", "j", "k", "l"],
    &["z", "x", "c", "v", "b", "n", "m", "⌫", "⏎"],
    &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "✕"],
];

/// Draw the keyboard (bottom of the screen) while a text field has focus, and
/// push the pressed keys into egui's input stream.
pub fn show(ctx: &egui::Context, app: &mut StormApp) {
    if !app.state.text_focus {
        return;
    }
    let mut events: Vec<egui::Event> = Vec::new();
    egui::Area::new(egui::Id::new("stormscope.virtual-keyboard"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, 0.0))
        .order(egui::Order::Foreground)
        .interactable(true)
        .movable(false)
        .constrain(false)
        .fade_in(false)
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(egui::Color32::from_black_alpha(215))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(6, 6))
                .show(ui, |ui| {
                    for row in ROWS {
                        ui.horizontal(|ui| {
                            for &key in row {
                                let (label, w) = match key {
                                    "⌫" => ("⌫", 52.0),
                                    "⏎" => ("⏎", 52.0),
                                    "✕" => ("✕", 52.0),
                                    _ => (key, 40.0),
                                };
                                if ui.add_sized([w, 44.0], egui::Button::new(label)).clicked() {
                                    match key {
                                        "⌫" => events.push(egui::Event::Key {
                                            key: egui::Key::Backspace,
                                            physical_key: None,
                                            pressed: true,
                                            repeat: false,
                                            modifiers: egui::Modifiers::NONE,
                                        }),
                                        "⏎" => events.push(egui::Event::Key {
                                            key: egui::Key::Enter,
                                            physical_key: None,
                                            pressed: true,
                                            repeat: false,
                                            modifiers: egui::Modifiers::NONE,
                                        }),
                                        "✕" => app.state.text_focus = false,
                                        _ => events.push(egui::Event::Text(key.to_string())),
                                    }
                                }
                            }
                        });
                    }
                });
        });
    if !events.is_empty() {
        ctx.input_mut(|i| i.events.extend(events));
    }
}
