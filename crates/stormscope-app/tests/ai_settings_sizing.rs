//! Headless check that the AI settings window hugs its form instead of
//! filling a hard-pinned 620×720 box with a blank band.
//!
//! The window is `fixed_size`, so egui offers the content the full box; what
//! the window finally reports comes from `content_ui.min_size()`. A vertical
//! `ScrollArea` with `auto_shrink` off on the y axis fills the offer even when
//! its content is short (`(true, false) => inner_size[1]` — "fill with blank
//! space"), which pins the window to 720 pt. With `auto_shrink([false, true])`
//! it takes `min(offer, content)` and the window collapses to the form.
//!
//! Run with: `cargo test -p stormscope-app --test ai_settings_sizing`

use egui::{Align2, Context, RawInput, Rect, Vec2};

/// Run a few frames so the area's one-frame sizing pass settles, then report
/// the window's outer rect.
fn measured_window_rect(auto_shrink: [bool; 2]) -> Rect {
    let ctx = Context::default();
    let mut rect = Rect::NOTHING;
    // Frame 1 is egui's sizing pass (area size unknown); frame 2+ report the
    // settled size. A few extra frames costs nothing and proves it stabilises.
    for _ in 0..4 {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(1600.0, 1000.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            let inner = egui::Window::new("AI meteorologist — settings")
                .id(egui::Id::new("stormscope.ai.settings"))
                .fixed_size(Vec2::new(620.0, 720.0))
                .pivot(Align2::CENTER_CENTER)
                .default_pos(ui.ctx().content_rect().center())
                .resizable(false)
                .show(ui, |ui| {
                    ui.set_width(580.0);
                    egui::ScrollArea::vertical()
                        .id_salt("stormscope.ai.settings.scroll")
                        .auto_shrink(auto_shrink)
                        .show(ui, |ui| {
                            // Roughly the real form: the desktop panel has no
                            // Gemini block, so it is far shorter than 720 pt.
                            for label in [
                                "Nothing is sent until you press Ask.",
                                "Provider",
                                "OpenCode Go  Ollama (local)  OpenAI",
                                "Base URL (OpenAI-compatible)",
                                "https://opencode.ai/inference/go/v1",
                                "Model",
                                "beam-free",
                                "API key",
                                "••••••••••••••••",
                                "Remember the key on disk (plain text, 0600)",
                                "Save   Clear key   Test connection",
                                "OpenCode Go: subscribe in the OpenCode Console.",
                            ] {
                                ui.label(label);
                            }
                        });
                });
            if let Some(inner) = inner {
                rect = inner.response.rect;
            }
        });
        output.drop_without_applying_deltas();
    }
    rect
}

/// The bug: with auto_shrink off vertically, the scroll area fills the offer
/// and the window stays pinned at the full 720 pt no matter how short the form.
#[test]
fn filling_scroll_area_pins_window_to_fixed_height() {
    let rect = measured_window_rect([false, false]);
    assert!(
        rect.height() > 700.0,
        "expected the blank-band bug (window ~720 pt tall), got {} pt",
        rect.height()
    );
}

/// The fix: auto_shrink on the y axis makes the window collapse to the form.
#[test]
fn shrinking_scroll_area_collapses_window_to_form() {
    let rect = measured_window_rect([false, true]);
    assert!(
        rect.height() < 500.0,
        "expected the window to hug the form (<500 pt), got {} pt",
        rect.height()
    );
    // Width must stay pinned at the fixed 620 pt regardless of the height fix.
    assert!(
        (rect.width() - 620.0).abs() < 2.0,
        "expected the width to stay 620 pt, got {} pt",
        rect.width()
    );
}

/// The pivot must keep the shrunken window centred on the screen, and must
/// re-centre when the content-driven height changes.
#[test]
fn pivot_keeps_window_centred_as_height_changes() {
    let ctx = Context::default();
    let screen = Vec2::new(1600.0, 1000.0);
    let mut rects = Vec::new();
    // Two different content heights, to prove re-centring on change.
    for extra in [0.0f32, 200.0] {
        let mut rect = Rect::NOTHING;
        for _ in 0..3 {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, screen)),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                let inner = egui::Window::new("AI meteorologist — settings")
                    .id(egui::Id::new("stormscope.ai.settings"))
                    .fixed_size(Vec2::new(620.0, 720.0))
                    .pivot(Align2::CENTER_CENTER)
                    .default_pos(ui.ctx().content_rect().center())
                    .resizable(false)
                    .show(ui, |ui| {
                        ui.set_width(580.0);
                        egui::ScrollArea::vertical()
                            .id_salt("stormscope.ai.settings.scroll")
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for i in 0..12 {
                                    ui.label(format!("row {i}"));
                                }
                                ui.add_space(extra);
                            });
                    });
                if let Some(inner) = inner {
                    rect = inner.response.rect;
                }
            });
            output.drop_without_applying_deltas();
        }
        rects.push(rect);
    }

    let screen_center = screen / 2.0;
    for rect in &rects {
        let delta = (rect.center().to_vec2() - screen_center).abs();
        assert!(
            delta.x < 2.0 && delta.y < 2.0,
            "window should stay centred, offset by {delta:?} (rect {rect:?})"
        );
    }
    // Heights differ, so this proves centring was recomputed rather than
    // pinned to the first frame's top-left corner.
    assert!(
        (rects[0].height() - rects[1].height()).abs() > 100.0,
        "test needs two meaningfully different heights, got {} vs {}",
        rects[0].height(),
        rects[1].height()
    );
}
