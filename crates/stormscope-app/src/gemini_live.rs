//! Gemini Live voice conversations (Android).
//!
//! One WebSocket session streams 16 kHz PCM16 from the microphone to Gemini
//! and plays the model's 24 kHz PCM16 answer as it arrives. The Live model is
//! native speech-to-speech: it handles turn detection and barge-in, so there
//! is no Android SpeechRecognizer or text-to-speech step in this path.
//!
//! The full weather snapshot is embedded in the setup's system instruction,
//! giving the voice model the same data the typed AI request uses.

#![cfg(target_os = "android")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

const WS_BASE: &str = "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.\
     GenerativeService.BidiGenerateContent";

/// Everything the session thread needs to open and configure the socket.
#[derive(Debug, Clone)]
pub struct LiveConfig {
    pub api_key: String,
    pub model: String,
    pub voice: String,
    pub system: String,
}

#[derive(Debug, Clone)]
pub enum LiveCommand {
    ToolResponse {
        id: String,
        name: String,
        result: Value,
    },
}

/// UI-facing session events, sent from the session thread.
#[derive(Debug, Clone)]
pub enum LiveEvent {
    /// A status line for the voice footer.
    Status(String),
    /// Setup completed; the mic is streaming and the model can hear.
    Listening,
    /// Interim transcript of the user's speech.
    UserPartial(String),
    /// Final transcript segment of the user's speech.
    UserFinal(String),
    /// A segment of the model's spoken answer (transcription).
    AssistantPartial(String),
    /// The user talked over the model; playback was flushed.
    Interrupted,
    /// The model finished its turn.
    TurnComplete,
    /// The model wants to call a whitelisted tool. The app must answer with
    /// `LiveSession::respond_tool` using the same `id`.
    ToolCall {
        id: String,
        name: String,
        args: Value,
    },
    /// A fatal problem (connection, key, microphone).
    Error(String),
    /// The session thread exited (normal stop or server close).
    Ended,
}

/// Handle to one live session thread.
pub struct LiveSession {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    cmd_tx: Sender<LiveCommand>,
}

