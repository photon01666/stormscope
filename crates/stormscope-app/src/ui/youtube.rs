//! "Weather videos" window: the newest three uploads from each curated
//! weather YouTube channel, with in-app playback.

use crate::app::StormApp;
use radar_data::youtube::{CHANNELS, YouTubeVideo};
use std::collections::HashSet;

const REFRESH_SECS: u64 = 30 * 60;

pub fn youtube_window(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    if !app.state.youtube_open {
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_secs(1));

    let stale = app
        .state
        .youtube_fetched_at
        .is_none_or(|at| at.elapsed().as_secs() >= REFRESH_SECS);
    let retry = app.state.youtube_error.is_some()
        && app
            .state
            .youtube_last_attempt
            .is_none_or(|at| at.elapsed().as_secs() >= 60);
    if !app.state.youtube_pending && (stale || retry) {
        app.state.youtube_pending = true;
        app.state.youtube_last_attempt = Some(std::time::Instant::now());
        let _ = app.tx.send(crate::app::DataCmd::LoadYouTubeFeed);
    }

    let fill = cfg!(target_os = "android");
    let scale = super::big_text_scale(app);
    let screen = ctx.content_rect().size();
    let max_h = super::popup_max_height(ctx, scale);
    let desktop_w = (680.0 * scale).min((screen.x - 32.0).max(260.0));
    let desktop_h = (740.0 * scale).min(max_h);
    let mut open = app.state.youtube_open;
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut close = false;
    let mut play_request: Option<(String, String)> = None;

    let mut window = egui::Window::new("Forecast videos")
        .id(egui::Id::new("stormscope.youtube.feed"))
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .max_width((screen.x - 16.0).max(260.0));
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
        window
            .fixed_size(egui::vec2(desktop_w, desktop_h))
            .resizable(false)
            .collapsible(false)
    };
    window.open(&mut open).show(ctx, |ui| {
        #[cfg(target_os = "android")]
        if fill {
            close |= super::phone_header(ui, "Forecast videos");
        }
        super::apply_text_scale(ui, scale);

        ui.horizontal_wrapped(|ui| {
            if app.state.youtube_pending {
                ui.spinner();
                if app.state.youtube_videos.is_empty() {
                    ui.label("Loading forecasters…");
                }
            }
            if let Some(at) = app.state.youtube_fetched_at {
                ui.label(
                    egui::RichText::new(format!("updated {}", ago(at.elapsed().as_secs())))
                        .weak()
                        .small(),
                );
            }
            if !app.state.youtube_videos.is_empty() {
                let shown = app.state.youtube_videos.len();
                ui.label(
                    egui::RichText::new(format!("{shown} videos · 3 newest per channel"))
                        .weak()
                        .small(),
                );
            }
        });
        if let Some(error) = &app.state.youtube_error {
            ui.label(
                egui::RichText::new(format!("Feed unavailable: {error}"))
                    .weak()
                    .small(),
            );
        }
        if let Some(error) = &app.state.youtube_player_error {
            ui.label(
                egui::RichText::new(format!("Video player: {error}"))
                    .color(egui::Color32::from_rgb(255, 160, 120))
                    .small(),
            );
        }

        ui.add_space(4.0);
        if app.state.youtube_videos.is_empty() && !app.state.youtube_pending {
            ui.label(
                egui::RichText::new("No videos loaded yet. Refresh to fetch the newest uploads.")
                    .weak(),
            );
            if !fill {
                super::fill_popup_height(ui);
            }
            return;
        }

        // Clone the Arc list: thumbnails are loaded into StormApp's texture
        // cache while the rows are drawn, so holding a borrow of `state`
        // would fight the texture cache.
        let videos = app.state.youtube_videos.clone();
        let scroll_h = if fill {
            ui.available_height()
        } else {
            (desktop_h - 108.0 * scale).max(160.0)
        };
        egui::ScrollArea::vertical()
            .id_salt("forecast-videos-scroll")
            .auto_shrink([false, false])
            .max_height(scroll_h)
            .show(ui, |ui| {
                // Group by channel name. Iterate the curated order first so
                // the window always lists the forecasters in the same order.
                let mut groups: std::collections::BTreeMap<String, Vec<_>> =
                    std::collections::BTreeMap::new();
                for video in &videos {
                    groups
                        .entry(video.channel.clone())
                        .or_default()
                        .push(video.clone());
                }
                let mut any = false;
                for channel in CHANNELS {
                    let Some(channel_videos) = groups.remove(channel.name) else {
                        continue;
                    };
                    any = true;
                    ui.add_space(if any { 8.0 } else { 0.0 });
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(channel.name).strong());
                        ui.label(
                            egui::RichText::new(format!("{} newest", channel_videos.len().min(3)))
                                .weak()
                                .small(),
                        );
                    });
                    for video in channel_videos.iter().take(3) {
                        let texture = thumbnail_texture(app, ctx, video);
                        if video_row(ui, video, texture.as_ref(), scale) {
                            play_request = Some((video.id.clone(), video.title.clone()));
                        }
                    }
                }
                for (name, channel_videos) in groups {
                    any = true;
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(name).strong());
                        ui.label(
                            egui::RichText::new(format!("{} newest", channel_videos.len().min(3)))
                                .weak()
                                .small(),
                        );
                    });
                    for video in channel_videos.iter().take(3) {
                        let texture = thumbnail_texture(app, ctx, video);
                        if video_row(ui, video, texture.as_ref(), scale) {
                            play_request = Some((video.id.clone(), video.title.clone()));
                        }
                    }
                }
                if !any && !app.state.youtube_pending {
                    ui.label(
                        egui::RichText::new("The feed did not contain any playable videos.").weak(),
                    );
                }
            });

        if !fill {
            super::fill_popup_height(ui);
        }

        // Drop textures for videos that are no longer in the feed.
        if !videos.is_empty() {
            let keep: HashSet<&str> = videos.iter().map(|video| video.id.as_str()).collect();
            app.youtube_tex.retain(|(id, _)| {
                if keep.contains(id.as_str()) {
                    true
                } else {
                    ctx.forget_image(&texture_name(id));
                    false
                }
            });
        }
    });

    if let Some((id, title)) = play_request {
        match crate::player::open(app, &id, &title) {
            Ok(()) => app.state.youtube_player_error = None,
            Err(error) => app.state.youtube_player_error = Some(error),
        }
    }
    if close {
        open = false;
    }
    app.state.youtube_open = open;
}

