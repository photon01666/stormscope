//! Observed radiosonde profiles from the University of Wyoming archive.
//!
//! The Wyoming server publishes the current station list as JSON and one
//! profile as CSV. We pick the station nearest the requested point and return
//! the most recent available launch. This is an *observed* profile, not a
//! model sounding: it is the real balloon ascent, with all of its gaps and
//! timing.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDateTime, Utc};
use radar_core::sounding::{Sounding, SoundingLevel};
use serde_json::Value;

const STATION_LIST: &str = "https://weather.uwyo.edu/cgi-bin/sounding_json";
const SOUNDING: &str = "https://weather.uwyo.edu/wsgi/sounding";

#[derive(Clone, Debug)]
pub struct RaobStation {
    pub id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub src: String,
}

pub struct RaobSource {
    client: reqwest::blocking::Client,
}

impl RaobSource {
    pub fn new(user_agent: &str) -> Result<Self> {
        let ua = user_agent.trim();
        if ua.is_empty() {
            bail!("RAOB source needs a User-Agent");
        }
        Ok(Self {
            client: reqwest::blocking::Client::builder()
                .user_agent(ua)
                .timeout(std::time::Duration::from_secs(30))
                .connect_timeout(std::time::Duration::from_secs(12))
                .build()?,
        })
    }

    /// The latest observed profile from the radiosonde station nearest
    /// `lat, lon`.
    pub fn latest_nearest(&self, lat: f64, lon: f64) -> Result<Sounding> {
        let (valid, stations) = self.station_list()?;
        let (station, distance_km) = stations
            .into_iter()
            .map(|s| {
                let d = distance_km(lat, lon, s.lat, s.lon);
                (s, d)
            })
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .ok_or_else(|| anyhow::anyhow!("no radiosonde stations available"))?;
        log::debug!(
            "RAOB: nearest {} ({}) {:.0} km away",
            station.id,
            station.name,
            distance_km
        );
        let mut last_error = String::new();
        for back in [0i64, 12, 24] {
            let at = valid - chrono::Duration::hours(back);
            match self
                .fetch_csv(&station, at)
                .and_then(|csv| parse_csv(&station, at, &csv))
            {
                Ok(s) => return Ok(s),
                Err(e) => last_error = format!("{e:#}"),
            }
        }
        bail!(
            "RAOB {} ({}) unavailable: {last_error}",
            station.id,
            station.name
        )
    }

