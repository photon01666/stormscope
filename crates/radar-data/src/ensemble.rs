//! GEFS point plumes from Open-Meteo's ensemble endpoint.
//!
//! This is a global ensemble, not HREF: it is useful for spread and scenario
//! context, but it is not a storm-scale ensemble. One small JSON request
//! carries every member, so it stays practical on a phone.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnsembleField {
    Temperature,
    Wind,
    Precipitation,
    Cape,
}

impl EnsembleField {
    pub fn label(self) -> &'static str {
        match self {
            EnsembleField::Temperature => "2 m temperature",
            EnsembleField::Wind => "10 m wind",
            EnsembleField::Precipitation => "precipitation",
            EnsembleField::Cape => "CAPE",
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            EnsembleField::Temperature => "°F",
            EnsembleField::Wind => "mph",
            EnsembleField::Precipitation => "in",
            EnsembleField::Cape => "J/kg",
        }
    }

    fn api_variable(self) -> &'static str {
        match self {
            EnsembleField::Temperature => "temperature_2m",
            EnsembleField::Wind => "wind_speed_10m",
            EnsembleField::Precipitation => "precipitation",
            EnsembleField::Cape => "cape",
        }
    }
}

#[derive(Clone, Debug)]
pub struct EnsembleSeries {
    pub field: EnsembleField,
    pub latitude: f64,
    pub longitude: f64,
    pub elevation_m: f64,
    pub times: Vec<DateTime<Utc>>,
    /// One row per member, one value per time.
    pub members: Vec<Vec<f64>>,
    pub mean: Vec<f64>,
    pub p10: Vec<f64>,
    pub p90: Vec<f64>,
}

pub struct EnsembleSource {
    client: reqwest::blocking::Client,
}

impl EnsembleSource {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::blocking::Client::builder()
                .user_agent(concat!(
                    "stormscope/",
                    env!("CARGO_PKG_VERSION"),
                    " (https://github.com/photon01666/stormscope)"
                ))
                .timeout(std::time::Duration::from_secs(35))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()?,
        })
    }

    pub fn fetch(&self, field: EnsembleField, lat: f64, lon: f64) -> Result<EnsembleSeries> {
        let var = field.api_variable();
        let url = format!(
            "https://ensemble-api.open-meteo.com/v1/ensemble\
             ?latitude={lat:.4}&longitude={lon:.4}&hourly={var}\
             &models=gfs025&forecast_days=3&timezone=UTC\
             &temperature_unit=fahrenheit&wind_speed_unit=mph&precipitation_unit=inch"
        );
        let v: Value = self
            .client
            .get(&url)
            .send()
            .context("requesting the ensemble plume")?
            .error_for_status()
            .context("ensemble HTTP error")?
            .json()
            .context("parsing ensemble JSON")?;
        if v.get("error").and_then(Value::as_bool) == Some(true) {
            bail!(
                "ensemble error: {}",
                v.get("reason").and_then(Value::as_str).unwrap_or("unknown")
            );
        }
        let hourly = v
            .get("hourly")
            .context("ensemble response had no hourly block")?;
        let times = hourly
            .get("time")
            .and_then(Value::as_array)
            .context("ensemble response had no time array")?
            .iter()
            .filter_map(|t| {
                t.as_str()
                    .and_then(|s| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").ok())
                    .map(|dt| dt.and_utc())
            })
            .collect::<Vec<_>>();
        if times.is_empty() {
            bail!("ensemble response had no usable times");
        }
        let prefix = format!("{var}_member");
        let mut members = Vec::new();
        let obj = hourly
            .as_object()
            .context("ensemble hourly was not an object")?;
        let mut keys = obj
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        for key in keys {
            let Some(values) = obj.get(&key).and_then(Value::as_array) else {
                continue;
            };
            let row = times
                .iter()
                .enumerate()
                .map(|(i, _)| values.get(i).and_then(Value::as_f64).unwrap_or(f64::NAN))
                .collect::<Vec<_>>();
            if row.iter().any(|v| v.is_finite()) {
                members.push(row);
            }
        }
        if members.is_empty() {
            bail!("ensemble response contained no member series");
        }
        let n = times.len();
        let mut mean = vec![0.0; n];
        let mut p10 = vec![0.0; n];
        let mut p90 = vec![0.0; n];
        for i in 0..n {
            let mut vals = members
                .iter()
                .filter_map(|m| m.get(i).copied().filter(|v| v.is_finite()))
                .collect::<Vec<_>>();
            if vals.is_empty() {
                mean[i] = f64::NAN;
                p10[i] = f64::NAN;
                p90[i] = f64::NAN;
                continue;
            }
            mean[i] = vals.iter().sum::<f64>() / vals.len() as f64;
            vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            p10[i] = percentile(&vals, 0.10);
            p90[i] = percentile(&vals, 0.90);
        }
        Ok(EnsembleSeries {
            field,
            latitude: v.get("latitude").and_then(Value::as_f64).unwrap_or(lat),
            longitude: v.get("longitude").and_then(Value::as_f64).unwrap_or(lon),
            elevation_m: v.get("elevation").and_then(Value::as_f64).unwrap_or(0.0),
            times,
            members,
            mean,
            p10,
            p90,
        })
    }
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let pos = q.clamp(0.0, 1.0) * (sorted.len().saturating_sub(1)) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    let t = pos - lo as f64;
    sorted[lo] * (1.0 - t) + sorted[hi.min(sorted.len() - 1)] * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_are_interpolated() {
        assert_eq!(percentile(&[0.0, 10.0], 0.5), 5.0);
        assert_eq!(percentile(&[0.0, 10.0, 20.0], 0.0), 0.0);
        assert_eq!(percentile(&[0.0, 10.0, 20.0], 1.0), 20.0);
    }

    #[test]
    fn fields_have_user_units() {
        assert_eq!(EnsembleField::Temperature.unit(), "°F");
        assert_eq!(EnsembleField::Wind.unit(), "mph");
        assert_eq!(EnsembleField::Precipitation.unit(), "in");
    }
}
