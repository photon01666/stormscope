//! The AI meteorologist: the Settings popup that configures the endpoint and
//! the report window that asks for a brief (or a question) about the markers.
//!
//! The snapshot, prompts and `MAP:` directives live in `crate::brief` and
//! `crate::ai_actions`; this module is presentation and request triggering.

use crate::app::StormApp;
use crate::state::AiRole;

/// Curated OpenCode Go models (the provider adds/removes models over time —
/// the field is editable, this is just a shortcut).
const GO_MODELS: &[&str] = &[
    "deepseek-v4.1-flash",
    "deepseek-v4-pro",
    "mimo-v2.6-flash",
    "glm-5.3-flash",
    "glm-5.3",
    "qwen3.8-flash",
    "qwen3.7-plus",
    "kimi-k3",
    "grok-4.7",
    "minimax-m3",
    "longcat-2.0",
];

pub fn ai_windows(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    // Gboard overlays the app instead of resizing the native window, so the
    // filled report window would otherwise put its question box underneath
    // the keyboard. GameActivity already receives the IME insets; poll them
    // (a few times a second) and lift the window's bottom edge.
    #[cfg(target_os = "android")]
    {
        let now = ctx.input(|i| i.time);
        if now - app.state.ime_polled_at > 0.12 {
            app.state.ime_polled_at = now;
            let px = crate::android_ime::ime_height_px();
            app.state.ime_inset_pt = px as f32 / ctx.pixels_per_point();
        }
    }
    #[cfg(target_os = "android")]
    let area = {
        let mut lifted = area;
        let inset = app.state.ime_inset_pt;
        if inset > 1.0 {
            // Never collapse past a couple of message rows.
            lifted.max.y = (lifted.max.y - inset).max(lifted.min.y + 160.0);
        }
        lifted
    };
    #[cfg(not(target_os = "android"))]
    let area = area;
    settings_window(ctx, app);
    report_window(ctx, app, area);
}

