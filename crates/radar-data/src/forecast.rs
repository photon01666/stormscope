//! NWS gridpoint forecasts via `api.weather.gov`.
//!
//! Two requests per forecast, per the API's own design: `points/{lat},{lon}`
//! resolves a coordinate to a forecast office grid and hands back the
//! forecast URL, which is then fetched. Requests need the same descriptive
//! `User-Agent` as the alerts endpoint.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use radar_core::forecast::{Forecast, ForecastPeriod};

/// `api.weather.gov` coordinate lookup.
const POINTS_URL: &str = "https://api.weather.gov/points";

/// How many periods to keep: a week of day/night periods is plenty for a
/// radar app and keeps the fetch small.
const MAX_PERIODS: usize = 14;

/// Client for gridpoint forecasts.
pub struct ForecastSource {
    client: reqwest::blocking::Client,
}

impl ForecastSource {
    /// Build a source with a descriptive User-Agent (required by
    /// api.weather.gov — include something identifying the app and a
    /// contact, e.g. `"stormscope/0.1 (contact@example.com)"`).
    pub fn new(user_agent: &str) -> Result<Self> {
        let ua = user_agent.trim();
        if ua.len() < 8 {
            bail!(
                "api.weather.gov requires a descriptive User-Agent \
                 like \"stormscope/0.1 (contact@example.com)\"; got {user_agent:?}"
            );
        }
        let client = reqwest::blocking::Client::builder()
            .user_agent(ua)
            .timeout(std::time::Duration::from_secs(20))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(Self { client })
    }

    /// Forecast for a point: resolves the grid, then fetches its periods.
    pub fn for_point(&self, lat: f64, lon: f64) -> Result<Forecast> {
        let points: serde_json::Value =
            self.get_json(&format!("{POINTS_URL}/{lat:.4},{lon:.4}"))?;
        let forecast_url = points
            .pointer("/properties/forecast")
            .and_then(|v| v.as_str())
            .context("points response had no forecast URL (point outside NWS coverage?)")?;
        let fc: serde_json::Value = self.get_json(forecast_url)?;
        // The forecast office is the path segment before the grid numbers.
        let office = forecast_url
            .split("/gridpoints/")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .unwrap_or("")
            .to_string();
        Ok(forecast_from_json(&points, &fc, &office))
    }

    fn get_json(&self, url: &str) -> Result<serde_json::Value> {
        let resp = self
            .client
            .get(url)
            .header("Accept", "application/geo+json")
            .send()
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            bail!("GET {url} returned HTTP {status}");
        }
        Ok(resp.json()?)
    }
}

