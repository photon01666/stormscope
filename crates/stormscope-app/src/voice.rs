//! Spoken warnings/watches for the desktop and Android builds.
//!
//! Triggering is deliberately narrow: only products that are warnings or
//! watches, and only when the alert polygon is within 50 miles of the device
//! GPS fix or one of the map's manual markers. Speech is serialized on one
//! worker thread.
//!
//! On Linux/Windows, Stormscope prefers the neural Piper voices (the female
//! `en_US-lessac-high` voice by default) and falls back to the system voice
//! when Piper or a voice model is unavailable. Android uses its system TTS
//! engine through [`crate::android_tts`].

use radar_core::warnings::Warning;
#[cfg(not(target_os = "android"))]
use std::path::{Path, PathBuf};
#[cfg(not(target_os = "android"))]
use std::sync::Mutex;
use std::sync::{OnceLock, mpsc};

/// 50 statute miles in kilometres. NWS polygons are WGS84 lon/lat, so the
/// proximity check works in kilometres and formats the radius as miles in the
/// UI.
pub const ALERT_RADIUS_KM: f64 = 50.0 * 1.609_344;

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

/// One installed Piper model. `id` is the path relative to the voice root,
/// without the `.onnx` suffix (e.g. `en/en_US/lessac/high/en_US-lessac-high`).
#[cfg(not(target_os = "android"))]
#[derive(Clone, Debug)]
pub struct Voice {
    pub id: String,
    pub path: PathBuf,
    pub label: String,
}

#[cfg(not(target_os = "android"))]
static VOICES: OnceLock<Vec<Voice>> = OnceLock::new();
#[cfg(not(target_os = "android"))]
static SELECTED_VOICE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

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

/// Is this alert polygon within 50 miles of any GPS fix or manual marker?
///
/// Zone-only alerts have no geometry to measure, so they are not spoken.
/// Checking the polygon (not just its centroid) matters for large warnings:
/// a centroid can be dozens of miles from the marker even when the marker is
/// inside the warned area.
pub fn near_points(w: &Warning, points: &[(f64, f64)]) -> bool {
    let Some(rings) = w.polygon.as_deref() else {
        return false;
    };
    if rings.is_empty() || points.is_empty() {
        return false;
    }
    points.iter().any(|&(lat, lon)| {
        rings
            .iter()
            .any(|ring| distance_to_ring_km(ring, lat, lon) <= ALERT_RADIUS_KM)
    })
}

