//! YouTube video playback.
//!
//! Desktop embeds a native WebView child over the full app surface
//! (`video_embed`). Android hands the video to the installed YouTube app
//! because YouTube rejects its embedded player in Android WebViews with a
//! sign-in/bot interstitial.

use crate::app::StormApp;
use crate::state::YouTubePlayer;

fn valid_video_id(id: &str) -> bool {
    (6..=24).contains(&id.len())
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

/// Open a video. Desktop sets the player state; the embedded WebView is
/// synced from `update`. Android launches the YouTube app.
pub fn open(app: &mut StormApp, video_id: &str, title: &str) -> Result<(), String> {
    if !valid_video_id(video_id) {
        return Err("YouTube returned an invalid video id".into());
    }

    #[cfg(target_os = "android")]
    {
        let _ = (app, title);
        // YouTube blocks its embedded player in Android WebViews with a
        // sign-in/bot interstitial. Hand the video to the installed YouTube
        // app (or the browser if it is absent) instead.
        crate::android_video::open(video_id)
    }

    #[cfg(not(target_os = "android"))]
    {
        app.state.youtube_player = Some(YouTubePlayer {
            id: video_id.to_string(),
            title: title.to_string(),
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_ids_are_safe_for_the_player() {
        assert!(valid_video_id("dQw4w9WgXcQ"));
        assert!(!valid_video_id("bad/id"));
        assert!(!valid_video_id("short"));
    }
}