fn settings_window(ctx: &egui::Context, app: &mut StormApp) {
    if !app.state.ai_settings_open {
        return;
    }
    let screen = ctx.content_rect().size();
    let text_scale = super::big_text_scale(app);
    let size = egui::vec2(
        (screen.x * 0.32).clamp(440.0, 640.0),
        (screen.y * 0.46).clamp(320.0, 560.0),
    );
    let max_h = (screen.y * 0.8).max(300.0);
    let mut open = app.state.ai_settings_open;
    let mut save = false;
    let mut test = false;
    let mut preset: Option<(&str, &str)> = None;

    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut window = egui::Window::new("AI meteorologist — settings")
        .id(egui::Id::new("stormscope.ai.settings"))
        .default_size(size)
        .max_height(max_h)
        .min_size(egui::vec2(380.0, 300.0))
        .default_pos(super::centered_popup_pos(ctx, size));
    #[cfg(target_os = "android")]
    {
        window = window.title_bar(false);
    }
    let window = window.open(&mut open);
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut close_requested = false;
    window.show(ctx, |ui| {
        #[cfg(target_os = "android")]
        {
            close_requested |= super::phone_header(ui, "AI meteorologist — settings");
        }
        super::apply_text_scale(ui, text_scale);
        ui.set_width((size.x - 40.0).max(320.0));
        ui.label(
            egui::RichText::new(
                "The brief is optional. Nothing is sent until you press Write brief or Ask, \
                     and then only the compact snapshot built from the decoded radar, warnings, \
                     forecasts and your markers — no screenshot, no image.",
            )
            .weak(),
        );
        ui.separator();

        ui.label(egui::RichText::new("Provider").strong());
        ui.horizontal(|ui| {
            if ui
                .button("OpenCode Go")
                .on_hover_text(
                    "OpenAI-compatible endpoint used by OpenCode Go; \
                         get a key at opencode.ai/console",
                )
                .clicked()
            {
                preset = Some((
                    radar_data::ai::OPENCODE_GO_BASE,
                    radar_data::ai::OPENCODE_GO_MODEL,
                ));
            }
            if ui
                .button("Ollama (local)")
                .on_hover_text("http://localhost:11434/v1 — no key needed")
                .clicked()
            {
                preset = Some(("http://localhost:11434/v1", "llama3.1:8b"));
            }
            if ui
                .button("OpenAI")
                .on_hover_text("https://api.openai.com/v1")
                .clicked()
            {
                preset = Some(("https://api.openai.com/v1", "gpt-4o-mini"));
            }
        });
        if let Some((base, model)) = preset {
            app.state.ai_base_url = base.to_string();
            app.state.ai_model = model.to_string();
        }

        ui.add_space(4.0);
        ui.label("Base URL (OpenAI-compatible)");
        ui.add(
            egui::TextEdit::singleline(&mut app.state.ai_base_url)
                .desired_width(ui.available_width())
                .hint_text("https://opencode.ai/inference/go/v1"),
        );

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Model");
            ui.menu_button("OpenCode Go models", |ui| {
                for model in GO_MODELS {
                    if ui
                        .selectable_label(app.state.ai_model == *model, *model)
                        .clicked()
                    {
                        app.state.ai_model = (*model).to_string();
                        ui.close();
                    }
                }
            });
        });
        ui.add(
            egui::TextEdit::singleline(&mut app.state.ai_model)
                .desired_width(ui.available_width())
                .hint_text("deepseek-v4.1-flash"),
        );

        ui.add_space(6.0);
        ui.label("API key");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut app.state.ai_api_key)
                    .password(!app.state.ai_show_key)
                    .desired_width((ui.available_width() - 72.0).max(100.0))
                    .hint_text("Paste your OpenCode Go key"),
            );
            if ui
                .small_button(if app.state.ai_show_key {
                    "Hide"
                } else {
                    "Show"
                })
                .clicked()
            {
                app.state.ai_show_key = !app.state.ai_show_key;
            }
        });
        ui.checkbox(
            &mut app.state.ai_remember_key,
            "Remember the key on disk (plain text, 0600)",
        );
        ui.label(
            egui::RichText::new(
                "The OPENCODE_API_KEY environment variable is read at launch; a key set here \
                     overrides it. Keys are sent only to the base URL above.",
            )
            .weak()
            .small(),
        );

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .button("Save")
                .on_hover_text("Write endpoint and model to ~/.config/stormscope/ai.json")
                .clicked()
            {
                save = true;
            }
            if ui.button("Clear key").clicked() {
                app.state.ai_api_key.clear();
                save = true;
            }
            if ui
                .button("Test connection")
                .on_hover_text("One tiny request — verifies URL, key and model")
                .clicked()
            {
                save = true;
                test = true;
            }
        });
        if save {
            app.state.persist_ai();
        }
        if test {
            app.ai_test();
        }
        if app.state.ai_test_pending {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Testing…");
            });
        } else if let Some(status) = &app.state.ai_test_status {
            ui.label(egui::RichText::new(status).small());
        }
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "OpenCode Go: subscribe in the OpenCode Console, copy the API key, paste it \
                     above. The default model is DeepSeek V4.1 Flash — fast and inexpensive \
                     enough for a brief every scan.",
            )
            .weak()
            .small(),
        );
    });
    if close_requested {
        open = false;
    }
    app.state.ai_settings_open = open;
}

