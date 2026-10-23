//! Spoken warnings/watches for the desktop builds.
//!
//! Triggering is deliberately narrow: only products that are warnings or
//! watches, and only when the alert overlaps the current 2D map viewport
//! around the selected radar. Speech is serialized on one worker thread;
//! command-line TTS on Linux and SAPI on Windows would otherwise overlap when
//! a burst of alerts arrives at once.

use radar_core::warnings::Warning;
use radar_render::MapView;
use std::sync::{OnceLock, mpsc};

/// The most alerts read from one batch before summarizing the rest. Two or
/// three warnings are already a long interruption; reading a whole outbreak
/// would talk for minutes.
pub const MAX_BURST: usize = 3;

/// The independent switches in Settings → Voice alerts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Tornado,
    SevereThunderstorm,
    Watch,
    OtherWarning,
}

/// Products worth interrupting for: warnings, watches, and flash-flood
/// emergencies. Advisories and statements are skipped.
pub fn announceable(w: &Warning) -> bool {
    let event = w.event.as_str();
    (event.contains("Warning") || event.contains("Watch") || event.contains("Emergency"))
        && !event.contains("Advisory")
        && !event.contains("Statement")
}

/// Which Settings switch controls this alert. Watches win over their hazard
/// family, so a Tornado Watch follows the Watches switch rather than the
/// Tornado warnings switch.
pub fn category(w: &Warning) -> Option<Category> {
    if !announceable(w) {
        return None;
    }
    let event = w.event.as_str();
    Some(if event.contains("Watch") {
        Category::Watch
    } else if event.contains("Tornado") {
        Category::Tornado
    } else if event.contains("Severe Thunderstorm") {
        Category::SevereThunderstorm
    } else {
        Category::OtherWarning
    })
}

/// Whether the alert is in the current 2D map viewport.
///
/// Polygon alerts are checked by projecting their rings. Zone-only alerts have
/// no geometry, so they are in view only when they apply to the selected
/// station and the station itself is on screen.
pub fn in_view(w: &Warning, view: &MapView, station: Option<(f64, f64)>) -> bool {
    match w.polygon.as_deref() {
        Some(rings) if !rings.is_empty() => rings.iter().any(|ring| ring_in_view(ring, view)),
        _ => station.is_some_and(|(lat, lon)| point_in_view(lon, lat, view)),
    }
}

fn point_in_view(lon: f64, lat: f64, view: &MapView) -> bool {
    let (x, y) = view.lonlat_to_view_px(lon, lat);
    x >= 0.0 && x <= f64::from(view.width_px) && y >= 0.0 && y <= f64::from(view.height_px)
}

fn ring_in_view(ring: &[(f64, f64)], view: &MapView) -> bool {
    if ring.is_empty() {
        return false;
    }
    let width = f64::from(view.width_px);
    let height = f64::from(view.height_px);
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for &(lon, lat) in ring {
        let (x, y) = view.lonlat_to_view_px(lon, lat);
        if x >= 0.0 && x <= width && y >= 0.0 && y <= height {
            return true;
        }
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    // The viewport can be entirely inside a large warning polygon, in which
    // case no vertex is on screen but the bounding boxes still overlap.
    min_x <= width && max_x >= 0.0 && min_y <= height && max_y >= 0.0
}

/// Priority for ordering a burst: tornado warning first, then emergencies,
/// severe thunderstorm warnings, flash flood warnings, other warnings, and
/// watches last. A tornado *watch* must not outrank a severe warning.
pub fn rank(w: &Warning) -> u8 {
    let event = w.event.as_str();
    if event.contains("Tornado") && event.contains("Warning") {
        6
    } else if event.contains("Emergency") {
        5
    } else if event.contains("Severe Thunderstorm") && event.contains("Warning") {
        4
    } else if event.contains("Flash Flood") && event.contains("Warning") {
        3
    } else if event.contains("Warning") {
        2
    } else {
        1
    }
}

/// A short spoken sentence. The full CAP description can run several
/// paragraphs; reading it would bury the information the listener needs.
pub fn announcement(w: &Warning) -> String {
    let mut text = format!("New {}", w.event);
    if let Some(area) = spoken_area(&w.area) {
        text.push_str(" for ");
        text.push_str(&area);
    }
    let expires = w.expires.with_timezone(&chrono::Local);
    let time = expires.format("%I:%M %p").to_string();
    // "5:30 PM" reads better than "05:30 PM"; %I is always 01–12, so the
    // only leading zero is the one trimmed here.
    text.push_str(&format!(". Expires at {}.", time.trim_start_matches('0')));
    if let Some(action) = action_sentence(w) {
        text.push(' ');
        text.push_str(&action);
    }
    text
}

fn spoken_area(area: &str) -> Option<String> {
    let mut parts: Vec<&str> = area
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }
    let more = parts.len() > 2;
    parts.truncate(2);
    let mut text = parts.join(" and ");
    if more {
        text.push_str(" and surrounding areas");
    }
    Some(clip(&text, 120))
}