impl LiveSession {
    pub fn start(config: LiveConfig, tx: Sender<LiveEvent>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let cmd_tx_thread = cmd_tx.clone();
        let join = std::thread::Builder::new()
            .name("gemini-live".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(error) => {
                        let _ = tx.send(LiveEvent::Error(format!(
                            "Could not start the Gemini Live runtime: {error}"
                        )));
                        let _ = tx.send(LiveEvent::Ended);
                        return;
                    }
                };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    rt.block_on(run(config, &tx, cmd_rx, &stop_thread))
                }));
                // The audio engine is shared Java state; always release it,
                // even when setup or the WebSocket failed.
                crate::android_live_audio::stop();
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        if !stop_thread.load(Ordering::Relaxed) {
                            log::warn!("Gemini Live: {error}");
                            let _ = tx.send(LiveEvent::Error(error));
                        }
                    }
                    Err(_) => {
                        if !stop_thread.load(Ordering::Relaxed) {
                            let _ = tx.send(LiveEvent::Error(
                                "Gemini Live stopped unexpectedly. Tap Talk to try again.".into(),
                            ));
                        }
                    }
                }
                let _ = tx.send(LiveEvent::Ended);
            })
            .ok();
        Self {
            stop,
            join,
            cmd_tx: cmd_tx_thread,
        }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }

    pub fn respond_tool(&self, id: String, name: String, result: Value) {
        let _ = self
            .cmd_tx
            .send(LiveCommand::ToolResponse { id, name, result });
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn run(
    config: LiveConfig,
    tx: &Sender<LiveEvent>,
    cmd_rx: std::sync::mpsc::Receiver<LiveCommand>,
    stop: &AtomicBool,
) -> Result<(), String> {
    crate::android_live_audio::start();
    let _ = tx.send(LiveEvent::Status("Connecting to the microphone…".into()));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let state = crate::android_live_audio::state();
        if state == "ready" {
            break;
        }
        if let Some(error) = state.strip_prefix("error:") {
            return Err(format!("Microphone unavailable: {error}"));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(
                "Timed out waiting for microphone permission. Allow the microphone in \
                 Android Settings, then tap Talk again."
                    .into(),
            );
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
    }

    // The dependency graph enables both rustls crypto providers (reqwest
    // pulls ring and aws-lc-rs), so rustls refuses to pick one implicitly.
    // Install ring before the WebSocket's TLS connector is built.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let url = format!("{WS_BASE}?key={}", url_encode(&config.api_key));
    let _ = tx.send(LiveEvent::Status("Connecting to Gemini Live…".into()));
    let connect = tokio::time::timeout(
        Duration::from_secs(20),
        tokio_tungstenite::connect_async(&url),
    );
    let mut socket = tokio::select! {
        _ = wait_stop(stop) => return Ok(()),
        result = connect => match result {
            Ok(Ok((socket, _response))) => socket,
            Ok(Err(error)) => return Err(connection_error(error)),
            Err(_) => return Err("Timed out connecting to Gemini Live.".into()),
        }
    };

    let model = config
        .model
        .trim()
        .trim_start_matches("models/")
        .to_string();
    let voice = if config.voice.trim().is_empty() {
        "Aoede"
    } else {
        config.voice.trim()
    };
    let mut setup = json!({
        "setup": {
            "model": format!("models/{model}"),
            "generationConfig": {
                "responseModalities": ["AUDIO"],
                "speechConfig": {
                    "languageCode": "en-US",
                    "voiceConfig": {
                        "prebuiltVoiceConfig": { "voiceName": voice }
                    }
                }
            },
            "systemInstruction": {
                "parts": [{ "text": config.system }]
            },
            "tools": [
                {
                    "functionDeclarations": [
                        {
                            "name": "move_view",
                            "description": "Move the radar/map view, switch product, open or close the 3D pane, change tilt or view angles, and mark features. Use to show the spotter something on the map.",
                            "parameters": {
                                "type": "object",
                                "properties": {
                                    "center_lat": { "type": "number" },
                                    "center_lon": { "type": "number" },
                                    "zoom": { "type": "number" },
                                    "product": { "type": "string" },
                                    "tilt_deg": { "type": "number" },
                                    "pane": { "type": "string" },
                                    "view3d_yaw": { "type": "number" },
                                    "view3d_pitch": { "type": "number" },
                                    "view3d_dist_km": { "type": "number" },
                                    "view3d_ve": { "type": "number" },
                                    "section": { "type": "boolean" },
                                    "section_az": { "type": "number" },
                                    "show_melting_layer": { "type": "boolean" },
                                    "annotations": {
                                        "type": "array",
                                        "items": {
                                            "type": "object",
                                            "properties": {
                                                "lat": { "type": "number" },
                                                "lon": { "type": "number" },
                                                "radius_mi": { "type": "number" },
                                                "alt_kft": { "type": "number" },
                                                "label": { "type": "string" },
                                                "kind": { "type": "string" }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                        {
                            "name": "look",
                            "description": "Inspect up to three other locations: move the map there and sample the radar for those areas. Returns an updated data summary.",
                            "parameters": {
                                "type": "object",
                                "properties": {
                                    "points": {
                                        "type": "array",
                                        "items": {
                                            "type": "object",
                                            "properties": {
                                                "lat": { "type": "number" },
                                                "lon": { "type": "number" },
                                                "label": { "type": "string" },
                                                "radius_mi": { "type": "number" }
                                            },
                                            "required": ["lat", "lon"]
                                        }
                                    },
                                    "product": { "type": "string" }
                                },
                                "required": ["points"]
                            }
                        },
                        {
                            "name": "fetch_data",
                            "description": "Fetch additional weather data (MRMS, GOES, NWS forecast, station totals, SPC outlook) and update the snapshot.",
                            "parameters": {
                                "type": "object",
                                "properties": {
                                    "mrms": { "type": "boolean" },
                                    "goes": { "type": "boolean" },
                                    "forecast": {
                                        "type": "object",
                                        "properties": {
                                            "lat": { "type": "number" },
                                            "lon": { "type": "number" },
                                            "label": { "type": "string" }
                                        },
                                        "required": ["lat", "lon"]
                                    },
                                    "totals": {
                                        "type": "object",
                                        "properties": {
                                            "lat": { "type": "number" },
                                            "lon": { "type": "number" },
                                            "label": { "type": "string" }
                                        },
                                        "required": ["lat", "lon"]
                                    },
                                    "spc": {
                                        "type": "object",
                                        "properties": {
                                            "day": { "type": "integer" },
                                            "kind": { "type": "string" }
                                        }
                                    }
                                }
                            }
                        }
                    ]
                }
            ],
            "realtimeInputConfig": {
                "automaticActivityDetection": {
                    "startOfSpeechSensitivity": "START_SENSITIVITY_HIGH",
                    "endOfSpeechSensitivity": "END_SENSITIVITY_HIGH",
                    "prefixPaddingMs": 250,
                    "silenceDurationMs": 450
                },
                "activityHandling": "START_OF_ACTIVITY_INTERRUPTS"
            },
            "inputAudioTranscription": { "languageCodes": ["en-US"] },
            "outputAudioTranscription": { "languageCodes": ["en-US"] },
            "contextWindowCompression": { "slidingWindow": {} }
        }
    });
    // The extended-thinking Live model rejects a setup that omits a thinking
    // level; the base Live model rejects a thinkingConfig it does not support.
    if model.contains("extended-thinking") {
        setup["setup"]["generationConfig"]["thinkingConfig"] = json!({ "thinkingLevel": "high" });
    }
    socket
        .send(Message::Text(setup.to_string().into()))
        .await
        .map_err(|error| format!("Could not configure Gemini Live: {error}"))?;

    // Wait for the server's setup acknowledgement before streaming audio.
    let setup_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let remaining = setup_deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err("Gemini Live did not acknowledge the session setup.".into());
        }
        let next = tokio::select! {
            _ = wait_stop(stop) => return Ok(()),
            next = tokio::time::timeout(remaining, socket.next()) => next,
        };
        match next {
            Err(_) => return Err("Gemini Live did not acknowledge the session setup.".into()),
            Ok(None) => return Err("Gemini closed the connection during setup.".into()),
            Ok(Some(Err(error))) => {
                return Err(friendly_connection_error(error.to_string()));
            }
            Ok(Some(Ok(message))) => {
                let Some(text) = text_of(message) else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if value.get("setupComplete").is_some() {
                    break;
                }
                if let Some(error) = server_error(&value) {
                    return Err(error);
                }
            }
        }
    }

    crate::android_live_audio::flush_mic();
    let _ = tx.send(LiveEvent::Listening);
    let mut audio_tick = tokio::time::interval(Duration::from_millis(50));
    audio_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut stop_tick = tokio::time::interval(Duration::from_millis(100));

    loop {
        tokio::select! {
            _ = stop_tick.tick() => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
            }
            incoming = socket.next() => {
                match incoming {
                    None => break,
                    Some(Err(error)) => {
                        // Mid-session transport errors are not actionable for
                        // the user (TCP reset, VPN hand-off, server recycle).
                        // Keep the detail in logcat and show a plain ending.
                        log::warn!("Gemini Live connection lost: {error}");
                        return Err(
                            "Live AI session ended. Tap Talk to start a new one.".into()
                        );
                    }
                    Some(Ok(Message::Close(_))) => break,
                    Some(Ok(message)) => {
                        if let Some(text) = text_of(message) {
                            let value: Option<Value> = serde_json::from_str(&text).ok();
                            if value
                                .as_ref()
                                .and_then(|v| v.get("toolCall"))
                                .is_some()
                            {
                                if let Some(calls) = value
                                    .as_ref()
                                    .and_then(|v| v.pointer("/toolCall/functionCalls"))
                                    .and_then(Value::as_array)
                                {
                                    for call in calls {
                                        let id = call
                                            .get("id")
                                            .and_then(Value::as_str)
                                            .unwrap_or("")
                                            .to_string();
                                        let name = call
                                            .get("name")
                                            .and_then(Value::as_str)
                                            .unwrap_or("")
                                            .to_string();
                                        let args = call
                                            .get("args")
                                            .cloned()
                                            .unwrap_or_else(|| json!({}));
                                        let _ = tx.send(LiveEvent::ToolCall {
                                            id: id.clone(),
                                            name: name.clone(),
                                            args,
                                        });
                                        let result = wait_for_tool_response(&cmd_rx, stop, &id);
                                        let response = json!({
                                            "toolResponse": {
                                                "functionResponses": [
                                                    {
                                                        "id": id,
                                                        "name": name,
                                                        "response": {
                                                            "result": result.unwrap_or_else(|| json!({"error": "no response"}))
                                                        }
                                                    }
                                                ]
                                            }
                                        });
                                        if let Err(error) = socket
                                            .send(Message::Text(response.to_string().into()))
                                            .await
                                        {
                                            return Err(format!("Could not send tool response: {error}"));
                                        }
                                    }
                                }
                            } else {
                                handle_server_message(&text, tx);
                            }
                        }
                    }
                }
            }
            _ = audio_tick.tick() => {
                while let Some(chunk) = crate::android_live_audio::read_mic() {
                    if chunk.is_empty() {
                        continue;
                    }
                    let message = json!({
                        "realtimeInput": {
                            "audio": {
                                "data": base64::engine::general_purpose::STANDARD.encode(&chunk),
                                "mimeType": "audio/pcm;rate=16000"
                            }
                        }
                    });
                    if let Err(error) = socket.send(Message::Text(message.to_string().into())).await {
                        return Err(format!("Could not send microphone audio: {error}"));
                    }
                }
            }
        }
    }

    let _ = socket.close(None).await;
    Ok(())
}

fn handle_server_message(text: &str, tx: &Sender<LiveEvent>) {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return;
    };
    if value.get("setupComplete").is_some() {
        let _ = tx.send(LiveEvent::Listening);
        return;
    }
    if let Some(error) = server_error(&value) {
        let _ = tx.send(LiveEvent::Error(error));
        return;
    }
    if value.get("goAway").is_some() {
        let _ = tx.send(LiveEvent::Status(
            "Gemini is closing this session — tap End, then Talk for a fresh snapshot.".into(),
        ));
    }
    let Some(content) = value.get("serverContent") else {
        return;
    };

    if content
        .get("interrupted")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        crate::android_live_audio::flush_playback();
        let _ = tx.send(LiveEvent::Interrupted);
    }
    if let Some(text) = content
        .pointer("/inputTranscription/text")
        .and_then(Value::as_str)
        && !text.trim().is_empty()
    {
        let _ = tx.send(LiveEvent::UserFinal(text.to_string()));
    }
    if let Some(text) = content
        .pointer("/interimInputTranscription/text")
        .and_then(Value::as_str)
        && !text.trim().is_empty()
    {
        let _ = tx.send(LiveEvent::UserPartial(text.to_string()));
    }
    if let Some(text) = content
        .pointer("/outputTranscription/text")
        .and_then(Value::as_str)
        && !text.trim().is_empty()
    {
        let _ = tx.send(LiveEvent::AssistantPartial(text.to_string()));
    }
    if let Some(parts) = content
        .pointer("/modelTurn/parts")
        .and_then(Value::as_array)
    {
        for part in parts {
            let Some(data) = part.pointer("/inlineData/data").and_then(Value::as_str) else {
                continue;
            };
            let Ok(pcm) = base64::engine::general_purpose::STANDARD.decode(data) else {
                continue;
            };
            if !pcm.is_empty() {
                crate::android_live_audio::play(&pcm);
            }
        }
    }
    if content
        .get("turnComplete")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let _ = tx.send(LiveEvent::TurnComplete);
    }
}

fn wait_for_tool_response(
    cmd_rx: &std::sync::mpsc::Receiver<LiveCommand>,
    stop: &AtomicBool,
    id: &str,
) -> Option<Value> {
    loop {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        match cmd_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(LiveCommand::ToolResponse {
                id: rid, result, ..
            }) if rid == id => return Some(result),
            Ok(_) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return None,
        }
    }
}