/// Build a [`Forecast`] from the two API responses. Separated from the
/// network so unit tests can run on canned fixtures.
pub(crate) fn forecast_from_json(
    points: &serde_json::Value,
    forecast: &serde_json::Value,
    office: &str,
) -> Forecast {
    let s = |v: &serde_json::Value, path: &str| -> String {
        v.pointer(path)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let place = {
        let city = s(points, "/properties/relativeLocation/properties/city");
        let state = s(points, "/properties/relativeLocation/properties/state");
        match (city.is_empty(), state.is_empty()) {
            (false, false) => format!("{city}, {state}"),
            (false, true) => city,
            _ => String::new(),
        }
    };
    let grid = {
        let x = points.pointer("/properties/gridX").and_then(|v| v.as_i64());
        let y = points.pointer("/properties/gridY").and_then(|v| v.as_i64());
        match (x, y) {
            (Some(x), Some(y)) => format!("{x},{y}"),
            _ => String::new(),
        }
    };
    let updated = parse_time(&s(forecast, "/properties/updated"));

    let mut periods = Vec::new();
    if let Some(arr) = forecast
        .pointer("/properties/periods")
        .and_then(|v| v.as_array())
    {
        for p in arr.iter().take(MAX_PERIODS) {
            let name = s(p, "/name");
            if name.is_empty() {
                continue;
            }
            periods.push(ForecastPeriod {
                name,
                temperature: p
                    .pointer("/temperature")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(f64::NAN) as f32,
                unit: s(p, "/temperatureUnit"),
                wind: {
                    let speed = s(p, "/windSpeed");
                    let dir = s(p, "/windDirection");
                    match (dir.is_empty(), speed.is_empty()) {
                        (false, false) => format!("{dir} {speed}"),
                        (false, true) => dir,
                        _ => speed,
                    }
                },
                short: s(p, "/shortForecast"),
                detailed: s(p, "/detailedForecast"),
                start: parse_time(&s(p, "/startTime")).unwrap_or_else(Utc::now),
                is_daytime: p.get("isDaytime").and_then(|v| v.as_bool()).unwrap_or(true),
            });
        }
    }

    Forecast {
        office: office.to_string(),
        place,
        grid,
        updated,
        periods,
    }
}

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    if s.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed-down real responses: one gridpoint lookup and one forecast.
    fn points_fixture() -> serde_json::Value {
        serde_json::json!({
            "properties": {
                "gridId": "TOP", "gridX": 62, "gridY": 46,
                "forecast": "https://api.weather.gov/gridpoints/TOP/62,46/forecast",
                "relativeLocation": { "properties": { "city": "Alma", "state": "KS" } }
            }
        })
    }

    fn forecast_fixture() -> serde_json::Value {
        serde_json::json!({
            "properties": {
                "updated": "2026-10-15T18:12:00+00:00",
                "periods": [
                    { "name": "This Afternoon", "temperature": 78, "temperatureUnit": "F",
                      "windSpeed": "15 to 20 mph", "windDirection": "SW",
                      "shortForecast": "Mostly Sunny", "isDaytime": true,
                      "startTime": "2026-10-15T13:00:00-05:00",
                      "detailedForecast": "Mostly sunny, with a high near 78. Southwest wind 15 to 20 mph." },
                    { "name": "Tonight", "temperature": 54, "temperatureUnit": "F",
                      "windSpeed": "5 to 15 mph", "windDirection": "SW",
                      "shortForecast": "Partly Cloudy", "isDaytime": false,
                      "startTime": "2026-10-15T18:00:00-05:00",
                      "detailedForecast": "Partly cloudy, with a low around 54." }
                ]
            }
        })
    }

    #[test]
    fn parses_points_and_periods() {
        let f = forecast_from_json(&points_fixture(), &forecast_fixture(), "TOP");
        assert_eq!(f.office, "TOP");
        assert_eq!(f.place, "Alma, KS");
        assert_eq!(f.grid, "62,46");
        assert!(f.updated.is_some());
        assert_eq!(f.periods.len(), 2);
        let p = f.current().unwrap();
        assert_eq!(p.name, "This Afternoon");
        assert_eq!(p.temperature, 78.0);
        assert_eq!(p.unit, "F");
        assert_eq!(p.wind, "SW 15 to 20 mph");
        assert_eq!(p.short, "Mostly Sunny");
        assert!(p.is_daytime);
        assert!(p.detailed.contains("high near 78"));
        assert!(!f.periods[1].is_daytime);
    }

    /// Missing pieces degrade instead of panicking: no place name, no
    /// periods, unparseable times.
    #[test]
    fn tolerates_missing_fields() {
        let f = forecast_from_json(&serde_json::json!({}), &serde_json::json!({}), "");
        assert!(f.place.is_empty() && f.periods.is_empty() && f.updated.is_none());
        assert!(f.current().is_none());

        // A period without a name is skipped; with a name it survives even
        // when temperature/time are bogus.
        let fc = serde_json::json!({
            "properties": { "periods": [
                { "temperature": 70 },
                { "name": "Tonight", "startTime": "not-a-time" }
            ]}
        });
        let f = forecast_from_json(&serde_json::json!({}), &fc, "OUN");
        assert_eq!(f.periods.len(), 1);
        assert_eq!(f.periods[0].name, "Tonight");
        assert!(f.periods[0].temperature.is_nan());
        assert!(f.periods[0].start <= Utc::now());
    }
}