fn action_sentence(w: &Warning) -> Option<String> {
    if w.event.contains("Watch") {
        return None;
    }
    let first = w
        .instruction
        .split(|c| matches!(c, '.' | '!' | '?'))
        .map(str::trim)
        .find(|sentence| !sentence.is_empty());
    if let Some(sentence) = first {
        return Some(format!("{}.", clip(sentence, 160)));
    }
    if w.event.contains("Tornado") {
        Some("Take cover now.".to_string())
    } else if w.event.contains("Severe Thunderstorm") {
        Some("Move indoors now.".to_string())
    } else if w.event.contains("Flash Flood") {
        Some("Move to higher ground now.".to_string())
    } else {
        None
    }
}

fn clip(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        out.push('…');
    }
    out
}

/// Queue text for spoken delivery. The first call starts one worker thread;
/// later calls are serialized behind it, so a burst of alerts does not become
/// an unintelligible overlap.
pub fn announce(text: impl Into<String>) {
    static QUEUE: OnceLock<mpsc::Sender<String>> = OnceLock::new();
    let tx = QUEUE.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<String>();
        if let Err(e) = std::thread::Builder::new()
            .name("voice-tts".into())
            .spawn(move || {
                for text in rx {
                    log::debug!("voice: speaking {text:?}");
                    speak_blocking(&text);
                }
            })
        {
            log::warn!("voice: could not start TTS thread: {e}");
        }
        tx
    });
    if let Err(e) = tx.send(text.into()) {
        log::warn!("voice: TTS queue closed: {e}");
    }
}

#[cfg(target_os = "linux")]
fn speak_blocking(text: &str) {
    // speech-dispatcher is the desktop-standard path (PipeWire/Pulse knows
    // where to send it); espeak-ng is the dependency-free fallback.
    if run("spd-say", &["-w", "-l", "en", text]) {
        return;
    }
    if run("espeak-ng", &["-s", "160", text]) {
        return;
    }
    if run("espeak", &["-s", "160", text]) {
        return;
    }
    log::warn!("voice: no working TTS engine; install speech-dispatcher or espeak-ng");
}

#[cfg(windows)]
fn speak_blocking(text: &str) {
    use std::os::windows::process::CommandExt;

    // CREATE_NO_WINDOW: Stormscope is a GUI-subsystem binary on Windows;
    // without this every announcement flashes a console.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = "$s = New-Object -ComObject SAPI.SpVoice; \
                  $s.Rate = 0; $s.Volume = 100; \
                  [void]$s.Speak($env:STORMSCOPE_TTS_TEXT)";
    for exe in ["powershell.exe", "pwsh.exe"] {
        let result = std::process::Command::new(exe)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                script,
            ])
            .env("STORMSCOPE_TTS_TEXT", text)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match result {
            Ok(status) if status.success() => return,
            Ok(status) => log::debug!("voice: {exe} exited with {status}"),
            Err(e) => log::debug!("voice: {exe}: {e}"),
        }
    }
    log::warn!("voice: PowerShell SAPI voice unavailable");
}

