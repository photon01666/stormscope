//! Latest uploads from the weather channels Stormscope follows.
//!
//! YouTube's channel RSS endpoint is the obvious source, but it regularly
//! answers 404/500 for hours at a time and only exposes the newest 15 items.
//! The public `/@handle/videos` page already contains the same newest-first
//! list in its `ytInitialData` JSON, so we read that directly. Each channel
//! contributes at most three videos; the window shows those three, never a
//! bottomless subscription feed.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;

/// Channels shown in the in-app feed. Keep this list focused on people who
/// actually forecast weather; three newest uploads are taken from each
/// channel. Storm-chasing channels are intentionally not included.
pub const CHANNELS: &[YouTubeChannel] = &[
    YouTubeChannel {
        name: "Ryan Hall, Y'all",
        handle: "RyanHallYall",
        id: "UCJHAT3Uvv-g3I8H3GhHWV7w",
    },
    YouTubeChannel {
        name: "Max Velocity",
        handle: "MaxVelocityWX",
        id: "UCvBVK2ymNzPLRJrgip2GeQQ",
    },
];

/// One curated channel.
#[derive(Debug, Clone, Copy)]
pub struct YouTubeChannel {
    pub name: &'static str,
    pub handle: &'static str,
    /// Uploads-playlist/channel id, used by the RSS fallback.
    pub id: &'static str,
}

/// A decoded thumbnail, ready to upload to an egui texture.
#[derive(Clone, Debug)]
pub struct YouTubeThumbnail {
    pub width: u32,
    pub height: u32,
    /// RGBA, tightly packed (`width * height * 4` bytes).
    pub rgba: Vec<u8>,
}

/// One video row in the feed.
#[derive(Clone, Debug)]
pub struct YouTubeVideo {
    pub id: String,
    pub title: String,
    pub channel: String,
    /// Approximate publication time derived from YouTube's relative label
    /// ("8 hours ago"). It is good enough to order the feed; exact seconds
    /// are not shown.
    pub published: DateTime<Utc>,
    pub thumbnail: Option<YouTubeThumbnail>,
}

impl YouTubeVideo {
    pub fn watch_url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.id)
    }
}

pub struct YouTubeSource {
    client: reqwest::blocking::Client,
}

impl YouTubeSource {
    pub fn new(_user_agent: &str) -> Result<Self> {
        // The app's own UA gets a non-browser page from YouTube. Use the
        // browser identity the public page expects.
        let client = reqwest::blocking::Client::builder()
            .user_agent(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
            )
            .timeout(std::time::Duration::from_secs(25))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(Self { client })
    }

    /// Three newest regular uploads from every curated channel. Failed
    /// channels are skipped; the feed still shows whatever succeeded.
    pub fn latest(&self) -> Result<Vec<YouTubeVideo>> {
        // Fetch channels one at a time. YouTube's anti-bot layer is much more
        // aggressive when several channel pages are requested in parallel
        // from a mobile connection; sequential requests with a small gap keep
        // both forecasters in the feed.
        let mut videos = Vec::new();
        let mut errors = Vec::new();
        for (index, channel) in CHANNELS.iter().copied().enumerate() {
            if index > 0 {
                std::thread::sleep(std::time::Duration::from_millis(350));
            }
            match self.channel_videos(channel) {
                Ok(mut found) => {
                    found.truncate(3);
                    videos.extend(found);
                }
                Err(error) => {
                    log::warn!("YouTube feed {}: {error:#}", channel.name);
                    errors.push(format!("{}: {error:#}", channel.name));
                }
            }
        }

        // A channel can repeat an upload in its "popular" shelf; keep the
        // first occurrence only.
        let mut seen = HashSet::new();
        videos.retain(|video| seen.insert(video.id.clone()));
        if videos.is_empty() {
            bail!(
                "no YouTube videos could be loaded{}",
                if errors.is_empty() {
                    String::new()
                } else {
                    format!(": {}", errors.join("; "))
                }
            );
        }

        videos.sort_by(|a, b| b.published.cmp(&a.published));
        self.attach_thumbnails(&mut videos);
        Ok(videos)
    }

