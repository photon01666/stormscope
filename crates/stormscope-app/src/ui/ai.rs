//! The AI meteorologist: the Settings popup that configures the endpoint and
//! the report window that asks for a brief (or a question) about the markers.
//!
//! The snapshot, prompts and `MAP:` directives live in `crate::brief` and
//! `crate::ai_actions`; this module is presentation and request triggering.

use crate::app::StormApp;
use crate::live_ai::{LiveEntry, ThreatLevel};
use crate::state::{AiRole, AiTab};

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

/// Current Claude models for the Anthropic Messages API. The model field is
/// editable, so a newly released model can be typed in immediately.
const CLAUDE_MODELS: &[&str] = &[
    "claude-sonnet-5-5",
    "claude-haiku-5-5",
    "claude-sonnet-4-6",
    "claude-opus-4-6",
    "claude-haiku-4-5-20251001",
];

pub fn ai_windows(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    // Live tracking has to keep pumping while the user is reading, not only
    // when they move the pointer: a new scan may arrive while the window sits
    // still. One frame a second is plenty for status/feed updates.
    if app.state.live_ai_enabled {
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
    }
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
    let raw_area = area;
    #[cfg(target_os = "android")]
    let area = {
        let mut lifted = raw_area;
        let inset = app.state.ime_inset_pt;
        if inset > 1.0 {
            // Never collapse past a couple of message rows.
            lifted.max.y = (lifted.max.y - inset).max(lifted.min.y + 160.0);
        }
        lifted
    };
    #[cfg(not(target_os = "android"))]
    let (raw_area, area) = (area, area);
    // Settings is a full-screen form with its own scroll area. Keeping it on
    // the raw map area means the IME opening does not resize/relayout the
    // window and bounce focus (which made the keyboard flash on and off).
    settings_window(ctx, app, raw_area);
    report_window(ctx, app, area);
    consent_window(ctx, app);
}