/// Approximate distance from a point to a warning ring, in kilometres.
///
/// A local equirectangular projection is accurate to well under a mile at the
/// 50-mile scale and avoids great-circle edge-distance trigonometry. If the
/// point is inside the ring, the distance is zero.
fn distance_to_ring_km(ring: &[(f64, f64)], lat0: f64, lon0: f64) -> f64 {
    if ring.is_empty() {
        return f64::INFINITY;
    }
    let km_per_deg_lat = 110.574;
    let km_per_deg_lon = 111.320 * lat0.to_radians().cos().abs().max(0.01);
    let projected: Vec<(f64, f64)> = ring
        .iter()
        .map(|&(lon, lat)| ((lon - lon0) * km_per_deg_lon, (lat - lat0) * km_per_deg_lat))
        .collect();

    // Ray-cast from the query point (0, 0). Any ring containing the point is
    // an immediate hit.
    let mut inside = false;
    for i in 0..projected.len() {
        let (x1, y1) = projected[i];
        let (x2, y2) = projected[(i + 1) % projected.len()];
        if (y1 > 0.0) != (y2 > 0.0) {
            let x = x1 + (0.0 - y1) * (x2 - x1) / (y2 - y1);
            if x > 0.0 {
                inside = !inside;
            }
        }
    }
    if inside {
        return 0.0;
    }

    let mut best = f64::INFINITY;
    for i in 0..projected.len() {
        let (x1, y1) = projected[i];
        let (x2, y2) = projected[(i + 1) % projected.len()];
        let (dx, dy) = (x2 - x1, y2 - y1);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 {
            (-(x1 * dx + y1 * dy) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let x = x1 + t * dx;
        let y = y1 + t * dy;
        best = best.min((x * x + y * y).sqrt());
    }
    best
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

// ---------------------------------------------------------------------------
// Installed Piper voices

/// Every installed English Piper voice, best candidates first.
#[cfg(not(target_os = "android"))]
pub fn voices() -> &'static [Voice] {
    VOICES.get_or_init(|| {
        std::panic::catch_unwind(scan_voices).unwrap_or_else(|_| {
            log::warn!("voice: voice discovery panicked; falling back to the system voice");
            Vec::new()
        })
    })
}

/// The voice used when the user has not picked one. Lessac high is a female
/// neural voice and the default requested for Stormscope.
#[cfg(not(target_os = "android"))]
pub fn default_voice() -> Option<&'static Voice> {
    let list = voices();
    list.iter()
        .find(|voice| voice.id.ends_with("en_US-lessac-high"))
        .or_else(|| list.iter().find(|voice| voice.label.contains("lessac")))
        .or_else(|| list.iter().find(|voice| voice.label.contains("female")))
        .or_else(|| list.first())
}

/// Change the selected Piper voice. `None` means "use the best installed
/// voice" (currently lessac high). Android uses the system TTS engine instead.
#[cfg(not(target_os = "android"))]
pub fn set_voice(id: Option<String>) {
    if let Ok(mut selected) = SELECTED_VOICE.get_or_init(|| Mutex::new(None)).lock() {
        *selected = id;
    }
}

#[cfg(not(target_os = "android"))]
fn selected_voice() -> Option<&'static Voice> {
    let list = voices();
    let selected = SELECTED_VOICE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .and_then(|selected| selected.clone());
    if let Some(id) = selected
        && let Some(voice) = list.iter().find(|voice| voice.id == id)
    {
        return Some(voice);
    }
    default_voice()
}

#[cfg(not(target_os = "android"))]
fn scan_voices() -> Vec<Voice> {
    fn add(paths: &mut Vec<(&'static str, PathBuf)>, id: &'static str, path: PathBuf) {
        paths.push((id, path));
    }

    let mut found = Vec::new();
    let mut paths: Vec<(&'static str, PathBuf)> = Vec::new();

    #[cfg(target_os = "linux")]
    {
        let mut roots = vec![
            PathBuf::from("/usr/share/piper-voices"),
            PathBuf::from("/usr/local/share/piper-voices"),
        ];
        if let Some(data) = data_home() {
            roots.push(data.join("piper-voices"));
        }
        for root in roots {
            add(
                &mut paths,
                "en_US-lessac-high",
                root.join("en/en_US/lessac/high/en_US-lessac-high.onnx"),
            );
            add(
                &mut paths,
                "en_US-hfc_female-medium",
                root.join("en/en_US/hfc_female/medium/en_US-hfc_female-medium.onnx"),
            );
            add(
                &mut paths,
                "en_US-ljspeech-high",
                root.join("en/en_US/ljspeech/high/en_US-ljspeech-high.onnx"),
            );
            add(
                &mut paths,
                "en_US-amy-medium",
                root.join("en/en_US/amy/medium/en_US-amy-medium.onnx"),
            );
            add(
                &mut paths,
                "en_US-ryan-high",
                root.join("en/en_US/ryan/high/en_US-ryan-high.onnx"),
            );
        }
    }

    #[cfg(windows)]
    {
        let mut roots = Vec::new();
        if let Ok(exe) = std::env::current_exe()
            && let Some(parent) = exe.parent()
        {
            roots.push(parent.join("piper").join("voices"));
        }
        if let Some(appdata) = std::env::var_os("APPDATA") {
            roots.push(
                PathBuf::from(appdata)
                    .join("stormscope")
                    .join("piper")
                    .join("voices"),
            );
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            roots.push(
                PathBuf::from(local)
                    .join("stormscope")
                    .join("piper")
                    .join("voices"),
            );
        }
        for root in roots {
            add(
                &mut paths,
                "en_US-lessac-high",
                root.join("en/en_US/lessac/high/en_US-lessac-high.onnx"),
            );
        }
    }

    #[cfg(target_os = "macos")]
    {
        let mut roots = vec![
            PathBuf::from("/usr/local/share/piper-voices"),
            PathBuf::from("/opt/homebrew/share/piper-voices"),
        ];
        if let Some(data) = data_home() {
            roots.push(data.join("piper-voices"));
        }
        for root in roots {
            add(
                &mut paths,
                "en_US-lessac-high",
                root.join("en/en_US/lessac/high/en_US-lessac-high.onnx"),
            );
        }
    }

    for (id, path) in paths {
        if let Some(voice) = voice_from_path(id, &path)
            && !found.iter().any(|existing: &Voice| existing.id == voice.id)
        {
            found.push(voice);
        }
    }
    found
}

/// Explicit voice locations for the bundled Windows app and the common
/// Linux/macOS Piper installs. This is deliberately not a recursive walk:
/// scanning a bundled Piper tree on the UI thread was both slow and fragile,
/// and a reparse-point loop on Windows could take the app down.
#[cfg(not(target_os = "android"))]
fn voice_from_path(id: &'static str, path: &Path) -> Option<Voice> {
    if !path.is_file() || !path.with_extension("onnx.json").is_file() {
        return None;
    }
    let stem = path.file_stem().and_then(|stem| stem.to_str())?;
    Some(Voice {
        id: id.to_string(),
        path: path.to_path_buf(),
        label: voice_label(stem),
    })
}

#[cfg(not(target_os = "android"))]
fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share"))
        })
}

