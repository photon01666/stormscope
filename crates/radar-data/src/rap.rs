//! RAP point analysis from NOMADS.
//!
//! The RAP 13 km pressure-level file is large, so this asks NOMADS' GRIB
//! filter for a tiny box around the point and only the variables a sounding
//! needs. That keeps the request around tens of KB and avoids shipping a
//! GRIB library to the UI thread.

use std::collections::BTreeMap;
use std::io::Cursor;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Timelike, Utc};
use grib::LatLons;
use radar_core::sounding::{Sounding, SoundingLevel};

const LEVELS: &[u16] = &[1000, 925, 850, 700, 600, 500, 400, 300, 250, 200, 150, 100];

pub struct RapSource {
    client: reqwest::blocking::Client,
}

#[derive(Default, Clone)]
struct LevelValues {
    temp_k: Option<f64>,
    rh_pct: Option<f64>,
    height_m: Option<f64>,
    u_ms: Option<f64>,
    v_ms: Option<f64>,
}

impl RapSource {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::blocking::Client::builder()
                .user_agent(concat!(
                    "stormscope/",
                    env!("CARGO_PKG_VERSION"),
                    " (https://github.com/photon01666/stormscope)"
                ))
                .timeout(std::time::Duration::from_secs(60))
                .connect_timeout(std::time::Duration::from_secs(12))
                .build()?,
        })
    }

    /// Latest RAP analysis profile at a point.
    pub fn latest(&self, lat: f64, lon: f64) -> Result<Sounding> {
        let now = Utc::now();
        let mut last_error = String::new();
        for back in 0..8 {
            let init = now - Duration::hours(back);
            let init = init
                .with_minute(0)
                .and_then(|t| t.with_second(0))
                .and_then(|t| t.with_nanosecond(0))
                .unwrap_or(init);
            match self.fetch_at(init, lat, lon) {
                Ok(s) => return Ok(s),
                Err(e) => {
                    last_error = format!("{e:#}");
                    log::debug!("RAP {init}: {last_error}");
                }
            }
        }
        bail!("RAP analysis unavailable: {last_error}");
    }

    fn fetch_at(&self, init: DateTime<Utc>, lat: f64, lon: f64) -> Result<Sounding> {
        let file = format!("rap.t{:02}z.awp130pgrbf00.grib2", init.hour());
        let dir = format!("/rap.{}", init.format("%Y%m%d"));
        let mut params = vec![
            ("dir".to_string(), dir),
            ("file".to_string(), file),
            ("var_TMP".to_string(), "on".to_string()),
            ("var_RH".to_string(), "on".to_string()),
            ("var_HGT".to_string(), "on".to_string()),
            ("var_UGRD".to_string(), "on".to_string()),
            ("var_VGRD".to_string(), "on".to_string()),
            ("var_PRES".to_string(), "on".to_string()),
            ("lev_2_m_above_ground".to_string(), "on".to_string()),
            ("lev_surface".to_string(), "on".to_string()),
        ];
        for p in LEVELS {
            params.push((format!("lev_{p}_mb"), "on".to_string()));
        }
        // Build the query manually so the level names above stay readable.
        let mut query = Vec::new();
        for (k, v) in params {
            query.push(format!("{k}={}", urlencode(&v)));
        }
        let padding = 0.6;
        query.push(format!(
            "subregion=&leftlon={:.3}&rightlon={:.3}&toplat={:.3}&bottomlat={:.3}",
            lon - padding,
            lon + padding,
            lat + padding,
            lat - padding
        ));
        let url = format!(
            "https://nomads.ncep.noaa.gov/cgi-bin/filter_rap.pl?{}",
            query.join("&")
        );
        let bytes = self
            .client
            .get(&url)
            .send()
            .context("requesting RAP profile")?
            .error_for_status()
            .context("RAP HTTP error")?
            .bytes()
            .context("reading RAP profile")?;
        if !bytes.starts_with(b"GRIB") {
            bail!("NOMADS returned {} non-GRIB bytes", bytes.len());
        }
        decode_profile(&bytes, init, lat, lon)
    }
}