    fn channel_videos(&self, channel: YouTubeChannel) -> Result<Vec<YouTubeVideo>> {
        let url = format!(
            "https://www.youtube.com/@{}/videos?hl=en&persist_hl=1",
            channel.handle
        );
        let mut last_error = None;
        for attempt in 0..2 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(900));
            }
            let page = self
                .client
                .get(&url)
                .header("Accept-Language", "en-US,en;q=0.9")
                .header("Cookie", "CONSENT=YES+cb")
                .send()
                .with_context(|| format!("GET {url}"))
                .and_then(|response| {
                    response
                        .error_for_status()
                        .with_context(|| format!("YouTube page HTTP error for {}", channel.name))
                })
                .and_then(|response| response.text().context("reading YouTube channel page"))
                .and_then(|html| {
                    parse_channel_page(&html, channel)
                        .with_context(|| format!("parsing {} videos", channel.name))
                });
            match page {
                Ok(videos) if !videos.is_empty() => return Ok(videos),
                Ok(_) => last_error = Some(anyhow::anyhow!("channel page had no videos")),
                Err(error) => last_error = Some(error),
            }
        }

        // Some mobile networks get a consent/bot page instead of
        // ytInitialData. The RSS endpoint returns the same newest-first list
        // with exact timestamps, so use it when the page is unavailable.
        match self.rss_videos(channel) {
            Ok(videos) if !videos.is_empty() => Ok(videos),
            Ok(_) => Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no videos found"))),
            Err(rss_error) => {
                let page_error = last_error
                    .map(|error| format!("{error:#}"))
                    .unwrap_or_else(|| "no channel page".into());
                bail!("channel page: {page_error}; RSS fallback: {rss_error:#}")
            }
        }
    }

    fn rss_videos(&self, channel: YouTubeChannel) -> Result<Vec<YouTubeVideo>> {
        let url = format!(
            "https://www.youtube.com/feeds/videos.xml?channel_id={}",
            channel.id
        );
        let xml = self
            .client
            .get(&url)
            .header("Accept-Language", "en-US,en;q=0.9")
            .send()
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("YouTube RSS HTTP error for {}", channel.name))?
            .text()
            .context("reading YouTube RSS")?;
        let mut videos = parse_rss(&xml, channel)?;
        videos.sort_by(|a, b| b.published.cmp(&a.published));
        Ok(videos)
    }

    fn attach_thumbnails(&self, videos: &mut [YouTubeVideo]) {
        let thumbnails = std::thread::scope(|scope| {
            let handles: Vec<_> = videos
                .iter()
                .map(|video| scope.spawn(|| self.thumbnail(&video.id)))
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap_or(None))
                .collect::<Vec<_>>()
        });
        for (video, thumbnail) in videos.iter_mut().zip(thumbnails) {
            video.thumbnail = thumbnail;
        }
    }

    fn thumbnail(&self, video_id: &str) -> Option<YouTubeThumbnail> {
        let url = format!("https://i.ytimg.com/vi/{video_id}/mqdefault.jpg");
        let bytes = self
            .client
            .get(&url)
            .send()
            .ok()?
            .error_for_status()
            .ok()?
            .bytes()
            .ok()?;
        let image = image::load_from_memory(&bytes).ok()?.to_rgba8();
        Some(YouTubeThumbnail {
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
        })
    }
}

/// Parse a YouTube channel's `/videos` page. `ytInitialData` is a large JSON
/// object embedded in the HTML; its video grid uses either the current
/// `lockupViewModel` shape or the older `videoRenderer` shape.
pub fn parse_channel_page(html: &str, channel: YouTubeChannel) -> Result<Vec<YouTubeVideo>> {
    let marker = "var ytInitialData = ";
    let fallback = "ytInitialData = ";
    let (start, len) = if let Some(start) = html.find(marker) {
        (start, marker.len())
    } else if let Some(start) = html.find(fallback) {
        (start, fallback.len())
    } else {
        bail!("YouTube page did not contain ytInitialData");
    };
    let json = &html[start + len..];
    let mut deserializer = serde_json::Deserializer::from_str(json);
    let root = Value::deserialize(&mut deserializer).context("decoding ytInitialData JSON")?;

    let mut videos = Vec::new();
    collect_videos(&root, channel.name, &mut videos);
    if videos.is_empty() {
        bail!("no video lockups found");
    }
    Ok(videos)
}

