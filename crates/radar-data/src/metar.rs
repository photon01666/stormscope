//! METAR surface observations from aviationweather.gov.
//!
//! METAR is the free, authoritative airport observation feed. We adapt each
//! report to the same `SynopticStation` shape the map already draws, so the
//! station layer, popup and popup formatting work without a second UI.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::synoptic::{StationSource, SynopticCoords, SynopticStation};

const METAR_URL: &str = "https://aviationweather.gov/api/data/metar";

pub struct MetarSource {
    client: reqwest::blocking::Client,
}

/// One airport report in a station history.
#[derive(Clone, Debug)]
pub struct MetarObservation {
    pub time: DateTime<Utc>,
    pub temp_c: Option<f64>,
    pub dewpoint_c: Option<f64>,
    pub wind_speed_ms: Option<f64>,
    pub wind_gust_ms: Option<f64>,
    pub wind_dir_deg: Option<f64>,
    pub altim_hpa: Option<f64>,
    pub visibility_mi: Option<f64>,
    pub cover: Option<String>,
}

#[derive(Clone, Debug)]
pub struct MetarHistory {
    pub stid: String,
    pub name: String,
    pub observations: Vec<MetarObservation>,
}

impl MetarSource {
    pub fn new(user_agent: &str) -> Result<Self> {
        let ua = user_agent.trim();
        Ok(Self {
            client: reqwest::blocking::Client::builder()
                .user_agent(ua)
                .timeout(std::time::Duration::from_secs(25))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()?,
        })
    }

    /// Latest METARs inside `bbox` = [south, west, north, east].
    pub fn latest_in_bbox(&self, bbox: [f64; 4]) -> Result<Vec<SynopticStation>> {
        let url = metar_url(bbox);
        let reports: Value = self
            .client
            .get(&url)
            .send()
            .context("requesting METAR observations")?
            .error_for_status()
            .context("METAR HTTP error")?
            .json()
            .context("parsing METAR JSON")?;
        let mut out = Vec::new();
        for report in reports.as_array().into_iter().flatten() {
            if let Some(station) = parse_report(report) {
                out.push(station);
            }
        }
        Ok(out)
    }

    /// Recent reports for one airport, newest first.
    pub fn history(&self, stid: &str, hours: u8) -> Result<MetarHistory> {
        let url = format!(
            "{METAR_URL}?ids={}&format=json&hours={}",
            stid.trim(),
            hours.clamp(1, 24)
        );
        let reports: Value = self
            .client
            .get(&url)
            .send()
            .context("requesting METAR history")?
            .error_for_status()
            .context("METAR history HTTP error")?
            .json()
            .context("parsing METAR history JSON")?;
        let mut observations = reports
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(parse_observation)
            .collect::<Vec<_>>();
        if observations.is_empty() {
            bail!("METAR history for {stid} had no usable reports");
        }
        observations.sort_by(|a, b| b.time.cmp(&a.time));
        let name = reports
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.get("name"))
            .and_then(Value::as_str)
            .unwrap_or(stid)
            .to_string();
        Ok(MetarHistory {
            stid: stid.to_string(),
            name,
            observations,
        })
    }
}

fn metar_url(bbox: [f64; 4]) -> String {
    let [south, west, north, east] = bbox;
    format!("{METAR_URL}?bbox={south:.4},{west:.4},{north:.4},{east:.4}&format=json&hours=1")
}

fn parse_observation(v: &Value) -> Option<MetarObservation> {
    let time = v
        .get("reportTime")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
        .or_else(|| {
            v.get("obsTime")
                .and_then(Value::as_i64)
                .and_then(|s| DateTime::<Utc>::from_timestamp(s, 0))
        })?;
    let knots = |key: &str| v.get(key).and_then(Value::as_f64).map(|v| v * 0.514_444);
    Some(MetarObservation {
        time,
        temp_c: v.get("temp").and_then(Value::as_f64),
        dewpoint_c: v.get("dewp").and_then(Value::as_f64),
        wind_speed_ms: knots("wspd"),
        wind_gust_ms: knots("wgst"),
        wind_dir_deg: v.get("wdir").and_then(Value::as_f64),
        altim_hpa: v.get("altim").and_then(Value::as_f64),
        visibility_mi: v
            .get("visib")
            .and_then(|x| x.as_f64().or_else(|| x.as_str().and_then(parse_visibility))),
        cover: v.get("cover").and_then(Value::as_str).map(str::to_string),
    })
}

