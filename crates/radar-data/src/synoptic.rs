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
    /// The latest observation, as returned under `OBSERVATIONS` (each variable
    /// is `{ "value": ..., "date_time": ... }`).
    pub obs: Value,
    /// Variable → unit map from the station's `UNITS`.
    pub units: Value,
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
    pub fn latest_in_bbox(
        &self,
        bbox: [f64; 4],
        width: u32,
        height: u32,
    ) -> Result<Vec<SynopticStation>> {
        let url = format!(
            "{LATEST_URL}?token={}&bbox={:.4},{:.4},{:.4},{:.4}&width={}&height={}&spacing=28&within=120",
            urlenc(&self.token),
            bbox[0],
            bbox[1],
            bbox[2],
            bbox[3],
            width.max(1),
            height.max(1),
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
        // Synoptic puts the variable→unit map at the *top level* of the
        // response, not inside each station (station UNITS only carries
        // position/elevation). Keep it for every parsed station.
        let global_units = v.get("UNITS").cloned().unwrap_or(Value::Null);
        let mut out = Vec::with_capacity(stations.len());
        for d in stations {
            let st = parse_station(&d, &global_units);
            // Drop stations with no numeric observation (some entries are
            // metadata-only or have gone silent).
            if !has_numeric_obs(&st.obs) {
                continue;
            }
            out.push(st);
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

fn has_numeric_obs(obs: &Value) -> bool {
    obs.as_object().is_some_and(|o| {
        o.values()
            .any(|v| v.get("value").and_then(|x| x.as_f64()).is_some() || v.as_f64().is_some())
    })
}

/// A JSON value that may be a number or a numeric string (Synoptic sends
/// LATITUDE / LONGITUDE / ELEVATION as strings).
fn loose_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn parse_station(d: &Value, global_units: &Value) -> SynopticStation {
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
    let lat = d.get("LATITUDE").and_then(loose_f64);
    let lon = d.get("LONGITUDE").and_then(loose_f64);
    let coords = match (lat, lon) {
        (Some(lat), Some(lon)) => Some(SynopticCoords { lat, lon }),
        _ => None,
    };
    // Merge the top-level units with any station-level entries (station wins).
    let mut units = global_units.clone();
    if let (Some(dst), Some(src)) = (
        units.as_object_mut(),
        d.get("UNITS").and_then(|u| u.as_object()),
    ) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
    let elevation_m = d.get("ELEVATION").and_then(loose_f64).map(|raw| {
        // Synoptic reports ELEVATION in the unit given by UNITS.elevation
        // (usually feet). Convert to metres for the struct; the UI converts
        // back to feet.
        match units.get("elevation").and_then(|u| u.as_str()) {
            Some("ft") | Some("feet") | Some("Feet") => raw / 3.280_84,
            _ => raw,
        }
    });
    let obs = d.get("OBSERVATIONS").cloned().unwrap_or(Value::Null);
    SynopticStation {
        stid,
        name,
        network,
        coords,
        elevation_m,
        obs,
        units,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synoptic sends LATITUDE / LONGITUDE / ELEVATION as strings. If this
    /// regresses, stations load but nothing can be plotted on the map.
    #[test]
    fn parses_string_coordinates() {
        let json = serde_json::json!({
            "STID": "KOUN",
            "NAME": "Norman / Max Westheimer",
            "LATITUDE": "35.24350",
            "LONGITUDE": "-97.47080",
            "ELEVATION": "1182.0",
            "UNITS": { "air_temp": "Celsius" },
            "OBSERVATIONS": {
                "air_temp_value_1": { "value": 19.0, "date_time": "2026-10-27T01:45:00Z" }
            }
        });
        let st = parse_station(&json, &serde_json::json!({"air_temp": "Celsius"}));
        let c = st.coords.expect("string coordinates must parse");
        assert!((c.lat - 35.2435).abs() < 1e-6);
        assert!((c.lon + 97.4708).abs() < 1e-6);
        assert_eq!(st.elevation_m, Some(1182.0));
    }

    #[test]
    fn converts_feet_elevation_to_metres() {
        let json = serde_json::json!({
            "STID": "KTOP",
            "LATITUDE": "39.07",
            "LONGITUDE": "-95.62",
            "ELEVATION": "880.0",
            "OBSERVATIONS": { "air_temp_value_1": { "value": 20.0 } },
        });
        let units = serde_json::json!({ "elevation": "ft", "air_temp": "Celsius" });
        let st = parse_station(&json, &units);
        let m = st.elevation_m.expect("elevation");
        assert!((m - 268.2).abs() < 1.0, "got {m}");
    }

    #[test]
    fn parses_numeric_coordinates_too() {
        let json = serde_json::json!({
            "STID": "X",
            "LATITUDE": 35.0,
            "LONGITUDE": -97.0,
        });
        let st = parse_station(&json, &serde_json::json!({"air_temp": "Celsius"}));
        assert!(st.coords.is_some());
    }
}