fn report_window(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    if !app.state.ai_open {
        return;
    }
    if app.state.ai_compact {
        compact_bar(ctx, app);
        return;
    }
    // On a phone there is no room for a floating window beside the map: fill
    // the app's map area edge to edge (status bar and timeline stay visible;
    // this is not the chrome-free fullscreen mode). Desktop keeps the
    // resizable floating window.
    let fill = cfg!(target_os = "android");
    let screen = ctx.content_rect().size();
    let text_scale = super::big_text_scale(app);
    // Leave real room below the window: opening it at ~62 % of the screen
    // height put the bottom edge against the constrain rect on a laptop
    // display, so the first downward drag snapped back. 42 % is a compact
    // starting size that can still be dragged much taller.
    let size = egui::vec2(
        (screen.x * 0.31).clamp(420.0, 640.0),
        (screen.y * 0.42).clamp(300.0, 520.0),
    );
    let mut window_size = if app.state.ai_window_size.x > 1.0 {
        app.state.ai_window_size
    } else {
        size
    };
    window_size.x = window_size.x.clamp(380.0, screen.x * 0.92);
    window_size.y = window_size.y.clamp(280.0, screen.y * 0.92);
    app.state.ai_window_size = window_size;
    let mut open = app.state.ai_open;
    let mut brief = false;
    let mut ask: Option<String> = None;
    let mut restore = false;
    let mut clear_chat = false;
    let mut clear_marks = false;
    let mut retry = false;
    let mut kbd = false;
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut close_requested = false;

    let window = egui::Window::new("AI meteorologist")
        .id(egui::Id::new("stormscope.ai"))
        .open(&mut open)
        // No inner frame margin: the custom resize grips then sit on the
        // actual window edge instead of a few pixels inside it.
        .frame(
            egui::Frame::window(&ctx.global_style()).inner_margin(if fill {
                // The filled window has no title-bar padding of its own; keep
                // the text off the frame edge.
                egui::Margin::same(10)
            } else {
                egui::Margin::ZERO
            }),
        );
    #[cfg(target_os = "android")]
    let window = if fill {
        window.title_bar(false)
    } else {
        window
    };
    let window = if fill {
        window
            .fixed_rect(area)
            .movable(false)
            .resizable(false)
            .constrain_to(area)
    } else {
        window
            .fixed_size(window_size)
            .default_pos(egui::pos2(18.0, 64.0))
    };
    window.show(ctx, |ui| {
        #[cfg(target_os = "android")]
        if fill {
            close_requested |= super::phone_header(ui, "AI meteorologist");
        }
        super::apply_text_scale(ui, text_scale);
        ui.set_min_width(400.0);

        // Header (always visible, even before the key is configured).
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{} · {}",
                    short_host(&app.state.ai_base_url),
                    app.state.ai_model
                ))
                .small()
                .weak(),
            );
            if ui.small_button("Settings").clicked() {
                app.state.ai_settings_open = true;
            }
            if ui
                .small_button("Minimize")
                .on_hover_text("Shrink this window so the map is fully visible")
                .clicked()
            {
                app.state.ai_compact = true;
            }
        });
        let configured = app.state.ai_config().is_configured();
        // No markers is not an error state: keep the full chat layout and
        // show the hint in the transcript. The unconfigured state keeps
        // the same layout too — composer disabled — instead of collapsing
        // the window to just the setup text (which looked like a broken
        // window on Windows, where no key was saved yet).
        // The GPS fix is a sample too: a brief works with no pins at all.
        let no_markers = app.state.markers.is_empty() && app.state.gps_fix.is_none();

        // Toolbar pinned to the top; transcript in the middle; input at the
        // bottom. Panels (not a hand-computed transcript height) make a
        // window drag native and smooth: the scroll area simply fills
        // whatever space is left.
        egui::Panel::top("stormscope.ai.toolbar")
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                ui.set_max_width((ui.available_width() - 14.0).max(120.0));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let write = ui.add_enabled(
                        configured && !no_markers && !app.state.ai_pending,
                        egui::Button::new("Write brief"),
                    );
                    if write
                        .on_hover_text(
                            "A short now / next-90-minutes report for your GPS location and \
                                 every marker",
                        )
                        .clicked()
                    {
                        brief = true;
                    }
                    if app.state.ai_pending {
                        ui.spinner();
                    }
                    if !app.state.ai_messages.is_empty() && ui.small_button("Clear chat").clicked()
                    {
                        clear_chat = true;
                    }
                    if !app.state.ai_annotations.is_empty()
                        && ui
                            .small_button("Clear marks")
                            .on_hover_text("Remove the AI's circles from the 2D and 3D maps")
                            .clicked()
                    {
                        clear_marks = true;
                    }
                    if let Some(snapshot) = &app.state.ai_snapshot {
                        ui.label(egui::RichText::new(snapshot).small().weak());
                    }
                });
                if let Some(actions) = &app.state.ai_actions {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("Map: {actions}"))
                                .small()
                                .weak(),
                        );
                        if ui
                            .small_button("Restore previous view")
                            .on_hover_text("Put the map and 3D pane back the way they were")
                            .clicked()
                        {
                            restore = true;
                        }
                        ui.label(egui::RichText::new("(behind this window)").small().weak());
                    });
                }
                ui.add_space(2.0);
            });

        egui::Panel::bottom("stormscope.ai.footer")
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                ui.set_max_width((ui.available_width() - 14.0).max(120.0));
                ui.separator();
                if !app.state.ai_pending
                    && app.state.ai_messages.last().is_some_and(|m| {
                        m.role == AiRole::Note && m.text.starts_with("Request failed")
                    })
                    && ui.button("Retry").clicked()
                {
                    retry = true;
                }
                ui.horizontal(|ui| {
                    let hint = if configured {
                        "Ask the meteorologist — e.g. \"is the hail core going to hit the \
                             north pin?\""
                    } else {
                        "Paste your API key in Settings to chat"
                    };
                    let response = ui.add_enabled(
                        configured,
                        egui::TextEdit::singleline(&mut app.state.ai_question)
                            .desired_width((ui.available_width() - 72.0).max(100.0))
                            .hint_text(hint),
                    );
                    kbd = response.has_focus();
                    let can_ask = configured && !no_markers && !app.state.ai_pending;
                    let submit = response.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        && can_ask;
                    if ui.add_enabled(can_ask, egui::Button::new("Ask")).clicked() || submit {
                        let question = app.state.ai_question.trim().to_string();
                        if !question.is_empty() {
                            ask = Some(question);
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Show me:").small().weak());
                    if ui
                        .small_button("the worst hazard")
                        .on_hover_text("Point the map at the most dangerous feature and explain it")
                        .clicked()
                        && configured
                        && !no_markers
                        && !app.state.ai_pending
                    {
                        ask = Some(
                            "Show me the worst hazard right now on the map and explain what I am \
                                 looking at."
                                .into(),
                        );
                    }
                    if ui
                        .small_button("the 3D structure")
                        .on_hover_text("Open the 3D pane on the most dangerous cell")
                        .clicked()
                        && configured
                        && !no_markers
                        && !app.state.ai_pending
                    {
                        ask = Some(
                            "Show me the 3D structure of the most dangerous cell, with a \
                                 cross-section if it helps, and explain it."
                                .into(),
                        );
                    }
                    if ui
                        .small_button("what happens next")
                        .on_hover_text("Focus the storm that arrives next and show its motion")
                        .clicked()
                        && configured
                        && !no_markers
                        && !app.state.ai_pending
                    {
                        ask = Some(
                            "Show me on the map what happens in the next 60 minutes at my \
                                 current location and markers."
                                .into(),
                        );
                    }
                });
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(format!(
                        "Sends decoded radar samples, coordinates, alerts and forecast text to {}. \
                             No screenshot is taken, no image is sent, no session history.",
                        short_host(&app.state.ai_base_url)
                    ))
                    .small()
                    .weak(),
                );
                // Leave the bottom edge to the resize grip: without this,
                // the privacy text sits inside its band.
                ui.add_space(16.0);
            });

        egui::CentralPanel::no_frame().show(ui, |ui| {
            let lane = (ui.available_width() - 14.0).max(80.0);
            ui.allocate_ui(egui::vec2(lane, ui.available_height()), |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("stormscope.ai.chat")
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    // Always reserve the scrollbar: when it appeared or
                    // vanished on the first pixel of a vertical resize it
                    // changed the content width and re-wrapped the chat —
                    // the "judder" at drag start.
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                    .show(ui, |ui| {
                        if !configured {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(
                                        "Paste your OpenCode Go API key in Settings → AI \
                                             meteorologist. The fields are already set to \
                                             OpenCode Go (deepseek-v4.1-flash), and the chat is \
                                             ready as soon as the key is in.",
                                    )
                                    .weak(),
                                )
                                .wrap(),
                            );
                            ui.add_space(6.0);
                            if ui.button("Open AI settings").clicked() {
                                app.state.ai_settings_open = true;
                            }
                        } else if app.state.ai_messages.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(if no_markers {
                                        "Waiting for a GPS fix. You can also place markers on \
                                             the map; the meteorologist writes about those \
                                             locations using the radar, warnings, storm cells and \
                                             forecast around them."
                                    } else {
                                        "Ask about your current location or markers — or write a \
                                             brief. Every exchange stays here; the model can move \
                                             the 2D/3D maps and mark what it means."
                                    })
                                    .weak(),
                                )
                                .wrap(),
                            );
                        }
                        let hidden = app.state.ai_messages.len().saturating_sub(20);
                        if hidden > 0 {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{hidden} older message{} hidden — Clear chat to reset",
                                    if hidden == 1 { "" } else { "s" }
                                ))
                                .small()
                                .weak(),
                            );
                        }
                        for message in &app.state.ai_messages[hidden..] {
                            match message.role {
                                AiRole::User => {
                                    ui.label(
                                        egui::RichText::new("You")
                                            .small()
                                            .strong()
                                            .color(egui::Color32::from_gray(160)),
                                    );
                                    ui.add(
                                        egui::Label::new(egui::RichText::new(message.text.trim()))
                                            .selectable(true)
                                            .wrap(),
                                    );
                                }
                                AiRole::Assistant => {
                                    ui.label(
                                        egui::RichText::new("Meteorologist")
                                            .small()
                                            .strong()
                                            .color(egui::Color32::from_rgb(120, 200, 255)),
                                    );
                                    ui.add(
                                        egui::Label::new(egui::RichText::new(clean_report(
                                            &message.text,
                                        )))
                                        .selectable(true)
                                        .wrap(),
                                    );
                                    if ui.small_button("Copy").clicked() {
                                        ui.ctx().copy_text(clean_report(&message.text));
                                    }
                                }
                                AiRole::Note => {
                                    let color = if message.text.starts_with("Request failed") {
                                        egui::Color32::from_rgb(240, 120, 110)
                                    } else {
                                        egui::Color32::from_gray(160)
                                    };
                                    ui.colored_label(color, &message.text);
                                }
                            }
                            ui.add_space(7.0);
                        }
                        if app.state.ai_pending {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                if let Some(looking) = &app.state.ai_looking_at {
                                    ui.label(
                                        egui::RichText::new(format!("Looking at {looking}…"))
                                            .weak(),
                                    );
                                } else {
                                    ui.label(
                                        egui::RichText::new(
                                            "Sampling the radar, warnings and forecast…",
                                        )
                                        .weak(),
                                    );
                                }
                            });
                        }
                    });
            });
        });

        if !fill {
            resize_grips(ui, app);
        }
    });
    if close_requested {
        open = false;
    }
    app.state.ai_open = open;
    // Keep the soft keyboard up while the composer holds focus, and mirror
    // its text for the Android IME bridge.
    app.state.text_focus |= kbd;
    if kbd {
        app.state.text_focus_text = app.state.ai_question.clone();
    }
    if brief {
        app.ai_request(None);
    }
    if let Some(question) = ask {
        app.state.ai_question.clear();
        app.ai_request(Some(question));
    }
    if retry {
        app.ai_retry();
    }
    if restore && crate::ai_actions::restore(&mut app.state) {
        app.state.ai_actions = None;
    }
    if clear_marks && !app.state.ai_annotations.is_empty() {
        app.state.ai_annotations.clear();
        app.state.ai_actions = None;
    }
    if clear_chat {
        app.state.ai_messages.clear();
        app.state.ai_report = None;
        app.state.ai_last_question = None;
        app.state.ai_error = None;
        app.state.ai_actions = None;
        app.state.ai_annotations.clear();
        app.state.ai_view_before = None;
        app.state.ai_snapshot = None;
        app.state.persist_ai_chat();
    }
}