fn parse_report(v: &Value) -> Option<SynopticStation> {
    let stid = v.get("icaoId")?.as_str()?.trim().to_string();
    let lat = v.get("lat")?.as_f64()?;
    let lon = v.get("lon")?.as_f64()?;
    if !(lat.is_finite() && lon.is_finite()) {
        return None;
    }
    let temp = v.get("temp").and_then(Value::as_f64);
    let dewp = v.get("dewp").and_then(Value::as_f64);
    let wind_dir = v.get("wdir").and_then(Value::as_f64);
    let wind_knots = v.get("wspd").and_then(Value::as_f64);
    let gust_knots = v.get("wgst").and_then(Value::as_f64);
    let altim = v.get("altim").and_then(Value::as_f64);
    let visib = v
        .get("visib")
        .and_then(|x| x.as_f64().or_else(|| x.as_str().and_then(parse_visibility)));
    let cover = v.get("cover").and_then(Value::as_str).map(cover_percent);
    let ceiling_ft = v
        .get("clouds")
        .and_then(Value::as_array)
        .and_then(|clouds| {
            clouds
                .iter()
                .filter_map(|c| {
                    let cover = c.get("cover")?.as_str()?;
                    if cover != "BKN" && cover != "OVC" && cover != "VV" {
                        return None;
                    }
                    c.get("base")?.as_f64()
                })
                .reduce(f64::min)
        });

    let mut obs = serde_json::Map::new();
    let mut units = serde_json::Map::new();
    let mut put = |key: &str, value: Option<f64>, unit: &str| {
        if let Some(value) = value.filter(|v| v.is_finite()) {
            obs.insert(
                format!("{key}_value_1"),
                serde_json::json!({ "value": value, "date_time": v.get("reportTime").cloned().unwrap_or(Value::Null) }),
            );
            units.insert(key.to_string(), Value::String(unit.to_string()));
        }
    };
    put("air_temp", temp, "Celsius");
    put("dew_point_temperature", dewp, "Celsius");
    put("wind_direction", wind_dir, "");
    put("wind_speed", wind_knots.map(|v| v * 0.514_444), "m/s");
    put("wind_gust", gust_knots.map(|v| v * 0.514_444), "m/s");
    put("altimeter", altim, "hPa");
    put("visibility", visib, "miles");
    put("cloud_cover", cover, "%");
    put("ceiling", ceiling_ft, "ft");

    Some(SynopticStation {
        stid,
        name: v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("METAR station")
            .to_string(),
        network: "METAR".to_string(),
        source: StationSource::Metar,
        coords: Some(SynopticCoords { lat, lon }),
        elevation_m: v.get("elev").and_then(Value::as_f64),
        obs: Value::Object(obs),
        units: Value::Object(units),
    })
}

fn parse_visibility(s: &str) -> Option<f64> {
    let digits = s
        .trim()
        .trim_end_matches('+')
        .split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()?;
    Some(digits)
}

fn cover_percent(cover: &str) -> f64 {
    match cover {
        "SKC" | "CLR" => 0.0,
        "FEW" => 20.0,
        "SCT" => 45.0,
        "BKN" => 75.0,
        "OVC" | "VV" => 100.0,
        _ => 50.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapts_a_metar_report() {
        let report = serde_json::json!({
            "icaoId": "KOUN",
            "name": "Norman / Max Westheimer",
            "lat": 35.245,
            "lon": -97.472,
            "elev": 357,
            "temp": 21.0,
            "dewp": 12.0,
            "wdir": 180,
            "wspd": 10,
            "wgst": 18,
            "altim": 1015.0,
            "visib": "10+",
            "cover": "BKN",
            "reportTime": "2026-10-27T18:00:00Z",
            "clouds": [{"cover":"BKN","base":2500}]
        });
        let st = parse_report(&report).expect("station");
        assert_eq!(st.source, StationSource::Metar);
        assert_eq!(st.network, "METAR");
        assert!(st.coords.is_some());
        assert!((st.units["air_temp"].as_str().unwrap()) == "Celsius");
        assert!(st.obs["air_temp_value_1"]["value"].as_f64().unwrap() > 20.0);
        assert!(st.obs["wind_speed_value_1"]["value"].as_f64().unwrap() > 5.0);
    }

    #[test]
    fn parses_visibility_strings() {
        assert_eq!(parse_visibility("10+"), Some(10.0));
        assert_eq!(parse_visibility("3/4"), None);
        assert_eq!(parse_visibility("5"), Some(5.0));
    }

    #[test]
    fn bbox_is_south_west_north_east() {
        let url = metar_url([30.0, -105.0, 40.0, -89.0]);
        assert!(
            url.contains("bbox=30.0000,-105.0000,40.0000,-89.0000"),
            "{url}"
        );
    }

    #[test]
    fn parses_history_observation() {
        let report = serde_json::json!({
            "reportTime": "2026-10-27T18:00:00Z",
            "temp": 21.0,
            "dewp": 12.0,
            "wdir": 180,
            "wspd": 10,
            "wgst": 18,
            "altim": 1015.0,
            "visib": "10+",
            "cover": "BKN"
        });
        let obs = parse_observation(&report).expect("observation");
        assert!((obs.temp_c.unwrap() - 21.0).abs() < 1e-9);
        assert!((obs.wind_speed_ms.unwrap() - 5.144_44).abs() < 1e-3);
        assert_eq!(obs.cover.as_deref(), Some("BKN"));
    }
}