#[cfg(not(target_os = "android"))]
fn voice_label(stem: &str) -> String {
    let parts: Vec<&str> = stem.split('-').collect();
    if parts.len() < 3 {
        return stem.to_string();
    }
    let quality = parts.last().copied().unwrap_or("");
    let name = parts[1..parts.len() - 1].join("-");
    let gender = if name.contains("lessac")
        || name.contains("hfc_female")
        || name.contains("ljspeech")
        || name.contains("amy")
        || name.contains("kristin")
        || name.contains("kathleen")
    {
        " · female"
    } else if name.contains("ryan")
        || name.contains("joe")
        || name.contains("john")
        || name.contains("norman")
        || name.contains("danny")
        || name.contains("bryce")
        || name.contains("mike")
        || name == "sam"
    {
        " · male"
    } else {
        ""
    };
    format!("{} · {} · {}{}", parts[0], name, quality, gender)
}

// ---------------------------------------------------------------------------
// Speech output

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

#[cfg(target_os = "android")]
fn speak_blocking(text: &str) {
    crate::android_tts::speak(text);
}

#[cfg(not(target_os = "android"))]
fn speak_blocking(text: &str) {
    if let Some(voice) = selected_voice() {
        log::info!("voice: Piper selected {}", voice.label);
        if speak_piper(text, voice) {
            return;
        }
        log::warn!(
            "voice: Piper voice {} failed; falling back to the system voice",
            voice.label
        );
    } else {
        log::warn!("voice: no Piper voice discovered; using the system voice");
    }
    speak_system(text);
}

#[cfg(not(target_os = "android"))]
fn speak_piper(text: &str, voice: &Voice) -> bool {
    let wav = temp_wav_path();
    if !synthesize_piper(text, voice, &wav) {
        let _ = std::fs::remove_file(&wav);
        return false;
    }
    let bytes = wav.metadata().map(|meta| meta.len()).unwrap_or(0);
    let played = play_wav(&wav);
    log::info!(
        "voice: Piper {} wav {} bytes, playback {}",
        voice.label,
        bytes,
        played
    );
    let _ = std::fs::remove_file(&wav);
    played
}

#[cfg(not(target_os = "android"))]
fn temp_wav_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "stormscope-voice-{}-{nanos}.wav",
        std::process::id()
    ))
}

