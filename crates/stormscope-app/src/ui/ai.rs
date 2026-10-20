//! The AI meteorologist: the Settings popup that configures the endpoint and
//! the report window that asks for a brief (or a question) about the markers.
//!
//! The snapshot, prompts and `MAP:` directives live in `crate::brief` and
//! `crate::ai_actions`; this module is presentation and request triggering.

use crate::app::StormApp;

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

pub fn ai_windows(ctx: &egui::Context, app: &mut StormApp) {
    settings_window(ctx, app);
    report_window(ctx, app);
}

fn settings_window(ctx: &egui::Context, app: &mut StormApp) {
    if !app.state.ai_settings_open {
        return;
    }
    let screen = ctx.content_rect().size();
    let text_scale = (app.sidebar_scale * 0.75).clamp(1.0, 1.8);
    let size = egui::vec2(
        (screen.x * 0.32).clamp(440.0, 640.0),
        (screen.y * 0.46).clamp(320.0, 560.0),
    );
    let max_h = (screen.y * 0.8).max(300.0);
    let mut open = app.state.ai_settings_open;
    let mut save = false;
    let mut test = false;
    let mut preset: Option<(&str, &str)> = None;

    egui::Window::new("AI meteorologist — settings")
        .id(egui::Id::new("stormscope.ai.settings"))
        .open(&mut open)
        .default_size(size)
        .max_height(max_h)
        .min_size(egui::vec2(380.0, 300.0))
        .default_pos(super::centered_popup_pos(ctx, size))
        .show(ctx, |ui| {
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
    app.state.ai_settings_open = open;
}

fn report_window(ctx: &egui::Context, app: &mut StormApp) {
    if !app.state.ai_open {
        return;
    }
    if app.state.ai_compact {
        compact_bar(ctx, app);
        return;
    }
    let screen = ctx.content_rect().size();
    let text_scale = (app.sidebar_scale * 0.75).clamp(1.0, 1.8);
    let size = egui::vec2(
        (screen.x * 0.33).clamp(460.0, 720.0),
        (screen.y * 0.56).clamp(340.0, 620.0),
    );
    let max_h = (screen.y * 0.82).max(300.0);
    let mut open = app.state.ai_open;
    let mut brief = false;
    let mut ask: Option<String> = None;
    let mut restore = false;
    let mut clear = false;

    egui::Window::new("AI meteorologist")
        .id(egui::Id::new("stormscope.ai"))
        .open(&mut open)
        .default_size(size)
        .max_height(max_h)
        .min_size(egui::vec2(380.0, 240.0))
        .default_pos(egui::pos2(
            18.0,
            (ctx.content_rect().height() - size.y - 74.0).max(64.0),
        ))
        .show(ctx, |ui| {
            super::apply_text_scale(ui, text_scale);
            ui.set_width((size.x - 40.0).max(340.0));
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
            if !configured {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "Paste your OpenCode Go API key in Settings → AI meteorologist. \
                         The fields are already set to OpenCode Go (deepseek-v4.1-flash).",
                    )
                    .strong(),
                );
                if ui.button("Open AI settings").clicked() {
                    app.state.ai_settings_open = true;
                }
                return;
            }
            if app.state.markers.is_empty() {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "Place markers on the map (Shift+click) — the meteorologist writes about \
                         those locations, using the radar, warnings, storm cells and forecast \
                         around them.",
                    )
                    .weak(),
                );
                return;
            }

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let label = if app.state.ai_pending {
                    "Working…"
                } else if app.state.ai_report.is_some() {
                    "Refresh brief"
                } else {
                    "Write brief"
                };
                if ui
                    .button(label)
                    .on_hover_text("A short now / next-90-minutes report for every marker")
                    .clicked()
                    && !app.state.ai_pending
                {
                    brief = true;
                }
                if app.state.ai_pending {
                    ui.spinner();
                }
                if app.state.ai_report.is_some() && ui.small_button("Clear").clicked() {
                    clear = true;
                }
                if !app.state.ai_annotations.is_empty()
                    && ui
                        .small_button("Clear marks")
                        .on_hover_text("Remove the AI's circles from the 2D and 3D maps")
                        .clicked()
                {
                    clear = true;
                }
                if let Some(snapshot) = &app.state.ai_snapshot {
                    ui.label(egui::RichText::new(snapshot).small().weak());
                }
            });

            ui.horizontal(|ui| {
                let hint = "Ask about these markers — e.g. \"is the hail core going to hit the \
                            north pin?\"";
                let response = ui.add(
                    egui::TextEdit::singleline(&mut app.state.ai_question)
                        .desired_width((ui.available_width() - 72.0).max(100.0))
                        .hint_text(hint),
                );
                let submit = response.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    && !app.state.ai_pending;
                if (ui.button("Ask").clicked() || submit) && !app.state.ai_pending {
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
                    && !app.state.ai_pending
                {
                    ask = Some(
                        "Show me on the map what happens in the next 60 minutes at my markers."
                            .into(),
                    );
                }
            });

            if let Some(error) = &app.state.ai_error {
                ui.add_space(4.0);
                ui.colored_label(egui::Color32::from_rgb(240, 120, 110), error);
            }

            if let Some(actions) = &app.state.ai_actions {
                ui.add_space(2.0);
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

            ui.separator();
            if let Some(question) = &app.state.ai_last_question {
                ui.label(egui::RichText::new(format!("Q: {question}")).strong());
                ui.add_space(2.0);
            }
            if let Some(report) = &app.state.ai_report {
                if let Some(at) = app.state.ai_generated_at {
                    ui.label(
                        egui::RichText::new(format!(
                            "written {}s ago · {}",
                            at.elapsed().as_secs(),
                            app.state
                                .ai_snapshot
                                .clone()
                                .unwrap_or_else(|| "snapshot".into())
                        ))
                        .small()
                        .weak(),
                    );
                }
                egui::ScrollArea::vertical()
                    .auto_shrink([false, true])
                    .max_height((screen.y * 0.30).clamp(140.0, 360.0))
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(clean_report(report)))
                                .selectable(true)
                                .wrap(),
                        );
                    });
                if ui.button("Copy").clicked() {
                    ui.ctx().copy_text(clean_report(report));
                }
            } else if let Some(looking) = &app.state.ai_looking_at {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(egui::RichText::new(format!("Looking at {looking}…")).weak());
                });
            } else if app.state.ai_pending {
                ui.label(egui::RichText::new("Sampling the radar, warnings and forecast…").weak());
            } else {
                ui.label(
                    egui::RichText::new(
                        "Write brief reads every marker; Ask takes one question. Both use the \
                         same snapshot and may move the map to illustrate the answer.",
                    )
                    .weak(),
                );
            }
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
        });
    app.state.ai_open = open;
    if brief {
        app.ai_request(None);
    }
    if let Some(question) = ask {
        app.state.ai_question.clear();
        app.ai_request(Some(question));
    }
    if restore && crate::ai_actions::restore(&mut app.state) {
        app.state.ai_actions = None;
    }
    if clear {
        app.state.ai_report = None;
        app.state.ai_last_question = None;
        app.state.ai_error = None;
        app.state.ai_actions = None;
        app.state.ai_annotations.clear();
        app.state.ai_view_before = None;
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