#[cfg(target_os = "macos")]
fn speak_blocking(text: &str) {
    if !run("say", &[text]) {
        log::warn!("voice: macOS say command failed");
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn speak_blocking(text: &str) {
    let _ = text;
    log::warn!("voice: TTS is not implemented on this platform");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run(program: &str, args: &[&str]) -> bool {
    use std::process::Stdio;
    match std::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => true,
        Ok(status) => {
            log::debug!("voice: {program} exited with {status}");
            false
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            log::warn!("voice: {program}: {e}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn warning(event: &str, area: &str, instruction: &str) -> Warning {
        Warning {
            id: format!("test-{event}"),
            event: event.to_string(),
            severity: "Severe".into(),
            urgency: "Immediate".into(),
            expires: Utc::now() + Duration::minutes(30),
            office: "OUN".into(),
            headline: String::new(),
            description: String::new(),
            instruction: instruction.to_string(),
            area: area.to_string(),
            polygon: None,
        }
    }

    #[test]
    fn only_warnings_watches_and_emergencies_are_spoken() {
        assert!(announceable(&warning("Tornado Warning", "", "")));
        assert!(announceable(&warning("Severe Thunderstorm Watch", "", "")));
        assert!(announceable(&warning("Flash Flood Emergency", "", "")));
        assert!(!announceable(&warning("Wind Advisory", "", "")));
        assert!(!announceable(&warning("Special Weather Statement", "", "")));
    }

    #[test]
    fn categories_follow_the_settings_families() {
        assert_eq!(
            category(&warning("Tornado Warning", "", "")),
            Some(Category::Tornado)
        );
        assert_eq!(
            category(&warning("Tornado Watch", "", "")),
            Some(Category::Watch)
        );
        assert_eq!(
            category(&warning("Severe Thunderstorm Warning", "", "")),
            Some(Category::SevereThunderstorm)
        );
        assert_eq!(
            category(&warning("Severe Thunderstorm Watch", "", "")),
            Some(Category::Watch)
        );
        assert_eq!(
            category(&warning("Flash Flood Warning", "", "")),
            Some(Category::OtherWarning)
        );
        assert_eq!(category(&warning("Wind Advisory", "", "")), None);
    }

    #[test]
    fn view_check_uses_the_visible_map() {
        let view = MapView {
            center_lat: 35.0,
            center_lon: -97.0,
            zoom: 7.0,
            width_px: 800.0,
            height_px: 600.0,
        };
        let mut inside = warning("Tornado Warning", "", "");
        inside.polygon = Some(vec![vec![(-97.1, 35.0), (-96.9, 35.0), (-96.9, 35.1)]]);
        assert!(in_view(&inside, &view, None));

        let mut outside = warning("Tornado Warning", "", "");
        outside.polygon = Some(vec![vec![(-90.0, 40.0), (-89.9, 40.0), (-89.9, 40.1)]]);
        assert!(!in_view(&outside, &view, None));

        let zone = warning("Tornado Warning", "", "");
        assert!(in_view(&zone, &view, Some((35.0, -97.0))));
        assert!(!in_view(&zone, &view, Some((40.0, -90.0))));
    }

    #[test]
    fn warnings_outrank_watches() {
        let warn = warning("Tornado Warning", "", "");
        let watch = warning("Tornado Watch", "", "");
        assert!(rank(&warn) > rank(&watch));
        assert!(rank(&warning("Severe Thunderstorm Warning", "", "")) > rank(&watch));
    }

    #[test]
    fn announcement_is_short_and_actionable() {
        let w = warning(
            "Tornado Warning",
            "Cleveland, OK; McClain, OK; Pottawatomie, OK",
            "TAKE COVER NOW! Move to a basement.",
        );
        let text = announcement(&w);
        assert!(text.starts_with("New Tornado Warning for Cleveland, OK and McClain, OK"));
        assert!(text.contains("Expires at"));
        assert!(text.contains("TAKE COVER NOW."));
        assert!(!text.contains("Pottawatomie"));
    }
}