fn decode_profile(bytes: &[u8], init: DateTime<Utc>, lat: f64, lon: f64) -> Result<Sounding> {
    let grib2 = grib::from_reader(Cursor::new(bytes)).context("parsing RAP GRIB2")?;
    let mut levels: BTreeMap<i32, LevelValues> = BTreeMap::new();
    let mut surface_pressure_pa: Option<f64> = None;
    let mut surface_height_m: Option<f64> = None;
    let mut surface_temp_k: Option<f64> = None;
    let mut two_m_temp_k: Option<f64> = None;
    let mut two_m_rh: Option<f64> = None;

    for ((_mi, _si), sub) in grib2.iter() {
        let pd = sub.prod_def();
        let (Some(cat), Some(num)) = (pd.parameter_category(), pd.parameter_number()) else {
            continue;
        };
        let Some((fs, _)) = pd.fixed_surfaces() else {
            continue;
        };
        let level_value = fs.value();
        let is_pressure = fs.surface_type == 100 && level_value.is_finite() && level_value > 0.0;
        let is_surface = fs.surface_type == 1;
        let is_two_m = fs.surface_type == 103 && (level_value - 2.0).abs() < 0.01;
        if !(is_pressure || is_surface || is_two_m) {
            continue;
        }
        let latlons = sub.latlons().context("RAP lat/lons")?;
        let decoder =
            grib::Grib2SubmessageDecoder::from(sub).context("building the RAP GRIB decoder")?;
        let values = decoder.dispatch().context("decoding RAP values")?;
        let mut best = f64::MAX;
        let mut found = None;
        for ((la, lo), raw) in latlons.zip(values) {
            if !la.is_finite() || !lo.is_finite() || !raw.is_finite() {
                continue;
            }
            let dy = (la - lat) * 111.32;
            let dx = (lo - lon) * 111.32 * lat.to_radians().cos();
            let d = dx * dx + dy * dy;
            if d < best {
                best = d;
                found = Some(f64::from(raw));
            }
        }
        let Some(value) = found else {
            continue;
        };
        if !value.is_finite() {
            continue;
        }

        if is_surface {
            match (cat, num) {
                (3, 0) => surface_pressure_pa = Some(value),
                (3, 5) => surface_height_m = Some(value),
                (0, 0) => surface_temp_k = Some(value),
                _ => {}
            }
            continue;
        }
        if is_two_m {
            match (cat, num) {
                (0, 0) => two_m_temp_k = Some(value),
                (1, 1) => two_m_rh = Some(value),
                _ => {}
            }
            continue;
        }

        let p = (level_value / 100.0).round() as i32;
        let entry = levels.entry(p).or_default();
        match (cat, num) {
            (0, 0) => entry.temp_k = Some(value),
            (1, 1) => entry.rh_pct = Some(value),
            (3, 5) => entry.height_m = Some(value),
            (2, 2) => entry.u_ms = Some(value),
            (2, 3) => entry.v_ms = Some(value),
            _ => {}
        }
    }

    let mut out = Vec::new();
    for (p, v) in levels {
        let (Some(t), Some(rh), Some(z), Some(u), Some(v)) =
            (v.temp_k, v.rh_pct, v.height_m, v.u_ms, v.v_ms)
        else {
            continue;
        };
        let temp_c = t - 273.15;
        let rh = rh.clamp(0.0, 100.0);
        let speed = u.hypot(v);
        let dir = (-u).atan2(-v).to_degrees().rem_euclid(360.0);
        out.push(SoundingLevel {
            pressure_hpa: p as f64,
            height_m: z,
            temp_c,
            dewpoint_c: radar_core::sounding::dewpoint(temp_c, rh),
            rh_pct: rh,
            wind_speed_ms: speed,
            wind_dir_deg: dir,
            u_ms: u,
            v_ms: v,
        });
    }

    if let (Some(p_pa), Some(z), Some(t_k)) = (
        surface_pressure_pa,
        surface_height_m,
        surface_temp_k.or(two_m_temp_k),
    ) {
        let temp_c = t_k - 273.15;
        let rh = two_m_rh.unwrap_or(70.0).clamp(0.0, 100.0);
        let p_hpa = p_pa / 100.0;
        if !out.iter().any(|l| (l.pressure_hpa - p_hpa).abs() < 2.0) {
            out.push(SoundingLevel {
                pressure_hpa: p_hpa,
                height_m: z,
                temp_c,
                dewpoint_c: radar_core::sounding::dewpoint(temp_c, rh),
                rh_pct: rh,
                wind_speed_ms: 0.0,
                wind_dir_deg: 0.0,
                u_ms: 0.0,
                v_ms: 0.0,
            });
        }
    }
    if out.len() < 3 {
        bail!("RAP profile had too few levels ({})", out.len());
    }
    Ok(Sounding::from_levels(lat, lon, init, "RAP analysis", out))
}

fn urlencode(s: &str) -> String {
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('&', "%26")
        .replace('=', "%3D")
        .replace('/', "%2F")
        .replace(':', "%3A")
}

#[cfg(test)]
mod tests {
    #[test]
    fn level_list_contains_mandatory_pressure_levels() {
        assert!(super::LEVELS.contains(&500));
        assert!(super::LEVELS.contains(&850));
        assert!(super::LEVELS.contains(&100));
    }
}
