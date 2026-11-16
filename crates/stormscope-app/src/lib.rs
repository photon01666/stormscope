#![recursion_limit = "512"]
//! Stormscope — app shell.
//!
//! Layout: auto-hiding controls sidebar (site, product, tilt, opacity, loop
//! controls) sliding in over the central map on left-edge hover,
//! central map canvas (basemap tiles + radar overlay + warning polygons),
//! bottom timeline scrubber, top status bar.
//!
//! This module owns UI + state wiring only; heavy work (fetch, decode,
//! rasterize) happens on background threads whose results are polled each frame.

mod ai_actions;
#[cfg(target_os = "android")]
pub mod android_ime;
#[cfg(target_os = "android")]
pub mod android_lifecycle;
#[cfg(target_os = "android")]
pub mod android_live_audio;
#[cfg(target_os = "android")]
pub mod android_location;
#[cfg(target_os = "android")]
pub mod android_storm_watch;
#[cfg(target_os = "android")]
pub mod android_tts;
#[cfg(target_os = "android")]
pub mod android_video;
mod app;
mod background;
mod brief;
#[cfg(windows)]
mod embedded_piper;
#[cfg(target_os = "android")]
pub mod gemini_live;
#[cfg(not(target_os = "android"))]
mod gemini_speech;
mod geodata;
mod live_ai;
mod player;
pub mod render3d;
mod state;
mod timefmt;
mod timeline;
mod ui;
#[cfg(not(target_os = "android"))]
mod video_embed;
mod voice;

pub use app::StormApp;

/// Release-facing version. Cargo only accepts three-part semver, so a hotfix
/// built as `1.9.7+1` is shown in the release's four-part form (`1.9.7.1`).
pub fn display_version() -> String {
    dotted_build_version(env!("CARGO_PKG_VERSION"))
}

fn dotted_build_version(raw: &str) -> String {
    raw.split_once('+')
        .map(|(base, build)| format!("{base}.{build}"))
        .unwrap_or_else(|| raw.to_string())
}

#[cfg(test)]
mod version_tests {
    use super::dotted_build_version;

    #[test]
    fn build_metadata_is_shown_with_a_dot() {
        assert_eq!(dotted_build_version("1.9.7+1"), "1.9.7.1");
        assert_eq!(dotted_build_version("1.9.8"), "1.9.8");
    }
}
