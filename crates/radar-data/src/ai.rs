//! Optional AI meteorologist: a small OpenAI-compatible chat-completions
//! client.
//!
//! The default target is **OpenCode Go** — `https://opencode.ai/inference/go/v1`
//! speaks the OpenAI chat-completions shape and is reached with an
//! `OPENCODE_API_KEY`-style bearer token. Any other OpenAI-compatible base URL
//! works too (OpenAI, OpenRouter, a local Ollama/LM Studio server), so the app
//! does not bake a single vendor in: the Settings popup stores the base URL,
//! model and key.
//!
//! The brief itself (what data is sampled and how it is phrased) lives in the
//! app; this module only carries the request and unwraps the reply.

use anyhow::{Context, Result, bail};
use std::time::Duration;

/// OpenCode Go's OpenAI-compatible inference endpoint.
pub const OPENCODE_GO_BASE: &str = "https://opencode.ai/inference/go/v1";
/// A fast, inexpensive model available on OpenCode Go.
pub const OPENCODE_GO_MODEL: &str = "deepseek-v4.1-flash";
/// What OpenCode itself reads the Go key from.
pub const OPENCODE_GO_KEY_ENV: &str = "OPENCODE_API_KEY";

/// One endpoint + credentials + model.
#[derive(Debug, Clone)]
pub struct AiConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    /// Upper bound on the reply length. A brief is short; the cap keeps a
    /// runaway model from producing a wall of text (and a bill to match).
    pub max_tokens: u32,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            base_url: OPENCODE_GO_BASE.to_string(),
            model: OPENCODE_GO_MODEL.to_string(),
            api_key: String::new(),
            max_tokens: 700,
        }
    }
}

impl AiConfig {
    /// The URL to POST to. Accepts a base with or without `/v1`, or a full
    /// `.../chat/completions` URL already.
    pub fn endpoint(&self) -> String {
        let base = self.base_url.trim().trim_end_matches('/');
        if base.ends_with("/chat/completions") {
            base.to_string()
        } else if base.ends_with("/v1") {
            format!("{base}/chat/completions")
        } else {
            format!("{base}/v1/chat/completions")
        }
    }

