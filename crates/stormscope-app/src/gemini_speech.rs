//! One-shot Gemini Live speech for desktop storm alerts.
//!
//! The Live model is used because it accepts the configured Gemini voice and
//! returns the same 24 kHz PCM16 stream the Android path already consumes.
//! Audio is collected into a local WAV and played by [`crate::voice`]; if any
//! step fails, the caller falls back to Piper.

#![cfg(not(target_os = "android"))]

use std::time::Duration;

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

const WS_BASE: &str = "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.\
     GenerativeService.BidiGenerateContent";

#[derive(Clone, Debug)]
pub struct Config {
    pub api_key: String,
    pub model: String,
    pub voice: String,
}

/// Speak one alert. Returns false on any Gemini failure so the caller can
/// fall back to Piper; never panics.
pub fn speak(config: &Config, text: &str) -> bool {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(error) => {
            log::warn!("Gemini speech runtime failed: {error}");
            return false;
        }
    };
    match rt.block_on(run(config, text)) {
        Ok(()) => true,
        Err(error) => {
            log::warn!("Gemini speech failed: {error}; falling back to Piper");
            false
        }
    }
}

async fn run(config: &Config, text: &str) -> Result<(), String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let url = format!("{WS_BASE}?key={}", url_encode(&config.api_key));
    let connect = tokio::time::timeout(
        Duration::from_secs(20),
        tokio_tungstenite::connect_async(&url),
    );
    let (mut socket, _) = match connect.await {
        Ok(Ok(pair)) => pair,
        Ok(Err(error)) => return Err(connection_error(error)),
        Err(_) => return Err("timed out connecting to Gemini Live".into()),
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
                "parts": [{ "text": "You are Stormscope's spoken storm alert. Read the supplied alert exactly once, clearly and at a natural pace. Do not greet, explain, add commentary, or ask questions. Use US English." }]
            }
        }
    });
    if model.contains("extended-thinking") {
        setup["setup"]["generationConfig"]["thinkingConfig"] = json!({ "thinkingLevel": "high" });
    }
    socket
        .send(Message::Text(setup.to_string().into()))
        .await
        .map_err(|error| format!("could not configure the alert voice: {error}"))?;

    let ack_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let remaining = ack_deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err("Gemini did not acknowledge the alert session".into());
        }
        let message = match tokio::time::timeout(remaining, socket.next()).await {
            Ok(Some(Ok(message))) => message,
            Ok(Some(Err(error))) => return Err(format!("alert setup lost: {error}")),
            Ok(None) | Err(_) => {
                return Err("Gemini did not acknowledge the alert session".into());
            }
        };
        let Some(raw) = text_of(message) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if value.get("setupComplete").is_some() {
            break;
        }
        if let Some(error) = server_error(&value) {
            return Err(error);
        }
    }

    let prompt =
        format!("Say exactly this alert once, with no greeting and no extra words: {text}");
    let turn = json!({
        "clientContent": {
            "turns": [{ "role": "user", "parts": [{ "text": prompt }] }],
            "turnComplete": true
        }
    });
    socket
        .send(Message::Text(turn.to_string().into()))
        .await
        .map_err(|error| format!("could not send the alert text: {error}"))?;

    let mut audio = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err("Gemini alert timed out".into());
        }
        let message = match tokio::time::timeout(remaining, socket.next()).await {
            Ok(Some(Ok(message))) => message,
            Ok(Some(Err(error))) => return Err(format!("alert connection lost: {error}")),
            Ok(None) | Err(_) => return Err("Gemini alert closed early".into()),
        };
        let Some(raw) = text_of(message) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if let Some(error) = server_error(&value) {
            return Err(error);
        }
        let Some(content) = value.get("serverContent") else {
            continue;
        };
        if let Some(parts) = content
            .pointer("/modelTurn/parts")
            .and_then(Value::as_array)
        {
            for part in parts {
                let Some(data) = part.pointer("/inlineData/data").and_then(Value::as_str) else {
                    continue;
                };
                if let Ok(pcm) = base64::engine::general_purpose::STANDARD.decode(data) {
                    audio.extend_from_slice(&pcm);
                }
            }
        }
        if content
            .get("turnComplete")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            break;
        }
    }
    let _ = socket.close(None).await;
    if audio.is_empty() {
        return Err("Gemini returned no audio".into());
    }
    if !crate::voice::play_pcm16_wav(&audio, 24_000) {
        return Err("could not play the Gemini audio".into());
    }
    Ok(())
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

fn connection_error(error: tokio_tungstenite::tungstenite::Error) -> String {
    use tokio_tungstenite::tungstenite::Error;
    match error {
        Error::Http(response) => format!(
            "Gemini rejected the connection (HTTP {})",
            response.status()
        ),
        other => format!("could not reach Gemini Live: {other}"),
    }
}

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