fn settings_window(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    if !app.state.ai_settings_open {
        return;
    }
    let text_scale = super::big_text_scale(app);
    let fill = cfg!(target_os = "android");
    // Desktop pins the width and caps the height at 620×720 but hugs
    // whatever the form actually needs — a hard-fixed panel left a huge
    // blank band under the footer once the Gemini block became Android-only.
    // The phone fills the map area edge to edge and cannot be dragged or
    // resized.
    let size = super::popup_size(ctx, text_scale, 620.0, 720.0);
    let mut open = app.state.ai_settings_open;
    let mut save = false;
    let mut test = false;
    let mut preset: Option<(&str, &str)> = None;

    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut window = egui::Window::new("AI meteorologist — settings")
        .id(egui::Id::new("stormscope.ai.settings"));
    #[cfg(target_os = "android")]
    {
        window = window.title_bar(false);
    }
    let window = if fill {
        let inset = area.shrink2(egui::vec2(4.0, 8.0));
        window
            .fixed_rect(inset)
            .movable(false)
            .resizable(false)
            .constrain_to(inset)
    } else {
        // Pin the width, cap the height at 620×720, but let the panel hug
        // the form. Pivot on the centre so the window stays centred as its
        // content-driven height changes — and, unlike `anchor`, it stays
        // draggable.
        window
            .fixed_size(size)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .resizable(false)
    };
    let window = window.open(&mut open);
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut close_requested = false;
    window.show(ctx, |ui| {
        #[cfg(target_os = "android")]
        {
            close_requested |= super::phone_header(ui, "AI meteorologist — settings");
        }
        super::apply_text_scale(ui, text_scale);
        let width = if fill {
            ui.available_width()
        } else {
            (size.x - 40.0).max(320.0)
        };
        ui.set_width(width.max(320.0));
        // Pad the scroll content (not the viewport) by the keyboard height:
        // the window and focused widget do not resize, so focus stays put,
        // but the form has room to scroll clear of the keyboard. When the
        // keyboard is closed the pad is zero and the page keeps its space.
        let ime_pad = if fill {
            app.state.ime_inset_pt.max(0.0)
        } else {
            0.0
        };
        egui::ScrollArea::vertical()
            .id_salt("stormscope.ai.settings.scroll")
            // Desktop: shrink to the form's height so the window hugs its
            // content instead of filling the fixed 620×720 box with a blank
            // band (the Gemini block is Android-only now). The phone keeps
            // filling its fixed panel edge to edge.
            .auto_shrink([false, !fill])
            .show(ui, |ui| {
                if fill {
                    // The phone form is tall but the map area is taller; a
                    // roomier rhythm fills the panel instead of leaving a
                    // big blank band at the bottom.
                    let y = ui.spacing().item_spacing.y;
                    ui.spacing_mut().item_spacing.y = (y * 2.2).max(10.0);
                }
                ui.label(
            egui::RichText::new(
                "Nothing is sent unless you press Ask, or unless Live AI Tracking is enabled. \
                     When it is on, one compact snapshot (radar, warnings, forecast and your \
                     marker/GPS point) is sent per new volume scan — no screenshot, no image.",
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
                    if ui
                        .button("Claude (Anthropic)")
                        .on_hover_text("Anthropic Messages API; get a key at platform.claude.com")
                        .clicked()
                    {
                        preset = Some((
                            radar_data::ai::ANTHROPIC_BASE,
                            radar_data::ai::ANTHROPIC_MODEL,
                        ));
                    }
                });
                if let Some((base, model)) = preset {
                    app.state.ai_base_url = base.to_string();
                    app.state.ai_model = model.to_string();
                }

                ui.add_space(4.0);
                ui.label("Base URL (OpenAI-compatible or Anthropic)");
                ui.add(
                    egui::TextEdit::singleline(&mut app.state.ai_base_url)
                        .desired_width(ui.available_width())
                        .hint_text("https://opencode.ai/inference/go/v1"),
                );

                let anthropic = app
                    .state
                    .ai_base_url
                    .to_ascii_lowercase()
                    .contains("anthropic.com");
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Model");
                    ui.menu_button(
                        if anthropic {
                            "Claude models"
                        } else {
                            "OpenCode Go models"
                        },
                        |ui| {
                            let models = if anthropic { CLAUDE_MODELS } else { GO_MODELS };
                            for model in models {
                                if ui
                                    .selectable_label(app.state.ai_model == *model, *model)
                                    .clicked()
                                {
                                    app.state.ai_model = (*model).to_string();
                                    ui.close();
                                }
                            }
                        },
                    );
                });
                ui.add(
                    egui::TextEdit::singleline(&mut app.state.ai_model)
                        .desired_width(ui.available_width())
                        .hint_text(if anthropic {
                            "claude-sonnet-5-5"
                        } else {
                            "deepseek-v4.1-flash"
                        }),
                );

                ui.add_space(6.0);
                ui.label("API key");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut app.state.ai_api_key)
                            .password(!app.state.ai_show_key)
                            .desired_width((ui.available_width() - 72.0).max(100.0))
                            .hint_text(if anthropic {
                                "Paste your Anthropic API key"
                            } else {
                                "Paste your OpenCode Go key"
                            }),
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
                    egui::RichText::new(if anthropic {
                        "The ANTHROPIC_API_KEY environment variable is read at launch when the \
                         Anthropic endpoint is selected; a key set here overrides it. Keys are \
                         sent only to the base URL above."
                    } else {
                        "The OPENCODE_API_KEY environment variable is read at launch; a key set \
                         here overrides it. Keys are sent only to the base URL above."
                    })
                    .weak()
                    .small(),
                );

                #[cfg(target_os = "android")]
                {
                    ui.add_space(10.0);
                    ui.separator();
                    ui.label(
                        egui::RichText::new("Gemini Live — natural voice conversation").strong(),
                    );
                    ui.label(
                        egui::RichText::new(
                            "Android only. Google AI Studio's Live API holds a native \
                     speech-to-speech conversation and receives the same weather snapshot \
                     as a typed question, as text. Voice audio is streamed to Google; the \
                     microphone pauses while Gemini speaks so it does not hear itself. \
                     Talk tests the voice key directly. Get a key at \
                     aistudio.google.com/apikey.",
                        )
                        .weak()
                        .small(),
                    );
                    ui.add_space(4.0);
                    ui.label("Gemini API key");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut app.state.gemini_api_key)
                                .password(!app.state.gemini_show_key)
                                .desired_width((ui.available_width() - 72.0).max(100.0))
                                .hint_text("Paste your AI Studio key"),
                        );
                        if ui
                            .small_button(if app.state.gemini_show_key {
                                "Hide"
                            } else {
                                "Show"
                            })
                            .clicked()
                        {
                            app.state.gemini_show_key = !app.state.gemini_show_key;
                        }
                    });
                    ui.checkbox(
                        &mut app.state.gemini_remember_key,
                        "Remember the Gemini key on disk (plain text, 0600)",
                    );
                    ui.add_space(4.0);
                    ui.label("Voice model");
                    ui.add(
                        egui::TextEdit::singleline(&mut app.state.gemini_model)
                            .desired_width(ui.available_width())
                            .hint_text("gemini-3.8-live-extended-thinking"),
                    );
                    ui.horizontal(|ui| {
                        ui.label("Voice");
                        egui::ComboBox::from_id_salt("stormscope.gemini.voice")
                            .selected_text(&app.state.gemini_voice)
                            .show_ui(ui, |ui| {
                                for voice in [
                                    "Aoede", "Kore", "Leda", "Zephyr", "Puck", "Charon", "Fenrir",
                                    "Orus",
                                ] {
                                    ui.selectable_value(
                                        &mut app.state.gemini_voice,
                                        voice.to_string(),
                                        voice,
                                    );
                                }
                            });
                    });
                    if ui.small_button("Clear Gemini key").clicked() {
                        app.state.gemini_api_key.clear();
                    }
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .button("Save")
                        .on_hover_text(
                            "Write endpoint, models and keys to ~/.config/stormscope/ai.json",
                        )
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
                    egui::RichText::new(if anthropic {
                        "Claude: create an API key at platform.claude.com, paste it above, and \
                         pick a model. Sonnet 5.5 is a good balance of speed and analysis quality \
                         for a brief every scan; Haiku 5.5 is the cheaper, faster option."
                    } else {
                        "OpenCode Go: subscribe in the OpenCode Console, copy the API key, paste \
                         it above. The default model is DeepSeek V4.1 Flash — fast and inexpensive \
                         enough for a brief every scan."
                    })
                    .weak()
                    .small(),
                );
                ui.add_space(16.0 + ime_pad);
            });
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
        // Android draws the minimised AI as a floating circle above the GPS
        // chip (see `ui::ai_min_circle`), so it stays anchored to the map
        // controls instead of floating over the timeline.
        #[cfg(not(target_os = "android"))]
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
    #[cfg_attr(target_os = "android", allow(unused_mut))]
    let mut ask: Option<String> = None;
    let mut clear_chat = false;
    let mut clear_marks = false;
    let mut retry = false;
    let mut kbd = false;
    let mut live_toggle = app.state.live_ai_enabled;
    let mut live_speak = app.state.live_ai_speak;
    let mut live_clear = false;
    let mut live_settings = false;
    #[cfg(target_os = "android")]
    let mut voice_start = false;
    #[cfg(target_os = "android")]
    let mut voice_end = false;
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut close_requested = false;
    let mut live_tab = app.state.ai_tab == AiTab::Live;

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
        // A few points of inset: without it the top of the phone header sat
        // flush against the pane edge and read as "cut off".
        let inset = area.shrink2(egui::vec2(4.0, 8.0));
        window
            .fixed_rect(inset)
            .movable(false)
            .resizable(false)
            .constrain_to(inset)
    } else {
        window
            .fixed_size(window_size)
            .default_pos(egui::pos2(18.0, 64.0))
    };
    window.show(ctx, |ui| {
        #[cfg(target_os = "android")]
        if fill {
            let (close, minimize) =
                super::ai_phone_header(ui, "AI meteorologist", &app.state.ai_model.clone());
            close_requested |= close;
            if minimize {
                app.state.ai_compact = true;
            }
        }
        super::apply_text_scale(ui, text_scale);
        ui.set_min_width(400.0);

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
                    if ui
                        .selectable_label(app.state.ai_tab == AiTab::Chat, "Chat")
                        .clicked()
                    {
                        app.state.ai_tab = AiTab::Chat;
                        live_tab = false;
                    }
                    if ui
                        .selectable_label(app.state.ai_tab == AiTab::Live, "Live")
                        .clicked()
                    {
                        app.state.ai_tab = AiTab::Live;
                        live_tab = true;
                    }
                    if app.state.live_ai_enabled {
                        ui.colored_label(egui::Color32::from_rgb(110, 220, 150), "● live")
                            .on_hover_text("Live AI Tracking is watching for new volume scans");
                    }
                    if app.state.ai_pending {
                        ui.spinner();
                    }
                    if app.state.ai_tab == AiTab::Chat
                        && !app.state.ai_messages.is_empty()
                        && ui.small_button("Clear chat").clicked()
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
                    if app.state.ai_tab == AiTab::Chat
                        && let Some(snapshot) = &app.state.ai_snapshot
                    {
                        ui.label(egui::RichText::new(snapshot).small().weak());
                    }
                });
                if app.state.ai_tab == AiTab::Chat
                    && let Some(actions) = &app.state.ai_actions
                {
                    ui.label(
                        egui::RichText::new(format!("Map: {actions}"))
                            .small()
                            .weak(),
                    );
                }
                ui.add_space(2.0);
            });

        egui::Panel::bottom("stormscope.ai.footer")
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                ui.set_max_width((ui.available_width() - 14.0).max(120.0));
                ui.separator();
                if live_tab {
                    ui.horizontal(|ui| {
                        if !configured {
                            if ui.button("Open AI settings").clicked() {
                                live_settings = true;
                            }
                        } else if !app.state.live_ai_enabled {
                            if ui.button("Enable live tracking").clicked() {
                                live_toggle = true;
                            }
                        } else if ui.button("Stop").clicked() {
                            live_toggle = false;
                        }
                        if ui
                            .button("Ask a question")
                            .on_hover_text("Switch to Chat and ask about the latest scan")
                            .clicked()
                        {
                            app.state.ai_tab = AiTab::Chat;
                            live_tab = false;
                        }
                        if !app.live_ai.feed.is_empty() && ui.button("Clear live feed").clicked() {
                            live_clear = true;
                        }
                    });
                } else {
                    if !app.state.ai_pending
                        && app.state.ai_messages.last().is_some_and(|m| {
                            m.role == AiRole::Note && m.text.starts_with("Request failed")
                        })
                        && ui.button("Retry").clicked()
                    {
                        retry = true;
                    }
                    // On a phone the spoken conversation gets the footer to
                    // itself while it runs; the composer (and Talk) return when
                    // the user taps End.
                    #[cfg(target_os = "android")]
                    let voice_active = app.state.voice_convo;
                    #[cfg(not(target_os = "android"))]
                    let voice_active = false;
                    if voice_active {
                        #[cfg(target_os = "android")]
                        voice_footer(ui, app, &mut voice_end);
                    } else {
                        ui.horizontal(|ui| {
                            let hint = if configured {
                                "Ask the meteorologist — e.g. \"is the hail core going to hit the \
                                     north pin?\""
                            } else {
                                "Paste your API key in Settings to chat"
                            };
                            #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
                            let mut editor = egui::TextEdit::singleline(&mut app.state.ai_question)
                                .desired_width((ui.available_width() - 72.0).max(100.0))
                                .hint_text(hint);
                            // The default composer is a finger-width strip on a
                            // phone; make the field and the buttons touch-sized.
                            #[cfg(target_os = "android")]
                            {
                                // Two rows at an 18 pt font: roughly double the
                                // default strip's height, comfortable to type into.
                                editor = editor
                                    .font(egui::FontId::new(18.0, egui::FontFamily::Proportional))
                                    .desired_rows(2)
                                    .desired_width((ui.available_width() - 180.0).max(80.0));
                            }
                            let response = ui.add_enabled(configured, editor);
                            kbd = response.has_focus();
                            let can_ask = configured && !no_markers && !app.state.ai_pending;
                            let submit = response.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                                && can_ask;
                            #[cfg(not(target_os = "android"))]
                            let ask_resp = ui.add_enabled(can_ask, egui::Button::new("Ask"));
                            #[cfg(target_os = "android")]
                            let ask_resp = ui.add_enabled(
                                can_ask,
                                egui::Button::new("Ask").min_size(egui::vec2(72.0, 44.0)),
                            );
                            if ask_resp.clicked() || submit {
                                let question = app.state.ai_question.trim().to_string();
                                if !question.is_empty() {
                                    ask = Some(question);
                                }
                            }
                            // Hands-free conversation: the answer is spoken and
                            // the microphone reopens after each reply.
                            #[cfg(target_os = "android")]
                            {
                                let talk = ui.add_enabled(
                                    !no_markers,
                                    egui::Button::new("Talk").min_size(egui::vec2(64.0, 44.0)),
                                );
                                if talk
                                    .on_hover_text(
                                        "Talk with the meteorologist using Gemini Live — a natural \
                                         spoken conversation with the full weather snapshot",
                                    )
                                    .clicked()
                                {
                                    voice_start = true;
                                    response.surrender_focus();
                                }
                            }
                        });
                    }
                }
                // Leave the bottom edge to the resize grip.
                ui.add_space(16.0);
            });

        if live_tab {
            live_center(
                ui,
                app,
                &mut live_toggle,
                &mut live_speak,
                &mut live_settings,
            );
        } else {
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
                        .scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                        )
                        .show(ui, |ui| {
                            if !configured {
                                let anthropic = app
                                    .state
                                    .ai_base_url
                                    .to_ascii_lowercase()
                                    .contains("anthropic.com");
                                let (provider, model) = if anthropic {
                                    ("Claude", "claude-sonnet-5-5")
                                } else {
                                    ("OpenCode Go", "deepseek-v4.1-flash")
                                };
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(format!(
                                            "Paste your {provider} API key in Settings → AI \
                                         meteorologist. The fields are already set to \
                                         {provider} ({model}), and the chat is ready as soon \
                                         as the key is in."
                                        ))
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
                                #[cfg(target_os = "android")]
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(
                                            "Tap Talk for a live voice conversation with the \
                                         meteorologist — ask naturally and hear the answer.",
                                        )
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
                                            egui::Label::new(egui::RichText::new(
                                                message.text.trim(),
                                            ))
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
        }

        if !fill {
            resize_grips(ui, app);
        }
    });
    if close_requested {
        open = false;
    }
    app.state.ai_open = open;
    if live_toggle != app.state.live_ai_enabled {
        app.set_live_ai_enabled(live_toggle);
    }
    if live_speak != app.state.live_ai_speak {
        app.state.live_ai_speak = live_speak;
        app.state.persist_ai();
    }
    if live_clear {
        app.live_ai.clear_feed();
    }
    if live_settings {
        app.state.ai_settings_open = true;
    }
    if app.state.ai_tab == AiTab::Live {
        app.live_ai.unread = 0;
    }
    #[cfg(target_os = "android")]
    if voice_start {
        app.start_voice_convo();
    }
    #[cfg(target_os = "android")]
    if voice_end {
        app.end_voice_convo();
    }
    // Keep the soft keyboard up while the composer holds focus, and mirror
    // its text for the Android IME bridge.
    app.state.text_focus |= kbd;
    if kbd {
        app.state.text_focus_text = app.state.ai_question.clone();
    }
    if let Some(question) = ask {
        app.state.ai_question.clear();
        app.ai_request(Some(question));
    }
    if retry {
        app.ai_retry();
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

/// The Live tab: control strip, deterministic "now" line, and remembered
/// scan-by-scan updates. It deliberately has no composer; Chat is one tab
/// away and the live prompt must never receive a user question.
fn live_center(
    ui: &mut egui::Ui,
    app: &mut StormApp,
    live_toggle: &mut bool,
    live_speak: &mut bool,
    live_settings: &mut bool,
) {
    let configured = app.state.ai_config().is_configured();
    ui.horizontal(|ui| {
        ui.checkbox(live_toggle, "Live AI tracking").on_hover_text(
            "Analyze each new full Level II volume scan automatically. The selected radar \
                 site and its existing marker/GPS point are used; the station itself is the \
                 fallback when neither is inside its coverage.",
        );
        if *live_toggle {
            ui.checkbox(live_speak, "Speak threats").on_hover_text(
                "Speak only when a storm newly crosses the 30- or 10-minute arrival threshold",
            );
        }
    });
    let status = app.live_ai.status_text(configured, *live_toggle);
    ui.horizontal(|ui| {
        if *live_toggle {
            let color = if configured {
                egui::Color32::from_rgb(110, 220, 150)
            } else {
                egui::Color32::from_rgb(255, 205, 80)
            };
            ui.colored_label(color, "●");
        }
        ui.label(egui::RichText::new(status).small().weak());
        if *live_toggle && app.live_ai.scan_number > 0 {
            let remembered = app
                .live_ai
                .memory
                .len()
                .min(crate::live_ai::MEMORY_IN_PROMPT);
            ui.label(
                egui::RichText::new(format!("remembering {remembered} scans"))
                    .small()
                    .weak(),
            );
        }
    });
    if !configured {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Live tracking needs the same AI endpoint and key as Chat.")
                    .weak(),
            );
            if ui.small_button("Open AI settings").clicked() {
                *live_settings = true;
            }
        });
    }
    if let Some(assessment) = app.live_ai.latest_assessment() {
        ui.add_space(2.0);
        let color = threat_color(assessment.threat);
        ui.horizontal(|ui| {
            ui.colored_label(color, assessment.threat.label());
            ui.label(egui::RichText::new(&assessment.headline).strong());
        });
        ui.label(
            egui::RichText::new(format!(
                "{} · {}",
                assessment.location.feed_label(),
                assessment.location.source.label().replace('_', " ")
            ))
            .small()
            .weak(),
        );
    }
    ui.separator();
    let entries: Vec<LiveEntry> = app.live_ai.feed.iter().rev().take(20).cloned().collect();
    if entries.is_empty() {
        if !*live_toggle {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(
                        "Live AI tracking watches each new volume scan, remembers the previous \
                         scans, and tells you what changed and whether a storm core is heading \
                         toward your point. It only runs while the app is open.",
                    )
                    .weak(),
                )
                .wrap(),
            );
        } else {
            ui.horizontal(|ui| {
                if app.live_ai.in_flight.is_some() {
                    ui.spinner();
                }
                ui.label(
                    egui::RichText::new(if app.live_ai.in_flight.is_some() {
                        "Sampling the volume and preparing the first live update…"
                    } else {
                        "Waiting for the next volume scan…"
                    })
                    .weak(),
                );
            });
        }
    } else {
        for entry in &entries {
            live_entry_card(ui, entry);
        }
    }
    if *live_toggle {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));
    }
}

