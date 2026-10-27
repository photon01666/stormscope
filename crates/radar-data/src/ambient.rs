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
            let mut st = parse_device(&d);
            // The developer API does not always include coordinates; fall
            // back to a one-time geocode of the station's location string so
            // the marker can still be placed on the map.
            if st.coords.is_none() && !st.location.is_empty() {
                st.coords = self.geocode(&st.location);
            }
            out.push(st);
        }
        Ok(out)
    }

    /// Rough coordinates for a station whose `info` has no `coords`: Open-Meteo
    /// geocoding (free, no key). Best-effort — the marker is only as precise as
    /// the location text the user entered.
    fn geocode(&self, place: &str) -> Option<AmbientCoords> {
        let attempts = [
            place.to_string(),
            place.split(',').next()?.trim().to_string(),
        ];
        for attempt in attempts {
            if attempt.is_empty() {
                continue;
            }
            let url = format!(
                "https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=en&format=json",
                urlenc(&attempt)
            );
            let Ok(resp) = self.client.get(&url).send() else {
                continue;
            };
            let Ok(v) = resp.json::<Value>() else {
                continue;
            };
            let Some(r) = v
                .get("results")
                .and_then(|r| r.as_array())
                .and_then(|a| a.first())
            else {
                continue;
            };
            if let (Some(lat), Some(lon)) = (
                r.get("latitude").and_then(|x| x.as_f64()),
                r.get("longitude").and_then(|x| x.as_f64()),
            ) {
                return Some(AmbientCoords { lat, lon });
            }
        }
        None
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

fn loose_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn parse_coords(v: &Value) -> Option<AmbientCoords> {
    // Ambient has shipped a few shapes over time: flat lat/lon, latitude/
    // longitude, a nested `coords` object (sometimes itself wrapping another
    // `coords`), or a [lat, lon] array.
    let lat = v
        .get("lat")
        .or_else(|| v.get("latitude"))
        .and_then(loose_f64);
    let lon = v
        .get("lon")
        .or_else(|| v.get("longitude"))
        .and_then(loose_f64);
    if let (Some(lat), Some(lon)) = (lat, lon) {
        return Some(AmbientCoords { lat, lon });
    }
    if let Some(arr) = v.as_array()
        && arr.len() >= 2
        && let (Some(lat), Some(lon)) = (loose_f64(&arr[0]), loose_f64(&arr[1]))
    {
        return Some(AmbientCoords { lat, lon });
    }
    if let Some(c) = v.get("coords") {
        return parse_coords(c);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flat_and_nested_coords() {
        let flat = serde_json::json!({"lat": 35.1, "lon": -97.2});
        assert!(parse_coords(&flat).is_some());

        let strings = serde_json::json!({"latitude": "35.1", "longitude": "-97.2"});
        assert!(parse_coords(&strings).is_some());

        let nested = serde_json::json!({"coords": {"coords": {"lat": 35.1, "lon": -97.2}}});
        let c = parse_coords(&nested).expect("nested coords");
        assert!((c.lat - 35.1).abs() < 1e-6 && (c.lon + 97.2).abs() < 1e-6);
    }
}