fn text_of(message: Message) -> Option<String> {
    match message {
        Message::Text(text) => Some(text.to_string()),
        Message::Binary(bytes) => String::from_utf8(bytes.to_vec()).ok(),
        _ => None,
    }
}

fn server_error(value: &Value) -> Option<String> {
    let error = value.get("error")?;
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .unwrap_or("unknown error");
    Some(format!("Gemini Live: {message}"))
}

/// The rustls/tungstenite EOF during setup usually means the server rejected
/// the API key or model; do not show a rustls documentation URL in the chat.
fn friendly_connection_error(error: String) -> String {
    if error.contains("close_notify")
        || error.contains("ResetWithoutClosingHandshake")
        || error.contains("peer closed connection")
    {
        "Gemini closed the connection during setup. Check the API key and model in          Settings → AI meteorologist → Gemini Live."
            .into()
    } else {
        format!("Gemini Live connection failed: {error}")
    }
}

fn connection_error(error: tokio_tungstenite::tungstenite::Error) -> String {
    use tokio_tungstenite::tungstenite::Error;
    match error {
        Error::Http(response) => {
            let status = response.status();
            let hint = match status.as_u16() {
                401 | 403 => " Check the Gemini API key in Settings.",
                404 => " Check the Gemini model name in Settings.",
                _ => "",
            };
            format!("Gemini rejected the connection (HTTP {status}).{hint}")
        }
        other => friendly_connection_error(format!("Could not reach Gemini Live: {other}")),
    }
}

async fn wait_stop(stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Minimal percent-encoding for the API key query parameter. API keys are
/// alphanumeric with `-`/`_`; this keeps anything unexpected safe.
fn url_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