fn live_entry_card(ui: &mut egui::Ui, entry: &LiveEntry) {
    let color = if entry.error {
        egui::Color32::from_rgb(240, 120, 110)
    } else {
        threat_color(entry.threat)
    };
    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{} · {}",
                    crate::live_ai::scan_clock(&entry.scan),
                    entry.site
                ))
                .small()
                .weak(),
            );
            ui.colored_label(
                color,
                if entry.error {
                    "Error"
                } else {
                    entry.threat.label()
                },
            );
            if let Some(minutes) = entry.eta_min {
                ui.label(
                    egui::RichText::new(format!("ETA {}", fmt_eta(minutes)))
                        .small()
                        .strong(),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!("{} ms", entry.latency_ms))
                        .small()
                        .weak(),
                );
            });
        });
        ui.label(egui::RichText::new(&entry.headline).strong());
        ui.label(egui::RichText::new(&entry.location).small().weak());
        ui.add(
            egui::Label::new(egui::RichText::new(clean_report(&entry.body)))
                .selectable(true)
                .wrap(),
        );
    });
    ui.add_space(6.0);
}

fn threat_color(threat: ThreatLevel) -> egui::Color32 {
    let [r, g, b] = threat.rgb();
    egui::Color32::from_rgb(r, g, b)
}

