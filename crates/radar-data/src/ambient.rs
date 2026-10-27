//! Ambient Weather PWS (personal weather station) data via the Ambient Weather
//! REST API: https://ambientweather.docs.apiary.io/
//!
//! One GET to `/v1/stations` returns every device on the account together
//! with its most recent observation (`lastData`), so a single poll keeps the
//! map's station markers and their popups fresh. The API requires two keys:
//! the account `apiKey` and an `applicationKey` created in the user's profile.

use anyhow::{Context, Result};
use serde_json::Value;

const STATIONS_URL: &str = "https://api.ambientweather.net/v1/stations";

#[derive(Debug, Clone, Default)]
pub struct AmbientCoords {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone)]
pub struct AmbientStation {
    pub mac_address: String,
    pub name: String,
    pub location: String,
    pub coords: Option<AmbientCoords>,
    /// The most recent observation, as returned by the API. Field names follow
    /// the Ambient Weather `lastData` schema (tempf, humidity, windspeedmph,
    /// winddir, baromrelin, dailyrainin, ...).
    pub last_data: Value,
}

impl AmbientStation {
    /// Human-friendly label for the marker / popup title.
    pub fn label(&self) -> String {
        if !self.name.is_empty() {
            self.name.clone()
        } else if !self.location.is_empty() {
            self.location.clone()
        } else {
            self.mac_address.clone()
        }
    }
}

pub struct AmbientSource {
    client: reqwest::blocking::Client,
    api_key: String,
    application_key: String,
}

impl AmbientSource {
    pub fn new(user_agent: &str, api_key: &str, application_key: &str) -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(user_agent)
            .timeout(std::time::Duration::from_secs(20))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(Self {
            client,
            api_key: api_key.trim().to_string(),
            application_key: application_key.trim().to_string(),
        })
    }

    pub fn has_keys(&self) -> bool {
        !self.api_key.is_empty() && !self.application_key.is_empty()
    }

    /// Fetch every station on the account together with its latest observation.
    pub fn stations(&self) -> Result<Vec<AmbientStation>> {
        let url = format!(
            "{STATIONS_URL}?apiKey={}&applicationKey={}",
            urlenc(&self.api_key),
            urlenc(&self.application_key)
        );
        let resp = self
            .client
            .get(&url)
            .send()
            .context("requesting ambient stations")?;
        let status = resp.status();
        let text = resp.text().context("reading ambient response")?;
        if !status.is_success() {
            anyhow::bail!("ambient stations HTTP {}: {}", status, text);
        }
        let arr: Value = serde_json::from_str(&text).context("parsing ambient stations")?;
        let items = arr.as_array().cloned().unwrap_or_default();
        let mut out = Vec::with_capacity(items.len());
        for d in items {
            out.push(parse_device(&d));
        }
        Ok(out)
    }
}

fn urlenc(s: &str) -> String {
    // Minimal percent-encoding for query values.
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn parse_device(d: &Value) -> AmbientStation {
    let mac = d
        .get("macAddress")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let info = d.get("info").cloned().unwrap_or(Value::Null);
    let name = info
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let location = info
        .get("location")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let coords = parse_coords(&info);
    let last_data = d.get("lastData").cloned().unwrap_or(Value::Null);
    AmbientStation {
        mac_address: mac,
        name,
        location,
        coords,
        last_data,
    }
}

fn parse_coords(info: &Value) -> Option<AmbientCoords> {
    // Try several known shapes.
    if let (Some(lat), Some(lon)) = (
        info.get("lat").and_then(|v| v.as_f64()),
        info.get("lon").and_then(|v| v.as_f64()),
    ) {
        return Some(AmbientCoords { lat, lon });
    }
    if let (Some(lat), Some(lon)) = (
        info.get("latitude").and_then(|v| v.as_f64()),
        info.get("longitude").and_then(|v| v.as_f64()),
    ) {
        return Some(AmbientCoords { lat, lon });
    }
    if let Some(c) = info.get("coords") {
        if let (Some(lat), Some(lon)) = (
            c.get("lat").and_then(|v| v.as_f64()),
            c.get("lon").and_then(|v| v.as_f64()),
        ) {
            return Some(AmbientCoords { lat, lon });
        }
        if let (Some(lat), Some(lon)) = (
            c.get("latitude").and_then(|v| v.as_f64()),
            c.get("longitude").and_then(|v| v.as_f64()),
        ) {
            return Some(AmbientCoords { lat, lon });
        }
        if let Some(arr) = c.as_array()
            && arr.len() >= 2
            && let (Some(lat), Some(lon)) = (arr[0].as_f64(), arr[1].as_f64())
        {
            return Some(AmbientCoords { lat, lon });
        }
    }
    None
}