#[cfg(not(target_os = "android"))]
fn synthesize_piper(text: &str, voice: &Voice, wav: &Path) -> bool {
    if !voice.path.is_file() {
        return false;
    }
    for program in piper_binaries() {
        let mut command = std::process::Command::new(&program);
        // piper-tts/python and the Unix builds accept -q; the Windows
        // piper.exe build does not list it.
        if !cfg!(windows) {
            command.arg("-q");
        }
        // Pass the text as an argument, not on stdin: the Python piper-tts
        // wrapper only synthesizes a short fragment when text arrives on a
        // pipe (heard as a "pew"), while an argument produces the whole
        // sentence. Announcements are well below the Windows command line
        // limit.
        command
            .arg("-m")
            .arg(&voice.path)
            .arg("-f")
            .arg(wav)
            .arg(text)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // A packaged Windows Piper needs its own directory as the working
        // directory so it finds espeak-ng-data and its DLLs.
        if let Some(parent) = Path::new(&program).parent()
            && !parent.as_os_str().is_empty()
        {
            command.current_dir(parent);
        }
        match command.status() {
            Ok(status) if status.success() && wav.is_file() => return true,
            Ok(status) => log::debug!("voice: {program:?} exited with {status}"),
            Err(e) => log::debug!("voice: {program:?}: {e}"),
        }
    }
    false
}

#[cfg(not(target_os = "android"))]
fn piper_binaries() -> Vec<std::ffi::OsString> {
    let mut binaries = Vec::new();
    #[cfg(target_os = "linux")]
    {
        binaries.push("piper-tts".into());
        binaries.push("piper".into());
    }
    #[cfg(target_os = "macos")]
    {
        binaries.push("piper".into());
    }
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            binaries.push(
                PathBuf::from(appdata)
                    .join("stormscope")
                    .join("piper")
                    .join("piper.exe")
                    .into_os_string(),
            );
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            binaries.push(
                PathBuf::from(local)
                    .join("stormscope")
                    .join("piper")
                    .join("piper.exe")
                    .into_os_string(),
            );
        }
        if let Ok(exe) = std::env::current_exe()
            && let Some(parent) = exe.parent()
        {
            binaries.push(parent.join("piper").join("piper.exe").into_os_string());
        }
        binaries.push("piper.exe".into());
    }
    binaries
}

#[cfg(target_os = "linux")]
fn play_wav(path: &Path) -> bool {
    for program in ["pw-play", "paplay", "aplay"] {
        let mut command = std::process::Command::new(program);
        if program == "aplay" {
            command.arg("-q");
        }
        command.arg(path);
        if run_command(command) {
            return true;
        }
    }
    false
}

#[cfg(windows)]
fn play_wav(path: &Path) -> bool {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = "$p = New-Object Media.SoundPlayer($env:STORMSCOPE_TTS_WAV); $p.PlaySync()";
    for exe in ["powershell.exe", "pwsh.exe"] {
        let mut command = std::process::Command::new(exe);
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                script,
            ])
            .env("STORMSCOPE_TTS_WAV", path)
            .creation_flags(CREATE_NO_WINDOW);
        if run_command(command) {
            return true;
        }
    }
    false
}

#[cfg(target_os = "macos")]
fn play_wav(path: &Path) -> bool {
    let mut command = std::process::Command::new("afplay");
    command.arg(path);
    run_command(command)
}

#[cfg(not(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    windows
)))]
fn play_wav(_path: &Path) -> bool {
    false
}

#[cfg(not(target_os = "android"))]
fn run_command(mut command: std::process::Command) -> bool {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn speak_system(text: &str) {
    // espeak-ng and espeak block until the audio is done; speech-dispatcher's
    // "success" can be silent if its configured output module is broken, so
    // it is the last fallback.
    if run("espeak-ng", &["-s", "160", text]) {
        return;
    }
    if run("espeak", &["-s", "160", text]) {
        return;
    }
    if run("spd-say", &["-w", "-l", "en", text]) {
        return;
    }
    log::warn!("voice: no working TTS engine; install speech-dispatcher or espeak-ng");
}

#[cfg(windows)]
fn speak_system(text: &str) {
    use std::os::windows::process::CommandExt;

    // CREATE_NO_WINDOW: Stormscope is a GUI-subsystem binary on Windows;
    // without this every announcement flashes a console.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = "$s = New-Object -ComObject SAPI.SpVoice; \
                  $s.Rate = 0; $s.Volume = 100; \
                  [void]$s.Speak($env:STORMSCOPE_TTS_TEXT)";
    for exe in ["powershell.exe", "pwsh.exe"] {
        let mut command = std::process::Command::new(exe);
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                script,
            ])
            .env("STORMSCOPE_TTS_TEXT", text)
            .creation_flags(CREATE_NO_WINDOW);
        if run_command(command) {
            return;
        }
    }
    log::warn!("voice: PowerShell SAPI voice unavailable");
}