/// The in-app player always fills the map pane. The native WebView is
/// positioned over the reserved `youtube_video_rect` after egui finishes
/// drawing it. There is no movable/fullscreen state: a fixed pane keeps the
/// video and its border exactly in sync.
pub fn youtube_player_window(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    #[cfg(target_os = "android")]
    {
        let _ = (ctx, app, area);
        return;
    }

    #[cfg(not(target_os = "android"))]
    {
        let Some(player) = app.state.youtube_player.clone() else {
            app.youtube_video_rect = None;
            return;
        };
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            app.state.youtube_player = None;
            app.youtube_video_rect = None;
            return;
        }

        // No egui frame: the video fills the whole app window edge to edge.
        // The page carries its own Close button, so there is no chrome to
        // drift out of sync with the native child.
        let _ = area;
        let _ = &player.title;
        app.youtube_video_rect = Some(ctx.content_rect());
    }
}

fn thumbnail_texture(
    app: &mut StormApp,
    ctx: &egui::Context,
    video: &YouTubeVideo,
) -> Option<egui::TextureHandle> {
    if let Some((_, texture)) = app.youtube_tex.iter().find(|(id, _)| id == &video.id) {
        return Some(texture.clone());
    }
    let thumbnail = video.thumbnail.as_ref()?;
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [thumbnail.width as usize, thumbnail.height as usize],
        &thumbnail.rgba,
    );
    let texture = ctx.load_texture(texture_name(&video.id), image, egui::TextureOptions::LINEAR);
    app.youtube_tex.push((video.id.clone(), texture.clone()));
    Some(texture)
}

fn texture_name(id: &str) -> String {
    format!("stormscope-youtube-{id}")
}

fn video_row(
    ui: &mut egui::Ui,
    video: &YouTubeVideo,
    texture: Option<&egui::TextureHandle>,
    scale: f32,
) -> bool {
    let mut play = false;
    let size = egui::vec2(150.0 * scale, 84.0 * scale);
    ui.horizontal(|ui| {
        if let Some(texture) = texture {
            let image = egui::Image::new((texture.id(), size)).sense(egui::Sense::click());
            if ui.add(image).clicked() {
                play = true;
            }
        } else {
            let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
            ui.painter()
                .rect_filled(rect, 5.0, egui::Color32::from_gray(32));
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "▶",
                egui::FontId::proportional(24.0 * scale),
                egui::Color32::from_gray(150),
            );
            if response.clicked() {
                play = true;
            }
        }
        ui.allocate_ui_with_layout(
            egui::vec2((ui.available_width() - 8.0).max(120.0), size.y),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.add(egui::Label::new(egui::RichText::new(&video.title).strong()).wrap());
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(video.channel.as_str()).weak().small());
                    ui.label(
                        egui::RichText::new(ago((chrono::Utc::now() - video.published)
                            .num_seconds()
                            .max(0) as u64))
                        .weak()
                        .small(),
                    );
                });
                if ui
                    .small_button("Play")
                    .on_hover_text("Play this video in the Stormscope video window")
                    .clicked()
                {
                    play = true;
                }
            },
        );
    });
    ui.separator();
    play
}

fn ago(seconds: u64) -> String {
    if seconds < 60 {
        "just now".into()
    } else if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3_600)
    } else if seconds < 604_800 {
        format!("{}d ago", seconds / 86_400)
    } else if seconds < 2_592_000 {
        format!("{}w ago", seconds / 604_800)
    } else {
        format!("{}mo ago", seconds / 2_592_000)
    }
}