/// Parse YouTube's Atom upload feed. Used when the channel page is blocked
/// by a consent/bot interstitial.
pub fn parse_rss(xml: &str, channel: YouTubeChannel) -> Result<Vec<YouTubeVideo>> {
    let mut out = Vec::new();
    for chunk in xml.split("<entry>").skip(1) {
        let item = chunk.split("</entry>").next().unwrap_or(chunk);
        let Some(id) = between(item, "<yt:videoId>", "</yt:videoId>") else {
            continue;
        };
        let Some(title) = between(item, "<title>", "</title>") else {
            continue;
        };
        let Some(published) = between(item, "<published>", "</published>") else {
            continue;
        };
        let Ok(published) = chrono::DateTime::parse_from_rfc3339(published.trim()) else {
            continue;
        };
        out.push(YouTubeVideo {
            id: id.trim().to_string(),
            title: unescape_xml(title.trim()),
            channel: channel.name.to_string(),
            published: published.with_timezone(&Utc),
            thumbnail: None,
        });
    }
    if out.is_empty() {
        bail!("YouTube RSS feed had no entries");
    }
    Ok(out)
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let start = text.find(start)? + start.len();
    let end = text[start..].find(end)? + start;
    Some(&text[start..end])
}

fn unescape_xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        let tail = &rest[index..];
        let (value, consumed) = if tail.starts_with("&amp;") {
            ("&", 5)
        } else if tail.starts_with("&lt;") {
            ("<", 4)
        } else if tail.starts_with("&gt;") {
            (">", 4)
        } else if tail.starts_with("&quot;") {
            ("\"", 6)
        } else if tail.starts_with("&#39;") {
            ("'", 5)
        } else if tail.starts_with("&apos;") {
            ("'", 6)
        } else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        out.push_str(value);
        rest = &tail[consumed..];
    }
    out.push_str(rest);
    out
}

fn collect_videos(value: &Value, channel: &str, out: &mut Vec<YouTubeVideo>) {
    match value {
        Value::Object(map) => {
            if let Some(lockup) = map.get("lockupViewModel")
                && let Some(video) = parse_lockup(lockup, channel)
                && !out.iter().any(|existing| existing.id == video.id)
            {
                out.push(video);
            }
            if let Some(renderer) = map.get("videoRenderer")
                && let Some(video) = parse_video_renderer(renderer, channel)
                && !out.iter().any(|existing| existing.id == video.id)
            {
                out.push(video);
            }
            for child in map.values() {
                collect_videos(child, channel, out);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_videos(child, channel, out);
            }
        }
        _ => {}
    }
}

fn parse_lockup(value: &Value, channel: &str) -> Option<YouTubeVideo> {
    if value.get("contentType")?.as_str()? != "LOCKUP_CONTENT_TYPE_VIDEO" {
        return None;
    }
    let id = value.get("contentId")?.as_str()?;
    let title = value
        .pointer("/metadata/lockupMetadataViewModel/title/content")?
        .as_str()?;
    if id.is_empty() || title.is_empty() {
        return None;
    }
    let age = find_age_text(value)?;
    let age_secs = relative_age_secs(age)?;
    Some(YouTubeVideo {
        id: id.to_string(),
        title: title.to_string(),
        channel: channel.to_string(),
        published: Utc::now() - Duration::seconds(age_secs.min(i64::MAX as u64) as i64),
        thumbnail: None,
    })
}

fn parse_video_renderer(value: &Value, channel: &str) -> Option<YouTubeVideo> {
    let id = value.get("videoId")?.as_str()?;
    let title = if let Some(title) = value.pointer("/title/simpleText").and_then(Value::as_str) {
        title.to_string()
    } else {
        let runs = value.pointer("/title/runs")?.as_array()?;
        runs.iter()
            .filter_map(|run| run.get("text").and_then(Value::as_str))
            .collect::<String>()
    };
    if id.is_empty() || title.is_empty() {
        return None;
    }
    let age = value
        .pointer("/publishedTimeText/simpleText")
        .and_then(Value::as_str)
        .or_else(|| find_age_text(value))?;
    let age_secs = relative_age_secs(age)?;
    Some(YouTubeVideo {
        id: id.to_string(),
        title,
        channel: channel.to_string(),
        published: Utc::now() - Duration::seconds(age_secs.min(i64::MAX as u64) as i64),
        thumbnail: None,
    })
}

