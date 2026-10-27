//! Point soundings from Open-Meteo's pressure-level forecast. No API key;
//! the endpoint returns hourly profiles for a coordinate.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use radar_core::sounding::{Sounding, SoundingLevel};
use serde_json::Value;

const LEVELS: &[u16] = &[1000, 925, 850, 700, 600, 500, 400, 300, 250, 200, 150, 100];

pub struct SoundingSource {
    client: reqwest::blocking::Client,
}

/// One point's HRRR forecast profiles, hourly out to the model horizon.
#[derive(Clone, Debug)]
pub struct SoundingSeries {
    pub latitude: f64,
    pub longitude: f64,
    pub profiles: Vec<Sounding>,
}

impl SoundingSource {
    pub fn new(user_agent: &str) -> Result<Self> {
        let ua = user_agent.trim();
        if ua.is_empty() {
            bail!("sounding source needs a User-Agent");
        }
        let client = reqwest::blocking::Client::builder()
            .user_agent(ua)
            .timeout(std::time::Duration::from_secs(20))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(Self { client })
    }

    pub fn model_sounding(&self, lat: f64, lon: f64) -> Result<Sounding> {
        let mut vars = vec![
            "temperature_2m".to_string(),
            "relativehumidity_2m".to_string(),
            "windspeed_10m".to_string(),
            "winddirection_10m".to_string(),
            "surface_pressure".to_string(),
        ];
        for p in LEVELS {
            vars.push(format!("temperature_{p}hPa"));
            vars.push(format!("relativehumidity_{p}hPa"));
            vars.push(format!("windspeed_{p}hPa"));
            vars.push(format!("winddirection_{p}hPa"));
            vars.push(format!("geopotential_height_{p}hPa"));
        }
        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={lat:.4}&longitude={lon:.4}\
             &hourly={}&forecast_days=1&timezone=UTC",
            vars.join(",")
        );
        let mut last_error = String::new();
        let mut resp = None;
        for attempt in 0..2 {
            match self.fetch_json(&url) {
                Ok(v) if v.get("hourly").is_some() => {
                    resp = Some(v);
                    break;
                }
                Ok(v) => {
                    last_error = v
                        .get("reason")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| "response had no hourly block".to_string());
                }
                Err(e) => last_error = format!("{e:#}"),
            }
            if attempt == 0 {
                std::thread::sleep(std::time::Duration::from_millis(700));
            }
        }
        let Some(resp) = resp else {
            bail!("Open-Meteo sounding failed: {last_error}");
        };

        let hourly = resp.get("hourly").context("no hourly block")?;
        let times = hourly
            .get("time")
            .and_then(Value::as_array)
            .context("no time array")?;
        let now = Utc::now();
        let idx = times
            .iter()
            .enumerate()
            .min_by_key(|(_, t)| {
                chrono::NaiveDateTime::parse_from_str(t.as_str().unwrap_or(""), "%Y-%m-%dT%H:%M")
                    .map(|dt| (dt.and_utc() - now).num_seconds().abs())
                    .unwrap_or(i64::MAX)
            })
            .map(|(i, _)| i)
            .unwrap_or(0);
        let valid_time = times
            .get(idx)
            .and_then(Value::as_str)
            .and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").ok())
            .map(|dt| dt.and_utc())
            .unwrap_or(now);

        let elevation = resp.get("elevation").and_then(Value::as_f64).unwrap_or(0.0);
        let mut levels = Vec::new();
        let st = num(hourly, "temperature_2m", idx);
        let srh = num(hourly, "relativehumidity_2m", idx);
        let sws = num(hourly, "windspeed_10m", idx);
        let swd = num(hourly, "winddirection_10m", idx);
        let sp = num(hourly, "surface_pressure", idx);
        if let (Some(t), Some(rh), Some(ws), Some(wd), Some(p)) = (st, srh, sws, swd, sp) {
            let td = radar_core::sounding::dewpoint(t, rh);
            let speed_ms = ws / 3.6;
            let dir = wd.to_radians();
            levels.push(SoundingLevel {
                pressure_hpa: p,
                height_m: elevation,
                temp_c: t,
                dewpoint_c: td,
                rh_pct: rh,
                wind_speed_ms: speed_ms,
                wind_dir_deg: wd,
                u_ms: -speed_ms * dir.sin(),
                v_ms: -speed_ms * dir.cos(),
            });
        }
        for p in LEVELS {
            let t = num(hourly, &format!("temperature_{p}hPa"), idx);
            let rh = num(hourly, &format!("relativehumidity_{p}hPa"), idx);
            let ws = num(hourly, &format!("windspeed_{p}hPa"), idx);
            let wd = num(hourly, &format!("winddirection_{p}hPa"), idx);
            let z = num(hourly, &format!("geopotential_height_{p}hPa"), idx);
            let (Some(t), Some(rh), Some(ws), Some(wd), Some(z)) = (t, rh, ws, wd, z) else {
                continue;
            };
            if levels
                .first()
                .is_some_and(|s| *p as f64 > s.pressure_hpa + 2.0)
            {
                continue;
            }
            let td = radar_core::sounding::dewpoint(t, rh);
            let speed_ms = ws / 3.6;
            let dir = wd.to_radians();
            // Meteorological wind: from direction.
            let u = -speed_ms * dir.sin();
            let v = -speed_ms * dir.cos();
            levels.push(SoundingLevel {
                pressure_hpa: *p as f64,
                height_m: z,
                temp_c: t,
                dewpoint_c: td,
                rh_pct: rh,
                wind_speed_ms: speed_ms,
                wind_dir_deg: wd,
                u_ms: u,
                v_ms: v,
            });
        }
        if levels.len() < 3 {
            bail!("sounding had too few levels");
        }
        Ok(Sounding::from_levels(
            lat,
            lon,
            valid_time,
            "Open-Meteo",
            levels,
        ))
    }

    /// Hourly HRRR profiles from Open-Meteo's `gfs_hrrr` model. Small enough
    /// for Android: one JSON request, no GRIB grids.
    pub fn hrrr_series(&self, lat: f64, lon: f64) -> Result<SoundingSeries> {
        let mut vars = vec![
            "temperature_2m".to_string(),
            "relativehumidity_2m".to_string(),
            "windspeed_10m".to_string(),
            "winddirection_10m".to_string(),
            "surface_pressure".to_string(),
        ];
        for p in LEVELS {
            vars.push(format!("temperature_{p}hPa"));
            vars.push(format!("relativehumidity_{p}hPa"));
            vars.push(format!("windspeed_{p}hPa"));
            vars.push(format!("winddirection_{p}hPa"));
            vars.push(format!("geopotential_height_{p}hPa"));
        }
        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={lat:.4}&longitude={lon:.4}\
             &hourly={}&models=gfs_hrrr&forecast_days=2&timezone=UTC",
            vars.join(",")
        );
        let resp = self.fetch_json(&url)?;
        let hourly = resp.get("hourly").context("no hourly block")?;
        let times = hourly
            .get("time")
            .and_then(Value::as_array)
            .context("no time array")?;
        let elevation = resp.get("elevation").and_then(Value::as_f64).unwrap_or(0.0);
        let mut profiles = Vec::new();
        for (idx, stamp) in times.iter().enumerate() {
            let Some(time) = stamp
                .as_str()
                .and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").ok())
                .map(|dt| dt.and_utc())
            else {
                continue;
            };
            let levels = levels_at(hourly, idx, elevation);
            if levels.len() < 3 {
                continue;
            }
            profiles.push(Sounding::from_levels(
                lat,
                lon,
                time,
                "HRRR (Open-Meteo)",
                levels,
            ));
        }
        if profiles.is_empty() {
            bail!("HRRR series had no usable profiles");
        }
        Ok(SoundingSeries {
            latitude: lat,
            longitude: lon,
            profiles,
        })
    }

    fn fetch_json(&self, url: &str) -> Result<Value> {
        let resp = self
            .client
            .get(url)
            .send()
            .context("requesting Open-Meteo sounding")?;
        let status = resp.status();
        let text = resp.text().context("reading Open-Meteo response")?;
        let value: Value = serde_json::from_str(&text)
            .with_context(|| format!("parsing Open-Meteo JSON ({status})"))?;
        if !status.is_success() {
            let reason = value
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            bail!("Open-Meteo HTTP {status}: {reason}");
        }
        if value.get("error").and_then(Value::as_bool) == Some(true) {
            let reason = value
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            bail!("Open-Meteo error: {reason}");
        }
        Ok(value)
    }
}