/// Bottom / right / corner resize grips. The window is `fixed_size`, so this
/// is entirely ours: a relative delta from the size that was on screen when
/// the drag began. No title-bar subtraction, no first-frame jump.
fn resize_grips(ui: &mut egui::Ui, app: &mut StormApp) {
    let rect = ui.max_rect();
    let edge = 14.0;
    let bottom = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 8.0, rect.bottom() - edge),
        egui::pos2(rect.right() - edge, rect.bottom()),
    );
    let right = egui::Rect::from_min_max(
        egui::pos2(rect.right() - edge, rect.top() + 8.0),
        egui::pos2(rect.right(), rect.bottom() - edge),
    );
    let corner = egui::Rect::from_min_max(
        egui::pos2(rect.right() - edge, rect.bottom() - edge),
        rect.max,
    );

    let b = ui.interact(
        bottom,
        egui::Id::new("ai.resize.bottom"),
        egui::Sense::drag(),
    );
    let r = ui.interact(right, egui::Id::new("ai.resize.right"), egui::Sense::drag());
    let c = ui.interact(
        corner,
        egui::Id::new("ai.resize.corner"),
        egui::Sense::drag(),
    );

    let pointer = ui.ctx().pointer_interact_pos();
    if c.drag_started() {
        app.state.ai_resize_drag = Some((3, pointer.unwrap_or_default(), app.state.ai_window_size));
    } else if b.drag_started() {
        app.state.ai_resize_drag = Some((1, pointer.unwrap_or_default(), app.state.ai_window_size));
    } else if r.drag_started() {
        app.state.ai_resize_drag = Some((2, pointer.unwrap_or_default(), app.state.ai_window_size));
    }

    if b.hovered() || b.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }
    if r.hovered() || r.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    if c.hovered() || c.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
    }

    if let Some((mask, start_pos, start_size)) = app.state.ai_resize_drag {
        if ui.input(|i| i.pointer.any_down()) {
            if let Some(pos) = pointer {
                let delta = pos - start_pos;
                let mut size = start_size;
                if mask & 1 != 0 {
                    size.y += delta.y;
                }
                if mask & 2 != 0 {
                    size.x += delta.x;
                }
                let screen = ui.ctx().content_rect().size();
                size.x = size.x.clamp(380.0, screen.x * 0.92);
                size.y = size.y.clamp(280.0, screen.y * 0.92);
                app.state.ai_window_size = size;
            }
        } else {
            app.state.ai_resize_drag = None;
        }
    }

    // A subtle grip so the corner is discoverable.
    let color = egui::Color32::from_gray(110);
    for i in 0..3 {
        let off = 3.0 + i as f32 * 3.5;
        ui.painter().line_segment(
            [
                egui::pos2(rect.right() - off - 3.0, rect.bottom() - 2.0),
                egui::pos2(rect.right() - 2.0, rect.bottom() - off - 3.0),
            ],
            egui::Stroke::new(1.0, color),
        );
    }
}