fn find_age_text(value: &Value) -> Option<&str> {
    match value {
        Value::Object(map) => {
            for child in map.values() {
                if let Some(text) = find_age_text(child) {
                    return Some(text);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(find_age_text),
        Value::String(text) => relative_age_secs(text).map(|_| text.as_str()),
        _ => None,
    }
}

/// Convert YouTube's relative label ("8h ago", "Streamed 2 days ago",
/// "Premiered 1 month ago") to seconds. Returns `None` for schedules and
/// anything that is not a relative age.
pub fn relative_age_secs(text: &str) -> Option<u64> {
    let lower = text.to_ascii_lowercase();
    // YouTube relative labels always end in "ago" (sometimes prefixed with
    // "Streamed" / "Premiered"). Requiring that also stops view counts such as
    // "1.2M" from being mistaken for a duration.
    if !lower.contains("ago") {
        return None;
    }
    let bytes = lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let number = lower[start..i].parse::<u64>().ok()?;
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'-') {
            i += 1;
        }
        let unit_start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let unit = &lower[unit_start..i];
        let seconds = match unit {
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3_600,
            "d" | "day" | "days" => 86_400,
            "w" | "week" | "weeks" => 604_800,
            "mo" | "mon" | "month" | "months" => 2_592_000,
            "y" | "yr" | "yrs" | "year" | "years" => 31_536_000,
            _ => continue,
        };
        return number.checked_mul(seconds);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_ages_parse() {
        assert_eq!(relative_age_secs("8h ago"), Some(8 * 3_600));
        assert_eq!(relative_age_secs("Streamed 2 days ago"), Some(2 * 86_400));
        assert_eq!(relative_age_secs("Premiered 1 month ago"), Some(2_592_000));
        assert_eq!(relative_age_secs("3 weeks ago"), Some(3 * 604_800));
        assert_eq!(relative_age_secs("12 minutes ago"), Some(12 * 60));
        assert_eq!(relative_age_secs("45 seconds ago"), Some(45));
        assert_eq!(relative_age_secs("1 year ago"), Some(31_536_000));
        assert_eq!(relative_age_secs("Scheduled for Oct 30, 2026"), None);
    }

    #[test]
    fn lockup_parser_reads_current_youtube_shape() {
        let channel = YouTubeChannel {
            name: "Test Weather",
            handle: "testweather",
            id: "UCtestweather",
        };
        let data = serde_json::json!({
            "contents": {
                "richItemRenderer": {
                    "content": {
                        "lockupViewModel": {
                            "contentId": "abc123XYZ_-",
                            "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
                            "metadata": {
                                "lockupMetadataViewModel": {
                                    "title": {"content": "A storm is coming"},
                                    "metadata": {
                                        "contentMetadataViewModel": {
                                            "metadataRows": [{
                                                "metadataParts": [
                                                    {"text": {"content": "1.2M"}},
                                                    {"text": {"content": "8h ago"}}
                                                ]
                                            }]
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
        let html = format!("<script>var ytInitialData = {data}</script>");
        let videos = parse_channel_page(&html, channel).unwrap();
        assert_eq!(videos.len(), 1);
        assert_eq!(videos[0].id, "abc123XYZ_-");
        assert_eq!(videos[0].title, "A storm is coming");
        assert_eq!(videos[0].channel, "Test Weather");
        assert!(videos[0].published < Utc::now());
    }

    #[test]
    #[ignore = "network: checks the live curated YouTube feed"]
    fn live_youtube_feed() {
        let source = YouTubeSource::new("stormscope-test").unwrap();
        let videos = source.latest().unwrap();
        assert!(!videos.is_empty());
        for video in videos.iter().take(6) {
            println!(
                "{} | {} | {} | thumb={}",
                video.channel,
                video.published,
                video.title,
                video.thumbnail.is_some()
            );
        }
        for channel in CHANNELS {
            let n = videos.iter().filter(|v| v.channel == channel.name).count();
            assert!(n <= 3, "{} returned {n} videos", channel.name);
            println!("{:3}  {}", n, channel.name);
        }
    }

    #[test]
    fn old_video_renderer_parser_still_works() {
        let channel = YouTubeChannel {
            name: "Test Weather",
            handle: "testweather",
            id: "UCtestweather",
        };
        let data = serde_json::json!({
            "contents": {
                "videoRenderer": {
                    "videoId": "old123",
                    "title": {"simpleText": "Older upload"},
                    "publishedTimeText": {"simpleText": "2 days ago"}
                }
            }
        });
        let html = format!("<script>var ytInitialData = {data}</script>");
        let videos = parse_channel_page(&html, channel).unwrap();
        assert_eq!(videos.len(), 1);
        assert_eq!(videos[0].title, "Older upload");
    }
}
