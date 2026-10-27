//! Synoptic (Mesowest) station observations: https://api.synopticdata.com/v2
//!
//! Aggregates thousands of networks (NWS ASOS/AWOS, state mesonets and many
//! personal-weather-station networks, including Ambient Weather) behind one
//! token. A single `stations/latest` request with a bounding box returns both
//! the station metadata and the latest observation for every station in view.

use anyhow::{Context, Result};
use serde_json::Value;

const LATEST_URL: &str = "https://api.synopticdata.com/v2/stations/latest";

#[derive(Debug, Clone, Default)]
pub struct SynopticCoords {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone)]
pub struct SynopticStation {
    pub stid: String,
    pub name: String,
    pub network: String,
    pub coords: Option<SynopticCoords>,
    pub elevation_m: Option<f64>,
    /// The latest observation, as returned under `OBSERVATIONS`.
    pub obs: Value,
}

impl SynopticStation {
    pub fn label(&self) -> String {
        if !self.name.is_empty() {
            self.name.clone()
        } else if !self.stid.is_empty() {
            self.stid.clone()
        } else {
            "station".to_string()
        }
    }
}

pub struct SynopticSource {
    client: reqwest::blocking::Client,
    token: String,
}

impl SynopticSource {
    pub fn new(user_agent: &str, token: &str) -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(user_agent)
            .timeout(std::time::Duration::from_secs(20))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(Self {
            client,
            token: token.trim().to_string(),
        })
    }

    pub fn has_token(&self) -> bool {
        !self.token.is_empty()
    }

    /// Latest observations for every station inside `bbox` = [west, south, east, north]
    /// (degrees). Returns station metadata + obs in one call.
    pub fn latest_in_bbox(&self, bbox: [f64; 4]) -> Result<Vec<SynopticStation>> {
        let url = format!(
            "{LATEST_URL}?token={}&bbox={:.4},{:.4},{:.4},{:.4}",
            urlenc(&self.token),
            bbox[0],
            bbox[1],
            bbox[2],
            bbox[3]
        );
        let resp = self
            .client
            .get(&url)
            .send()
            .context("requesting synoptic stations")?;
        let status = resp.status();
        let text = resp.text().context("reading synoptic response")?;
        if !status.is_success() {
            anyhow::bail!("synoptic HTTP {}: {}", status, text);
        }
        let v: Value = serde_json::from_str(&text).context("parsing synoptic response")?;
        let stations = v
            .get("STATION")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::with_capacity(stations.len());
        for d in stations {
            out.push(parse_station(&d));
        }
        Ok(out)
    }
}

fn urlenc(s: &str) -> String {
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

fn parse_station(d: &Value) -> SynopticStation {
    let stid = d
        .get("STID")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let name = d
        .get("NAME")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let network = d
        .get("NETWORK")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let lat = d.get("LATITUDE").and_then(|v| v.as_f64());
    let lon = d.get("LONGITUDE").and_then(|v| v.as_f64());
    let coords = match (lat, lon) {
        (Some(lat), Some(lon)) => Some(SynopticCoords { lat, lon }),
        _ => None,
    };
    let elevation_m = d.get("ELEVATION").and_then(|v| v.as_f64());
    let obs = d.get("OBSERVATIONS").cloned().unwrap_or(Value::Null);
    SynopticStation {
        stid,
        name,
        network,
        coords,
        elevation_m,
        obs,
    }
}