#[cfg(target_os = "macos")]
fn speak_system(text: &str) {
    if !run("say", &[text]) {
        log::warn!("voice: macOS say command failed");
    }
}

#[cfg(not(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    windows
)))]
fn speak_system(text: &str) {
    let _ = text;
    log::warn!("voice: system TTS is not implemented on this platform");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run(program: &str, args: &[&str]) -> bool {
    let mut command = std::process::Command::new(program);
    command.args(args);
    run_command(command)
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

    fn polygon_warning(lat: f64, lon: f64) -> Warning {
        let mut w = warning("Tornado Warning", "", "");
        w.polygon = Some(vec![vec![
            (lon - 0.02, lat - 0.02),
            (lon + 0.02, lat - 0.02),
            (lon + 0.02, lat + 0.02),
            (lon - 0.02, lat + 0.02),
        ]]);
        w
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
    fn proximity_is_a_polygon_distance_not_a_centroid() {
        // The point is inside the warning.
        assert!(near_points(&polygon_warning(35.0, -97.0), &[(35.0, -97.0)]));
        // Roughly 34 miles north: inside the 50-mile radius.
        assert!(near_points(&polygon_warning(35.5, -97.0), &[(35.0, -97.0)]));
        // Roughly 55 miles north: outside.
        assert!(!near_points(
            &polygon_warning(35.8, -97.0),
            &[(35.0, -97.0)]
        ));
        // Zone-only alerts cannot be measured.
        assert!(!near_points(
            &warning("Tornado Warning", "", ""),
            &[(35.0, -97.0)]
        ));
        // A second marker can make the difference.
        assert!(near_points(
            &polygon_warning(35.8, -97.0),
            &[(35.0, -97.0), (35.8, -97.0)]
        ));
    }

    #[test]
    fn voice_labels_name_the_family_and_quality() {
        assert_eq!(
            voice_label("en_US-lessac-high"),
            "en_US · lessac · high · female"
        );
        assert_eq!(
            voice_label("en_US-ryan-medium"),
            "en_US · ryan · medium · male"
        );
    }

    #[test]
    #[ignore = "requires Piper and an installed English voice"]
    fn local_piper_synthesizes_a_wav() {
        let voice = default_voice().expect("at least one Piper voice is installed");
        assert!(voice.id.ends_with("en_US-lessac-high"));
        assert!(voice.label.contains("female"));
        let wav = temp_wav_path();
        assert!(synthesize_piper(
            "Stormscope voice alerts are working.",
            voice,
            &wav
        ));
        // A full sentence is ~100 KB at 22.05 kHz/16-bit; the old stdin path
        // produced a ~20 KB fragment.
        assert!(
            wav.metadata()
                .map(|meta| meta.len() > 50_000)
                .unwrap_or(false)
        );
        let _ = std::fs::remove_file(&wav);
    }

    #[test]
    #[ignore = "plays the local Piper voice through the system audio"]
    fn local_piper_plays_audio() {
        let voice = default_voice().expect("at least one Piper voice is installed");
        assert!(speak_piper(
            "Stormscope voice test. This is lessac high.",
            voice
        ));
    }

    #[test]
    #[ignore = "plays the queued announcement path used by Test voice"]
    fn local_announce_queue_plays() {
        set_voice(Some("en/en_US/lessac/high/en_US-lessac-high".into()));
        announce("Stormscope voice alerts are working.");
        std::thread::sleep(std::time::Duration::from_secs(6));
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