    /// A loopback endpoint (Ollama, LM Studio) needs no key.
    pub fn is_local(&self) -> bool {
        let Ok(url) = reqwest::Url::parse(self.base_url.trim()) else {
            return false;
        };
        match url.host_str() {
            Some("localhost" | "127.0.0.1" | "::1" | "0.0.0.0") => true,
            _ => false,
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.model.trim().is_empty() && (self.is_local() || !self.api_key.trim().is_empty())
    }
}

/// Ask the model for a reply. `session` is sent as `x-opencode-session`; Go's
/// docs ask clients to identify a conversation with a stable value so routing
/// and prompt caching work.
pub fn complete(config: &AiConfig, system: &str, user: &str, session: &str) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .user_agent(concat!("stormscope/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("building the AI HTTP client")?;
    let body = serde_json::json!({
        "model": config.model.trim(),
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "temperature": 0.35,
        "max_tokens": config.max_tokens.clamp(64, 4096),
    });
    let payload_bytes = body.to_string().len();
    let mut last: Option<Failure> = None;
    for attempt in 0..4u32 {
        match send_once(&client, config, &body, session) {
            Ok(text) => {
                log::debug!(
                    "AI reply: {} bytes -> {} (prompt {} bytes, attempt {})",
                    text.len(),
                    config.model,
                    payload_bytes,
                    attempt + 1
                );
                return Ok(text);
            }
            Err(failure) => {
                log::debug!(
                    "AI attempt {} failed ({} byte prompt): {}",
                    attempt + 1,
                    payload_bytes,
                    failure.message
                );
                let will_retry = failure.retryable && attempt < 3;
                last = Some(failure);
                if !will_retry {
                    break;
                }
                // A hiccup at the provider gateway (the Go endpoint can answer
                // "Endpoint is unavailable" for one request while the next one
                // works) must not throw away the spotter's question.
                std::thread::sleep(Duration::from_millis(1_000 * (1u64 << attempt.min(2))));
            }
        }
    }
    let failure = last.expect("at least one attempt");
    bail!(
        "{}{}",
        failure.message,
        if failure.retryable {
            " (after 4 attempts)"
        } else {
            ""
        }
    )
}

struct Failure {
    message: String,
    retryable: bool,
}

/// One POST + response classification. Retryable = transport error, 429 or a
/// 5xx from the gateway; everything else is shown to the user as-is.
fn send_once(
    client: &reqwest::blocking::Client,
    config: &AiConfig,
    body: &serde_json::Value,
    session: &str,
) -> Result<String, Failure> {
    let mut request = client
        .post(config.endpoint())
        .header("x-opencode-session", session)
        .json(body);
    if !config.api_key.trim().is_empty() {
        request = request.bearer_auth(config.api_key.trim());
    }
    let response = match request.send() {
        Ok(response) => response,
        Err(error) => {
            return Err(Failure {
                message: format!("Network error: {error}"),
                retryable: true,
            });
        }
    };
    let status = response.status();
    let text = match response.text() {
        Ok(text) => text,
        Err(error) => {
            return Err(Failure {
                message: format!("Could not read the AI response: {error}"),
                retryable: true,
            });
        }
    };
    if !status.is_success() {
        let lower = text.to_ascii_lowercase();
        let retryable = status.as_u16() == 429
            || status.is_server_error()
            || lower.contains("unavailable")
            || lower.contains("overloaded")
            || lower.contains("try again");
        let detail = error_message(&text).unwrap_or_else(|| "no error body".to_string());
        return Err(Failure {
            message: format!("HTTP {} · {detail}", status.as_u16()),
            retryable,
        });
    }
    reply_text(&text).map_err(|error| Failure {
        message: format!("{error:#}"),
        retryable: false,
    })
}

/// Pull `choices[0].message.content` out of a chat-completions response.
fn reply_text(body: &str) -> Result<String> {
    let value: serde_json::Value =
        serde_json::from_str(body).context("the AI reply was not JSON")?;
    let content = value
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .trim();
    if content.is_empty() {
        bail!("the model returned an empty reply");
    }
    Ok(content.to_string())
}

/// The useful part of an OpenAI-style error body, including the provider's
/// machine code when it sent one (Go's `api_key_expired`, for example).
fn error_message(body: &str) -> Option<String> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(error) = value.get("error") {
            let message = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .trim();
            let code = error.get("code").and_then(|c| c.as_str()).unwrap_or("");
            if !message.is_empty() {
                return Some(if code.is_empty() {
                    message.to_string()
                } else {
                    format!("{message} [{code}]")
                });
            }
        }
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.chars().take(300).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_accepts_every_base_shape() {
        let mut c = AiConfig::default();
        assert_eq!(
            c.endpoint(),
            "https://opencode.ai/inference/go/v1/chat/completions"
        );
        c.base_url = "https://opencode.ai/inference/go/v1/".into();
        assert_eq!(
            c.endpoint(),
            "https://opencode.ai/inference/go/v1/chat/completions"
        );
        c.base_url = "http://localhost:11434".into();
        assert_eq!(c.endpoint(), "http://localhost:11434/v1/chat/completions");
        c.base_url = "https://example.test/v1/chat/completions".into();
        assert_eq!(c.endpoint(), "https://example.test/v1/chat/completions");
    }

    #[test]
    fn local_endpoints_do_not_need_a_key() {
        let mut c = AiConfig::default();
        assert!(!c.is_configured());
        c.base_url = "http://127.0.0.1:1234/v1".into();
        assert!(c.is_configured());
        c.base_url = "https://opencode.ai/inference/go/v1".into();
        c.api_key = "sk-test".into();
        assert!(c.is_configured());
        c.model.clear();
        assert!(!c.is_configured());
    }

    #[test]
    fn provider_errors_keep_the_message_and_code() {
        let body = r#"{"error":{"message":"This API key has expired.","type":"authentication_error","code":"api_key_expired"}}"#;
        assert_eq!(
            error_message(body).as_deref(),
            Some("This API key has expired. [api_key_expired]")
        );
        assert_eq!(
            error_message("  plain failure  ").as_deref(),
            Some("plain failure")
        );
        assert_eq!(error_message("   "), None);
    }

    #[test]
    fn replies_are_unwrapped_and_trimmed() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"  pong\n"}}]}"#;
        assert_eq!(reply_text(body).unwrap(), "pong");
        assert!(reply_text(r#"{"choices":[]}"#).is_err());
    }

    /// The whole request path against a loopback OpenAI-compatible server:
    /// proves the body, the session header, the bearer-less local mode and the
    /// reply unwrapping without touching the network.
    #[test]
    fn talks_to_a_local_openai_compatible_server() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 8192];
            let n = socket.read(&mut buf).expect("read");
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = r#"{"choices":[{"message":{"role":"assistant","content":"pong"}}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).expect("write");
            request
        });
        let config = AiConfig {
            base_url: format!("http://{addr}/v1"),
            model: "test-model".into(),
            api_key: String::new(),
            max_tokens: 32,
        };
        let reply =
            complete(&config, "system text", "user text", "session-42").expect("a local reply");
        let request = server.join().expect("server thread");
        assert_eq!(reply, "pong");
        assert!(request.contains(r#""model":"test-model""#), "{request}");
        assert!(request.contains("system text"), "{request}");
        assert!(
            request
                .to_ascii_lowercase()
                .contains("x-opencode-session: session-42"),
            "{request}"
        );
    }

    /// A 503 from the gateway is retried once and the retry's answer wins.
    #[test]
    fn retries_a_temporarily_unavailable_endpoint() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            for attempt in 0..2 {
                let (mut socket, _) = listener.accept().expect("accept");
                let mut buf = [0u8; 8192];
                let _ = socket.read(&mut buf).expect("read");
                let response = if attempt == 0 {
                    let body = r#"{"error":{"message":"Upstream request failed: Endpoint is unavailable.","code":"unavailable"}}"#;
                    format!(
                        "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else {
                    let body = r#"{"choices":[{"message":{"content":"pong"}}]}"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                };
                socket.write_all(response.as_bytes()).expect("write");
            }
        });
        let config = AiConfig {
            base_url: format!("http://{addr}/v1"),
            model: "test-model".into(),
            api_key: String::new(),
            max_tokens: 16,
        };
        let reply = complete(&config, "system", "user", "retry-test").expect("a retried reply");
        assert_eq!(reply, "pong");
        server.join().expect("server thread");
    }

    /// Real call, driven by the environment (ignored by default):
    /// `OPENCODE_API_KEY=... cargo test -p radar-data --lib ai::tests::live -- --ignored --nocapture`
    #[test]
    #[ignore = "needs OPENCODE_API_KEY and network access"]
    fn live_opencode_go_round_trip() {
        let key = std::env::var(OPENCODE_GO_KEY_ENV).expect("OPENCODE_API_KEY");
        let config = AiConfig {
            api_key: key,
            max_tokens: 16,
            ..AiConfig::default()
        };
        let reply = complete(
            &config,
            "You are a connectivity test.",
            "Reply with the single word: pong",
            "stormscope-ai-test",
        )
        .expect("a reply from OpenCode Go");
        eprintln!("OpenCode Go replied: {reply}");
        assert!(!reply.is_empty());
    }
}