fn fmt_eta(minutes: f64) -> String {
    let minutes = minutes.round().max(0.0) as i64;
    if minutes < 90 {
        format!("{minutes} min")
    } else {
        format!("{}h {:02}m", minutes / 60, minutes % 60)
    }
}

fn consent_window(ctx: &egui::Context, app: &mut StormApp) {
    if !app.state.live_ai_consent_open {
        return;
    }
    let mut open = true;
    let mut enable = false;
    let mut cancel = false;
    let width = (ctx.content_rect().width() - 32.0).clamp(300.0, 520.0);
    egui::Window::new("Live AI tracking")
        .id(egui::Id::new("stormscope.ai.live.consent"))
        .collapsible(false)
        .resizable(false)
        .default_width(width)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .open(&mut open)
        .show(ctx, |ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(
                        "Live AI Tracking sends a compact radar snapshot — including your marker or \
                         GPS point when one is inside the selected radar's coverage — to the AI \
                         provider on each new volume scan.",
                    ),
                )
                .wrap(),
            );
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(
                        "About 12 requests per hour while enabled. It runs only while the app is \
                         open. The station itself is used as the fallback when no marker or GPS lies \
                         inside the selected radar's coverage.",
                    )
                    .weak(),
                )
                .wrap(),
            );
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!(
                    "Provider: {} · model: {}",
                    app.state.ai_base_url, app.state.ai_model
                ))
                .small()
                .weak(),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Enable live tracking").clicked() {
                    enable = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
    if enable {
        app.confirm_live_ai();
    } else if cancel || !open {
        app.state.live_ai_consent_open = false;
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

/// Footer while a Gemini Live conversation is running: a level meter, the
/// current phase, the live transcript and one End button.
#[cfg(target_os = "android")]
fn voice_footer(ui: &mut egui::Ui, app: &StormApp, end: &mut bool) {
    let status = app
        .state
        .voice_status
        .as_deref()
        .unwrap_or("Gemini Live — talk naturally");
    let active = app.state.voice_listening || app.state.voice_speaking;
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        voice_meter(ui, app.state.voice_level, active);
        ui.label(egui::RichText::new(status).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(egui::Button::new("End").min_size(egui::vec2(72.0, 40.0)))
                .clicked()
            {
                *end = true;
            }
        });
    });
    if app.state.voice_speaking && !app.state.voice_reply_text.trim().is_empty() {
        ui.add(
            egui::Label::new(
                egui::RichText::new(app.state.voice_reply_text.trim())
                    .italics()
                    .weak(),
            )
            .wrap(),
        );
    } else if !app.state.voice_partial.trim().is_empty() {
        ui.add(
            egui::Label::new(
                egui::RichText::new(format!("“{}”", app.state.voice_partial.trim()))
                    .italics()
                    .weak(),
            )
            .wrap(),
        );
    } else {
        ui.label(
            egui::RichText::new(
                "Just talk — Gemini Live answers out loud. The microphone pauses while it \
                 speaks, then reopens for your next question. It has the same live radar, \
                 alert and forecast snapshot as a typed question.",
            )
            .small()
            .weak(),
        );
    }
}