fn levels_at(hourly: &Value, idx: usize, elevation: f64) -> Vec<SoundingLevel> {
    let mut levels = Vec::new();
    let st = num(hourly, "temperature_2m", idx);
    let srh = num(hourly, "relativehumidity_2m", idx);
    let sws = num(hourly, "windspeed_10m", idx);
    let swd = num(hourly, "winddirection_10m", idx);
    let sp = num(hourly, "surface_pressure", idx);
    if let (Some(t), Some(rh), Some(ws), Some(wd), Some(p)) = (st, srh, sws, swd, sp) {
        let td = radar_core::sounding::dewpoint(t, rh);
        let speed_ms = ws / 3.6;
        let dir = wd.to_radians();
        levels.push(SoundingLevel {
            pressure_hpa: p,
            height_m: elevation,
            temp_c: t,
            dewpoint_c: td,
            rh_pct: rh,
            wind_speed_ms: speed_ms,
            wind_dir_deg: wd,
            u_ms: -speed_ms * dir.sin(),
            v_ms: -speed_ms * dir.cos(),
        });
    }
    for p in LEVELS {
        let t = num(hourly, &format!("temperature_{p}hPa"), idx);
        let rh = num(hourly, &format!("relativehumidity_{p}hPa"), idx);
        let ws = num(hourly, &format!("windspeed_{p}hPa"), idx);
        let wd = num(hourly, &format!("winddirection_{p}hPa"), idx);
        let z = num(hourly, &format!("geopotential_height_{p}hPa"), idx);
        let (Some(t), Some(rh), Some(ws), Some(wd), Some(z)) = (t, rh, ws, wd, z) else {
            continue;
        };
        if levels
            .first()
            .is_some_and(|s| *p as f64 > s.pressure_hpa + 2.0)
        {
            continue;
        }
        let td = radar_core::sounding::dewpoint(t, rh);
        let speed_ms = ws / 3.6;
        let dir = wd.to_radians();
        levels.push(SoundingLevel {
            pressure_hpa: *p as f64,
            height_m: z,
            temp_c: t,
            dewpoint_c: td,
            rh_pct: rh,
            wind_speed_ms: speed_ms,
            wind_dir_deg: wd,
            u_ms: -speed_ms * dir.sin(),
            v_ms: -speed_ms * dir.cos(),
        });
    }
    levels
}

fn num(root: &Value, key: &str, idx: usize) -> Option<f64> {
    root.get(key)?.as_array()?.get(idx)?.as_f64()
}