/// The minimised report: one small bar that keeps the map (which the model may
/// have just moved) visible, with the report one click away.
fn compact_bar(ctx: &egui::Context, app: &mut StormApp) {
    let mut open = app.state.ai_open;
    egui::Window::new("AI meteorologist")
        .id(egui::Id::new("stormscope.ai.compact"))
        .open(&mut open)
        .resizable(false)
        .title_bar(false)
        .default_pos(egui::pos2(18.0, ctx.content_rect().height() - 106.0))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                if app.state.ai_pending {
                    ui.spinner();
                }
                let status = if app.state.ai_pending {
                    "Meteorologist is looking…".to_string()
                } else if app.state.ai_report.is_some() {
                    "Brief ready".to_string()
                } else {
                    "Meteorologist".to_string()
                };
                ui.label(egui::RichText::new(status).strong());
                if ui.small_button("Expand").clicked() {
                    app.state.ai_compact = false;
                }
                if app.state.ai_actions.is_some() && ui.small_button("Restore view").clicked() {
                    if crate::ai_actions::restore(&mut app.state) {
                        app.state.ai_actions = None;
                    }
                }
                if ui.small_button("Hide").clicked() {
                    app.state.ai_open = false;
                }
            });
        });
    if !open {
        app.state.ai_open = false;
    }
}