/// Four small bars that follow the microphone level.
#[cfg(target_os = "android")]
fn voice_meter(ui: &mut egui::Ui, level: f32, listening: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(30.0, 18.0), egui::Sense::hover());
    let painter = ui.painter();
    let bars = 4usize;
    let gap = 2.0;
    let width = (rect.width() - gap * (bars - 1) as f32) / bars as f32;
    let time = ui.input(|i| i.time) as f32;
    for i in 0..bars {
        let phase = ((time * 6.0 + i as f32 * 1.1).sin() * 0.5 + 0.5).clamp(0.0, 1.0);
        let amp = if listening {
            (level.max(0.08) * (0.55 + 0.45 * phase)).clamp(0.12, 1.0)
        } else {
            0.12
        };
        let height = (rect.height() * amp).max(2.0);
        let x = rect.left() + i as f32 * (width + gap);
        let color = if listening {
            egui::Color32::from_rgb(120, 200, 255)
        } else {
            egui::Color32::from_gray(110)
        };
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x, rect.bottom() - height),
                egui::pos2(x + width, rect.bottom()),
            ),
            1.0,
            color,
        );
    }
}

/// The minimised report: one small bar that keeps the map (which the model may
/// have just moved) visible, with the report one click away.
#[cfg(not(target_os = "android"))]
fn compact_bar(ctx: &egui::Context, app: &mut StormApp) {
    let mut open = app.state.ai_open;
    // Keep the bar above the timeline/status chrome: it is a floating overlay,
    // not part of the bottom panels. The phone timeline is taller than the
    // desktop one (finger-sized scrub strip).
    let bottom_clearance = if cfg!(target_os = "android") {
        116.0
    } else {
        76.0
    };
    egui::Window::new("AI meteorologist")
        .id(egui::Id::new("stormscope.ai.compact"))
        .open(&mut open)
        .resizable(false)
        .title_bar(false)
        .anchor(
            egui::Align2::CENTER_BOTTOM,
            egui::vec2(0.0, -bottom_clearance),
        )
        .frame(egui::Frame::window(&ctx.global_style()).inner_margin(egui::Margin::same(18)))
        .show(ctx, |ui| {
            ui.spacing_mut().button_padding = egui::vec2(18.0, 11.0);
            ui.spacing_mut().item_spacing.x = 12.0;
            ui.set_min_height(48.0);
            ui.horizontal(|ui| {
                if app.state.ai_pending {
                    ui.spinner();
                }
                let status = if app.state.ai_pending {
                    "Meteorologist is looking…".to_string()
                } else if app.state.live_ai_enabled {
                    if let Some(assessment) = app.live_ai.latest_assessment() {
                        match assessment.eta.as_ref() {
                            Some(eta) => format!(
                                "Live · {} ETA {}",
                                assessment.threat.label(),
                                fmt_eta(eta.minutes)
                            ),
                            None => format!("Live · {}", assessment.threat.label()),
                        }
                    } else {
                        "Live · watching for a scan".to_string()
                    }
                } else if app.state.ai_report.is_some() {
                    "Brief ready".to_string()
                } else {
                    "Meteorologist".to_string()
                };
                ui.label(egui::RichText::new(status).strong().size(20.0));
                if ui.button("Expand").clicked() {
                    app.state.ai_compact = false;
                    if app.state.live_ai_enabled {
                        app.state.ai_tab = AiTab::Live;
                    }
                }
                if ui.button("Hide").clicked() {
                    app.state.ai_open = false;
                }
            });
        });
    if !open {
        app.state.ai_open = false;
    }
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
}