    fn station_list(&self) -> Result<(DateTime<Utc>, Vec<RaobStation>)> {
        let value: Value = self
            .client
            .get(STATION_LIST)
            .send()
            .context("requesting the RAOB station list")?
            .error_for_status()
            .context("RAOB station list HTTP error")?
            .json()
            .context("parsing the RAOB station list")?;
        let valid = value
            .get("datetime")
            .and_then(Value::as_str)
            .and_then(|s| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").ok())
            .map(|dt| dt.and_utc())
            .unwrap_or_else(Utc::now);
        let stations = value
            .get("stations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(parse_station)
            .collect::<Vec<_>>();
        if stations.is_empty() {
            bail!("RAOB station list was empty");
        }
        Ok((valid, stations))
    }

    fn fetch_csv(&self, station: &RaobStation, valid: DateTime<Utc>) -> Result<String> {
        let url = format!(
            "{SOUNDING}?datetime={}&id={}&src={}&type=TEXT%3ACSV",
            urlencode(&valid.format("%Y-%m-%d %H:%M:%S").to_string()),
            urlencode(&station.id),
            urlencode(&station.src),
        );
        let text = self
            .client
            .get(&url)
            .send()
            .context("requesting the RAOB profile")?
            .error_for_status()
            .context("RAOB profile HTTP error")?
            .text()
            .context("reading the RAOB profile")?;
        if !text.contains("pressure_hPa") {
            bail!("RAOB profile was not CSV (station/data unavailable)");
        }
        Ok(text)
    }
}

fn parse_station(v: &Value) -> Option<RaobStation> {
    let id = v.get("stationid")?.as_str()?.trim().to_string();
    if id.is_empty() {
        return None;
    }
    Some(RaobStation {
        id,
        name: v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("radiosonde")
            .to_string(),
        lat: v.get("lat")?.as_f64()?,
        lon: v.get("lon")?.as_f64()?,
        src: v
            .get("src")
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN")
            .to_string(),
    })
}

fn parse_csv(station: &RaobStation, valid: DateTime<Utc>, csv: &str) -> Result<Sounding> {
    let mut lines = csv.lines();
    let header = lines.next().context("RAOB CSV had no header")?;
    let cols = header
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    let col = |name: &str| cols.iter().position(|c| c == name);
    let ip = col("pressure_hpa").context("RAOB CSV has no pressure_hPa")?;
    let iz = col("geopotential height_m").context("RAOB CSV has no height")?;
    let it = col("temperature_c").context("RAOB CSV has no temperature")?;
    let id = col("dew point temperature_c").context("RAOB CSV has no dew point")?;
    let irh = col("relative humidity_%");
    let iwd = col("wind direction_degree");
    let iws = col("wind speed_m/s");

    let mut levels = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with(',') {
            continue;
        }
        let fields = line.split(',').map(str::trim).collect::<Vec<_>>();
        let Some(pressure) = field(&fields, ip).and_then(|s| s.parse::<f64>().ok()) else {
            continue;
        };
        let Some(height) = field(&fields, iz).and_then(|s| s.parse::<f64>().ok()) else {
            continue;
        };
        let Some(temp) = field(&fields, it).and_then(|s| s.parse::<f64>().ok()) else {
            continue;
        };
        let Some(dewpoint) = field(&fields, id).and_then(|s| s.parse::<f64>().ok()) else {
            continue;
        };
        if !(pressure.is_finite() && height.is_finite() && temp.is_finite()) {
            continue;
        }
        let rh = irh
            .and_then(|i| field(&fields, i))
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or_else(|| relative_humidity(temp, dewpoint));
        let wind_dir = iwd
            .and_then(|i| field(&fields, i))
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        let wind_speed = iws
            .and_then(|i| field(&fields, i))
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        let dir = wind_dir.to_radians();
        levels.push(SoundingLevel {
            pressure_hpa: pressure,
            height_m: height,
            temp_c: temp,
            dewpoint_c: dewpoint,
            rh_pct: rh.clamp(0.0, 100.0),
            wind_speed_ms: wind_speed.max(0.0),
            wind_dir_deg: wind_dir.rem_euclid(360.0),
            u_ms: -wind_speed * dir.sin(),
            v_ms: -wind_speed * dir.cos(),
        });
    }
    if levels.len() < 3 {
        bail!(
            "RAOB {} had too few usable levels ({}). Try another launch.",
            station.id,
            levels.len()
        );
    }
    // High-resolution RAOBs can contain thousands of 1-second levels. Five
    // hPa is plenty for a Skew-T and the parcel integration, and keeps the
    // UI thread responsive.
    levels.sort_by(|a, b| b.pressure_hpa.partial_cmp(&a.pressure_hpa).unwrap());
    let mut thinned: Vec<SoundingLevel> = Vec::with_capacity(256);
    for level in levels {
        let keep = thinned
            .last()
            .is_none_or(|last| (last.pressure_hpa - level.pressure_hpa).abs() >= 5.0);
        if keep {
            thinned.push(level);
        }
    }
    if thinned.len() < 3 {
        bail!("RAOB {} thinned to too few levels", station.id);
    }
    Ok(Sounding::from_levels(
        station.lat,
        station.lon,
        valid,
        format!("RAOB {} · {}", station.id, station.name),
        thinned,
    ))
}

fn field<'a>(fields: &'a [&str], idx: usize) -> Option<&'a str> {
    fields
        .get(idx)
        .copied()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn relative_humidity(temp_c: f64, dewpoint_c: f64) -> f64 {
    let es = 6.112 * (17.67 * temp_c / (temp_c + 243.5)).exp();
    let e = 6.112 * (17.67 * dewpoint_c / (dewpoint_c + 243.5)).exp();
    (100.0 * e / es).clamp(0.0, 100.0)
}

fn distance_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let dy = (lat2 - lat1) * 111.32;
    let dx = (lon2 - lon1) * 111.32 * ((lat1 + lat2) * 0.5).to_radians().cos();
    (dx * dx + dy * dy).sqrt()
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_style_csv() {
        let csv = "\
time,longitude,latitude,pressure_hPa,geopotential height_m,temperature_C,dew point temperature_C,ice point temperature_C,relative humidity_%,humidity wrt ice_%,mixing ratio_g/kg,wind direction_degree,wind speed_m/s
2026-10-27 00:00:00,-97.0,35.0, 1000.0, 100, 20.0, 15.0, 15.0, 73, 73, 10.0, 180, 10.0
2026-10-27 00:00:00,-97.0,35.0,  925.0, 800, 16.0, 12.0, 12.0, 77, 77,  8.0, 200, 15.0
2026-10-27 00:00:00,-97.0,35.0,  850.0, 1500, 12.0, 8.0, 8.0, 76, 76,  6.0, 220, 20.0
2026-10-27 00:00:00,-97.0,35.0,  500.0, 5600, -10.0, -20.0, -20.0, 50, 50, 2.0, 250, 25.0
";
        let station = RaobStation {
            id: "OUN".into(),
            name: "Norman".into(),
            lat: 35.18,
            lon: -97.44,
            src: "BUFR".into(),
        };
        let s = parse_csv(
            &station,
            DateTime::parse_from_rfc3339("2026-10-27T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            csv,
        )
        .expect("profile");
        assert_eq!(s.levels.len(), 4);
        assert!(s.source.contains("RAOB OUN"));
        assert!(s.sb_cape_jkg.is_finite());
        assert!((s.levels[0].wind_speed_ms - 10.0).abs() < 1e-6);
    }

    #[test]
    fn finds_the_nearest_station() {
        assert!(distance_km(35.0, -97.0, 35.0, -97.0) < 0.01);
        assert!(distance_km(35.0, -97.0, 36.0, -97.0) > 100.0);
    }
}