/// Host (and path when it is not the default `/v1`) for the small header line.
fn short_host(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    without_scheme.to_string()
}

/// The model's prose is plain text; strip the few markdown ornaments it may
/// still use rather than showing literal asterisks.
fn clean_report(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let mut line = line.trim_end().to_string();
        line = line.replace("**", "").replace("__", "");
        let trimmed = line.trim_start();
        if let Some(heading) = trimmed.strip_prefix('#') {
            line = heading.trim_start_matches('#').trim_start().to_string();
        }
        if line.trim_start().starts_with("```") {
            continue;
        }
        out.push_str(&line);
        out.push('\n');
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_cleaning_removes_markdown_ornaments() {
        let text = "**Home**\n### Now\nRain, 45 dBZ.\n```\ncode\n```";
        let clean = clean_report(text);
        assert!(!clean.contains("**"));
        assert!(!clean.contains("###"));
        assert!(!clean.contains("```"));
        assert!(clean.contains("Home"));
        assert!(clean.contains("Rain, 45 dBZ."));
    }

    #[test]
    fn host_shortening_is_readable() {
        assert_eq!(
            short_host("https://opencode.ai/inference/go/v1"),
            "opencode.ai/inference/go/v1"
        );
        assert_eq!(
            short_host("http://localhost:11434/v1/"),
            "localhost:11434/v1"
        );
    }
}
