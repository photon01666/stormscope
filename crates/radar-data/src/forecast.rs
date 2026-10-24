//! NWS gridpoint forecasts via `api.weather.gov`.
//!
//! Two requests per forecast, per the API's own design: `points/{lat},{lon}`
//! resolves a coordinate to a forecast office grid and hands back the
//! forecast URL, which is then fetched. Requests need the same descriptive
//! `User-Agent` as the alerts endpoint.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use radar_core::forecast::{Forecast, ForecastPeriod, Observation};

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
        let points = self.points(lat, lon)?;
        let forecast_url = points
            .pointer("/properties/forecast")
            .and_then(|v| v.as_str())
            .context("points response had no forecast URL (point outside NWS coverage?)")?;
        let fc: serde_json::Value = self.get_json(forecast_url)?;
        Ok(forecast_from_json(&points, &fc, &office_from(forecast_url)))
    }

    /// Current conditions at the nearest reporting station.
    pub fn observation_for_point(&self, lat: f64, lon: f64) -> Result<Observation> {
        let points = self.points(lat, lon)?;
        let obs_url = points
            .pointer("/properties/observationStations")
            .and_then(|v| v.as_str())
            .context("points response listed no observation stations")?;
        self.observation_from_stations_url(obs_url)
    }

    /// Both in one go, sharing the gridpoint lookup: the forecast, and the
    /// current conditions when a nearby station reports them. Observation
    /// failures are non-fatal (many points sit between stations).
    /// Current UV index for a point from Open-Meteo (NWS has no UV feed).
    /// Free, no API key; one tiny request.
    pub fn uv_index(&self, lat: f64, lon: f64) -> Result<Option<f32>> {
        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={lat:.4}&longitude={lon:.4} \
             &current=uv_index"
        );
        let resp: serde_json::Value = self
            .client
            .get(url)
            .send()
            .context("requesting Open-Meteo UV index")?
            .json()
            .context("parsing Open-Meteo UV index")?;
        Ok(resp
            .pointer("/current/uv_index")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32))
    }

    pub fn snapshot(&self, lat: f64, lon: f64) -> (Result<Forecast>, Option<Result<Observation>>) {
        let points = match self.points(lat, lon) {
            Ok(p) => p,
            Err(e) => return (Err(e), None),
        };
        let forecast = (|| {
            let url = points
                .pointer("/properties/forecast")
                .and_then(|v| v.as_str())
                .context("points response had no forecast URL")?;
            Ok(forecast_from_json(
                &points,
                &self.get_json(url)?,
                &office_from(url),
            ))
        })();
        let observation = points
            .pointer("/properties/observationStations")
            .and_then(|v| v.as_str())
            .map(|obs_url| self.observation_from_stations_url(obs_url));
        (forecast, observation)
    }

    fn observation_from_stations_url(&self, obs_url: &str) -> Result<Observation> {
        let stations = self.get_json(obs_url)?;
        Ok(observation_for_station(&stations, &mut |url: &str| {
            self.get_json(url)
        })?)
    }

    /// Rain/snow totals for up to `max_stations` stations reporting *within
    /// `radius_km` of the point* — one `/points` lookup, one station list,
    /// then one observation *series* per station (the series carries both
    /// the rolling buckets and the hourly accumulations an event total is
    /// summed from).
    ///
    /// The radius keeps the pull inside the selected station's viewing area:
    /// only stations whose geometry puts them inside are requested at all,
    /// so nothing outside the area is fetched. Failures are per-station — a
    /// station that will not answer is skipped, because "totals for seven of
    /// eight stations" beats none.
    pub fn station_totals(
        &self,
        lat: f64,
        lon: f64,
        max_stations: usize,
        radius_km: f64,
    ) -> Result<Vec<StationTotals>> {
        let points = self.points(lat, lon)?;
        let stations_url = points
            .pointer("/properties/observationStations")
            .and_then(|v| v.as_str())
            .context("points response listed no observation stations")?;
        let list = self.get_json(stations_url)?;
        let mut out = Vec::new();
        for (url, id, name, geo) in nearest_stations(&list, lat, lon, max_stations, radius_km) {
            let (lat_opt, lon_opt) = match geo {
                Some((la, lo)) => (Some(la), Some(lo)),
                None => (None, None),
            };
            match self.get_json(&format!("{url}/observations?limit=80")) {
                Ok(series) => out.push(totals_from_series(&series, &id, &name, lat_opt, lon_opt)),
                Err(e) => log::debug!("totals for {id}: {e:#}"),
            }
        }
        Ok(out)
    }

    /// The `/points/{lat},{lon}` lookup both responses hang off.
    fn points(&self, lat: f64, lon: f64) -> Result<serde_json::Value> {
        self.get_json(&format!("{POINTS_URL}/{lat:.4},{lon:.4}"))
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

/// One station's precipitation/snow picture, from its observation series.
#[derive(Debug, Clone, PartialEq)]
pub struct StationTotals {
    pub station_id: String,
    pub station_name: String,
    /// Where the station is, when the listing carried geometry — the map
    /// overlay plots it (stations without geometry simply stay off-map).
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    /// Time of the newest observation in the series.
    pub last_obs: Option<DateTime<Utc>>,
    /// Newest non-null value per hydrometeor property: (label, inches) —
    /// `"rain · last hour"`, `"snow depth"`, … whatever the station reports.
    pub buckets: Vec<(String, f64)>,
    /// Rolling one-hour accumulations summed over the loaded window:
    /// (label, inches). Samples are taken at least 50 minutes apart, so a
    /// 5-minute station's overlapping hour-windows do not double-count.
    pub hourly_sums: Vec<(String, f64)>,
    /// Hours the sums above cover, and how many observations went into them.
    pub window_hours: f64,
    pub obs_count: usize,
}

/// Property keys that are precipitation, rain, snow or ice (and not, say,
/// dew point).
fn is_hydrometeor(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.contains("precip") || k.contains("rain") || k.contains("snow") || k.contains("ice")
}

/// camelCase → words: `precipitationLastHour` → ["precipitation", "Last", "Hour"].
fn camel_words(key: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    for (i, c) in key.chars().enumerate() {
        let starts_word = !cur.is_empty()
            && i > 0
            && (c.is_uppercase()
                // Digits open their own word so "Last24Hours" splits as
                // Last | 24 | Hours rather than keeping "Last24".
                || (c.is_ascii_digit() && key.as_bytes()[i - 1].is_ascii_alphabetic()));
        if starts_word {
            words.push(std::mem::take(&mut cur));
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

/// Human label for a hydrometeor property: `precipitationLastHour` →
/// "rain · last hour", `snowDepth` → "snow depth", `snowfallLast6Hours` →
/// "snow · last 6 hours". "hour" becomes "hours" only after digits ("24
/// hour" → "24 hours", but "last hour" stays singular).
pub fn hydrometeor_label(key: &str) -> String {
    let words = camel_words(key);
    if words.len() == 2
        && words[0].eq_ignore_ascii_case("snow")
        && words[1].eq_ignore_ascii_case("depth")
    {
        return "snow depth".to_string();
    }
    let metric = match words.first().map(String::as_str) {
        Some("precipitation") => "rain",
        Some("snowfall") | Some("snow") => "snow",
        Some("ice") => "ice",
        _ => {
            return words
                .iter()
                .map(|w| w.to_lowercase())
                .collect::<Vec<_>>()
                .join(" ");
        }
    };
    let mut rest: Vec<String> = words[1..].iter().map(|w| w.to_lowercase()).collect();
    // A trailing "hour" *after digits* is a span ("24 Hour" → "24 hours");
    // "last Hour" keeps its singular.
    for i in 1..rest.len() {
        if rest[i] == "hour" && rest[i - 1].chars().all(|c| c.is_ascii_digit()) {
            rest[i] = "hours".to_string();
        }
    }
    if rest.is_empty() {
        metric.to_string()
    } else {
        format!("{metric} · {}", rest.join(" "))
    }
}

/// Value in inches: NWS precip/depth is millimetres (`wmoUnit:mm`); anything
/// else passes through untouched (it is already in the unit asked for).
fn in_inches(value: f64, unit_code: &str) -> f64 {
    if unit_code.contains("mm") {
        value / 25.4
    } else {
        value
    }
}

/// Parse an `observations` series into a station's totals. Series arrives
/// newest-first; the hourly accumulation walk sorts ascending so it can take
/// samples at least 50 minutes apart.
pub(crate) fn totals_from_series(
    series: &serde_json::Value,
    station_id: &str,
    station_name: &str,
    lat: Option<f64>,
    lon: Option<f64>,
) -> StationTotals {
    let mut obs: Vec<(DateTime<Utc>, &serde_json::Map<String, serde_json::Value>)> = Vec::new();
    for f in series
        .pointer("/features")
        .and_then(|v| v.as_array())
        .map(|v| v.as_slice())
        .unwrap_or(&[])
    {
        let p = f.pointer("/properties");
        let Some(ts) = p
            .and_then(|p| p.pointer("/timestamp"))
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc))
        else {
            continue;
        };
        if let Some(props) = p.and_then(|p| p.as_object()) {
            obs.push((ts, props));
        }
    }
    obs.sort_by(|a, b| b.0.cmp(&a.0)); // newest first

    let mut buckets: Vec<(String, f64)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (_, props) in &obs {
        for (key, val) in props.iter() {
            if !is_hydrometeor(key) || seen.contains(key.as_str()) {
                continue;
            }
            let value = val.get("value").and_then(|v| v.as_f64());
            let Some(value) = value else { continue };
            let unit = val.get("unitCode").and_then(|v| v.as_str()).unwrap_or("");
            seen.insert(key.clone());
            buckets.push((hydrometeor_label(key), in_inches(value, unit)));
        }
    }

    // Rolling one-hour fields, summed at ≥50-minute spacing (denser cadences
    // report overlapping windows; hourly cadences are taken every sample).
    // (label, summed inches, hours covered)
    let mut hourly_sums: Vec<(String, f64, f64)> = Vec::new();
    let newest = obs.first().map(|(t, _)| *t);
    let mut hour_keys: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for (_, props) in &obs {
        for (key, val) in props.iter() {
            if !key.ends_with("LastHour") {
                continue;
            }
            let Some(value) = val.get("value").and_then(|v| v.as_f64()) else {
                continue;
            };
            hour_keys.entry(key.clone()).or_insert_with(|| {
                in_inches(
                    value,
                    val.get("unitCode").and_then(|v| v.as_str()).unwrap_or(""),
                )
            });
        }
    }
    for (key, _) in &hour_keys {
        let mut last: Option<DateTime<Utc>> = None;
        let mut sum = 0.0;
        let mut oldest: Option<DateTime<Utc>> = None;
        for (ts, props) in obs.iter().rev() {
            // walk oldest → newest
            let Some(val) = props.get(key) else { continue };
            let Some(value) = val.get("value").and_then(|v| v.as_f64()) else {
                continue;
            };
            if last.is_some_and(|l| (*ts - l).num_minutes() < 50) {
                continue;
            }
            last = Some(*ts);
            oldest.get_or_insert(*ts);
            sum += in_inches(
                value,
                val.get("unitCode").and_then(|v| v.as_str()).unwrap_or(""),
            );
        }
        if last.is_some() {
            let label = format!(
                "{} · hourly sum",
                hydrometeor_label(key).replace(" · last hour", "")
            );
            let span = match (oldest, newest) {
                (Some(o), Some(n)) => (n - o).num_seconds() as f64 / 3600.0,
                _ => 0.0,
            };
            hourly_sums.push((label, sum, span));
        }
    }
    let window_hours = hourly_sums
        .iter()
        .map(|(_, _, h)| *h)
        .fold(0.0_f64, f64::max);
    let hourly_sums: Vec<(String, f64)> = hourly_sums.into_iter().map(|(l, v, _)| (l, v)).collect();

    StationTotals {
        station_id: station_id.to_string(),
        station_name: station_name.to_string(),
        lat,
        lon,
        last_obs: newest,
        buckets,
        hourly_sums,
        window_hours,
        obs_count: obs.len(),
    }
}

/// The `max_stations` stations nearest (lat, lon) as (url, id, name),
/// nearest first. Stations without geometry keep their listing order after
/// the geolocated ones — the list is one request either way, so sorting
/// costs nothing and the fetch count stays bounded.
pub(crate) fn nearest_stations(
    list: &serde_json::Value,
    lat: f64,
    lon: f64,
    max_stations: usize,
    radius_km: f64,
) -> Vec<(String, String, String, Option<(f64, f64)>)> {
    let Some(features) = list
        .pointer("/features")
        .and_then(|v| v.as_array())
        .map(|v| v.as_slice())
    else {
        return Vec::new();
    };
    // (distance, url, id, name, geometry)
    let mut with_geo: Vec<(f64, String, String, String, Option<(f64, f64)>)> = Vec::new();
    let mut without_geo: Vec<(String, String, String, Option<(f64, f64)>)> = Vec::new();
    for f in features {
        let Some(url) = f.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let props = f.get("properties");
        let name = props
            .and_then(|p| p.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let id = props
            .and_then(|p| p.get("stationIdentifier"))
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| url.rsplit('/').next().unwrap_or(""))
            .to_string();
        let coords = f
            .pointer("/geometry/coordinates")
            .and_then(|c| c.as_array())
            .and_then(|c| Some((c.first()?.as_f64()?, c.get(1)?.as_f64()?)));
        match coords {
            Some((slon, slat)) => with_geo.push((
                radar_core::sites::haversine_km(lat, lon, slat, slon),
                url.to_string(),
                id,
                name,
                Some((slat, slon)),
            )),
            None => without_geo.push((url.to_string(), id, name, None)),
        }
    }
    with_geo.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    // The viewing area is the contract: a geolocated station must fall inside
    // the radius, and one without geometry cannot prove that it does — so it
    // is dropped rather than fetched speculatively.
    with_geo
        .into_iter()
        .filter(|(d, ..)| *d <= radius_km)
        .map(|(_, u, i, n, g)| (u, i, n, g))
        .take(max_stations)
        .collect()
}

/// Forecast office from a gridpoint URL (the path segment before the grid).
fn office_from(forecast_url: &str) -> String {
    forecast_url
        .split("/gridpoints/")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("")
        .to_string()
}

/// Pick the first station out of a `/points/{lat},{lon}`-listed station
/// collection and fetch its latest observation. Separated from the network
/// so tests can drive it with canned JSON.
fn observation_for_station(
    stations: &serde_json::Value,
    fetch: &mut dyn FnMut(&str) -> Result<serde_json::Value>,
) -> Result<Observation> {
    let station_url = stations
        .pointer("/features/0/id")
        .and_then(|v| v.as_str())
        .context("station list was empty")?;
    let station_name = stations
        .pointer("/features/0/properties/name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let station_id = stations
        .pointer("/features/0/properties/stationIdentifier")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| station_url.rsplit('/').next().unwrap_or(""))
        .to_string();
    let latest = fetch(&format!("{station_url}/observations/latest"))?;
    Ok(observation_from_json(&latest, &station_id, &station_name))
}

/// Build an [`Observation`] from a `/stations/{id}/observations/latest`
/// response: every measurement arrives as `{"value": .., "unitCode": ..}` or
/// `null`, so each one is optional.
pub(crate) fn observation_from_json(
    latest: &serde_json::Value,
    station_id: &str,
    station_name: &str,
) -> Observation {
    let p = latest.pointer("/properties");
    let num = |key: &str| -> Option<f32> {
        p.and_then(|p| p.pointer(&format!("/{key}/value")))
            .and_then(|v| v.as_f64())
            .map(|v| v as f32)
    };
    Observation {
        station_id: station_id.to_string(),
        station_name: station_name.to_string(),
        timestamp: p
            .and_then(|p| p.pointer("/timestamp"))
            .and_then(|v| v.as_str())
            .and_then(parse_time),
        text: p
            .and_then(|p| p.pointer("/textDescription"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        temperature_c: num("temperature"),
        dewpoint_c: num("dewpoint"),
        wind_speed_kmh: num("windSpeed"),
        wind_direction_deg: num("windDirection"),
        wind_gust_kmh: num("windGust"),
        humidity_pct: num("relativeHumidity"),
        pressure_pa: num("barometricPressure"),
        sea_level_pressure_pa: num("seaLevelPressure"),
        visibility_m: num("visibility"),
        precip_last_hour_mm: num("precipitationLastHour"),
        precip_3h_mm: num("precipitationLast3Hours"),
        precip_6h_mm: num("precipitationLast6Hours"),
        sky: sky_summary(p),
        cloud_cover_pct: cloud_cover(p),
        cloud_bases_ft: cloud_bases(p),
        heat_index_c: num("heatIndex"),
        wind_chill_c: num("windChill"),
    }
}

/// NWS cloud-layer amount codes -> percent coverage.
fn cloud_amount_pct(amount: &str) -> Option<f32> {
    match amount {
        "CLR" | "SKC" | "NCD" | "NSC" => Some(0.0),
        "FEW" => Some(25.0),
        "SCT" => Some(50.0),
        "BKN" => Some(75.0),
        "OVC" | "VV" => Some(100.0),
        _ => None,
    }
}

/// Human sky summary + coverage + cloud bases (feet) from `cloudLayers`.
fn cloud_cover(p: Option<&serde_json::Value>) -> Option<f32> {
    let layers = p?.pointer("/cloudLayers")?.as_array()?;
    layers
        .iter()
        .filter_map(|l| l.get("amount").and_then(|v| v.as_str()))
        .filter_map(cloud_amount_pct)
        .fold(None, |acc: Option<f32>, v| {
            Some(acc.map_or(v, |a| a.max(v)))
        })
}

fn cloud_bases(p: Option<&serde_json::Value>) -> Vec<f32> {
    p.and_then(|p| p.pointer("/cloudLayers"))
        .and_then(|v| v.as_array())
        .map(|layers| {
            layers
                .iter()
                .filter_map(|l| l.get("base").and_then(|v| v.as_f64()))
                .map(|m| m as f32 * 3.280_84)
                .collect()
        })
        .unwrap_or_default()
}

fn sky_summary(p: Option<&serde_json::Value>) -> String {
    let Some(layers) = p
        .and_then(|p| p.pointer("/cloudLayers"))
        .and_then(|v| v.as_array())
    else {
        return String::new();
    };
    let mut best = 0.0f32;
    let mut base_ft: Option<f32> = None;
    for layer in layers {
        let Some(amount) = layer.get("amount").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(pct) = cloud_amount_pct(amount) else {
            continue;
        };
        if pct >= best {
            best = pct;
            base_ft = layer
                .get("base")
                .and_then(|v| v.as_f64())
                .map(|m| m as f32 * 3.280_84);
        }
    }
    let word = match best as u32 {
        0 => "Clear",
        25 => "Few clouds",
        50 => "Scattered",
        75 => "Broken",
        100 => "Overcast",
        _ => return String::new(),
    };
    match base_ft {
        Some(base) if base.is_finite() => format!("{word} {} ft", (base / 100.0).round() * 100.0),
        _ => word.to_string(),
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

    /// Current conditions: the station lookup picks the first station and the
    /// latest observation's optional measurements survive nulls and units.
    #[test]
    fn parses_current_conditions_from_a_station() {
        let stations = serde_json::json!({
            "features": [{
                "id": "https://api.weather.gov/stations/KTOP",
                "properties": { "stationIdentifier": "KTOP", "name": "Topeka, Philip Billard Municipal Airport" }
            }]
        });
        let latest = serde_json::json!({
            "properties": {
                "timestamp": "2026-10-15T21:53:00+00:00",
                "textDescription": "Light Rain",
                "temperature": { "value": 18.3, "unitCode": "wmoUnit:degC" },
                "dewpoint": { "value": 16.1, "unitCode": "wmoUnit:degC" },
                "windSpeed": { "value": 22.2, "unitCode": "wmoUnit:km_h-1" },
                "windDirection": { "value": 140, "unitCode": "wmoUnit:degree_(angle)" },
                "windGust": { "value": null, "unitCode": "wmoUnit:km_h-1" },
                "relativeHumidity": { "value": 88.0, "unitCode": "wmoUnit:percent" },
                "barometricPressure": { "value": 100820.0, "unitCode": "wmoUnit:Pa" },
                "visibility": { "value": 16093.44, "unitCode": "wmoUnit:m" },
                "precipitationLastHour": { "value": 2.5, "unitCode": "wmoUnit:mm" },
                "heatIndex": { "value": null, "unitCode": "wmoUnit:degC" },
                "windChill": { "value": null, "unitCode": "wmoUnit:degC" }
            }
        });
        let calls = std::cell::RefCell::new(Vec::new());
        let obs = observation_for_station(&stations, &mut |url: &str| {
            calls.borrow_mut().push(url.to_string());
            Ok(latest.clone())
        })
        .expect("observation");
        assert_eq!(obs.station_id, "KTOP");
        assert!(obs.station_name.contains("Topeka"));
        assert_eq!(obs.text, "Light Rain");
        assert_eq!(obs.temperature_c, Some(18.3));
        assert_eq!(obs.dewpoint_c, Some(16.1));
        assert_eq!(obs.wind_speed_kmh, Some(22.2));
        assert_eq!(obs.wind_direction_deg, Some(140.0));
        assert_eq!(obs.wind_gust_kmh, None, "null measurement stays None");
        assert_eq!(obs.precip_last_hour_mm, Some(2.5));
        assert!(obs.timestamp.is_some());
        assert_eq!(
            calls.borrow()[0],
            "https://api.weather.gov/stations/KTOP/observations/latest"
        );
    }

    /// No stations listed (or an empty collection) is an error, not a panic,
    /// and a station that reports nothing still yields an observation shell.
    #[test]
    fn current_conditions_degrade_gracefully() {
        let empty = serde_json::json!({ "features": [] });
        let mut never = |_: &str| -> Result<serde_json::Value> { unreachable!() };
        assert!(observation_for_station(&empty, &mut never).is_err());

        let stations = serde_json::json!({
            "features": [{ "id": "https://api.weather.gov/stations/KABC",
                           "properties": { "stationIdentifier": "KABC" } }]
        });
        let bare = serde_json::json!({ "properties": {} });
        let obs = observation_for_station(&stations, &mut |_| Ok(bare.clone())).unwrap();
        assert_eq!(obs.station_id, "KABC");
        assert!(obs.text.is_empty() && obs.temperature_c.is_none() && obs.timestamp.is_none());
    }

    /// Missing pieces degrade instead of panicking: no place name, no
    /// periods, unparseable times.
    /// Hydrometeor keys read as labels the way an operator would say them.
    #[test]
    fn hydrometeor_labels_read_naturally() {
        assert_eq!(
            hydrometeor_label("precipitationLastHour"),
            "rain · last hour"
        );
        assert_eq!(hydrometeor_label("precipitation24Hour"), "rain · 24 hours");
        assert_eq!(
            hydrometeor_label("precipitationLast24Hours"),
            "rain · last 24 hours"
        );
        assert_eq!(hydrometeor_label("snowDepth"), "snow depth");
        assert_eq!(
            hydrometeor_label("snowfallLast6Hours"),
            "snow · last 6 hours"
        );
        assert_eq!(
            hydrometeor_label("dewpointTemperature"),
            "dewpoint temperature"
        );
    }

    /// Totals from a real-shaped series: newest-first buckets (mm → in),
    /// hourly sums taken at ≥50-minute spacing so overlapping windows do not
    /// double-count, and a window spanning what was summed.
    #[test]
    fn totals_from_series_buckets_sums_and_spans() {
        let series: serde_json::Value = serde_json::from_str(
            r#"{"features":[
            {"properties":{"timestamp":"2026-10-16T01:35:00Z",
              "precipitationLastHour":{"value":12.7,"unitCode":"wmoUnit:mm"},
              "snowDepth":{"value":25.4,"unitCode":"wmoUnit:mm"},
              "dewpoint":{"value":10.0,"unitCode":"wmoUnit:degC"}}},
            {"properties":{"timestamp":"2026-10-16T01:05:00Z",
              "precipitationLastHour":{"value":2.54,"unitCode":"wmoUnit:mm"}}},
            {"properties":{"timestamp":"2026-10-16T00:25:00Z",
              "precipitationLastHour":{"value":0.0,"unitCode":"wmoUnit:mm"}}},
            {"properties":{"timestamp":"2026-10-16T00:05:00Z",
              "precipitationLastHour":{"value":5.08,"unitCode":"wmoUnit:mm"}}}]}"#,
        )
        .unwrap();
        let t = totals_from_series(&series, "KTOP", "Topeka", Some(39.07), Some(-95.69));
        assert_eq!(t.station_id, "KTOP");
        assert_eq!(t.lat, Some(39.07), "coords carried for the map overlay");
        assert_eq!(t.lon, Some(-95.69));
        assert_eq!(t.obs_count, 4);
        assert_eq!(
            t.last_obs.unwrap().to_rfc3339(),
            "2026-10-16T01:35:00+00:00"
        );
        assert_eq!(t.buckets.len(), 2, "dewpoint is not a hydrometeor");
        assert_eq!(t.buckets[0].0, "rain · last hour");
        assert!((t.buckets[0].1 - 0.5).abs() < 1e-6, "{}", t.buckets[0].1);
        assert_eq!(t.buckets[1].0, "snow depth");
        assert!((t.buckets[1].1 - 1.0).abs() < 1e-6);
        // 00:05 and 01:05 taken (00:25, 01:35 are <50 min after them):
        // 5.08 + 2.54 mm = 7.62 mm = 0.30 in over a 1.5 h span.
        assert_eq!(t.hourly_sums.len(), 1);
        assert_eq!(t.hourly_sums[0].0, "rain · hourly sum");
        assert!(
            (t.hourly_sums[0].1 - 0.3).abs() < 1e-6,
            "{}",
            t.hourly_sums[0].1
        );
        assert!((t.window_hours - 1.5).abs() < 1e-6, "{}", t.window_hours);
    }

    /// Stations sort nearest-first when geometry is present, geometry-less
    /// stations still come through, and max_stations bounds the fetches.
    #[test]
    fn nearest_stations_sorts_and_caps() {
        let list: serde_json::Value = serde_json::from_str(
            r#"{"features":[
            {"id":"https://api.weather.gov/stations/FAR","properties":{"stationIdentifier":"KFAR","name":"Far Away"},
             "geometry":{"coordinates":[-97.4,46.0]}},
            {"id":"https://api.weather.gov/stations/NEAR","properties":{"stationIdentifier":"KNEAR","name":"Near"},
             "geometry":{"coordinates":[-97.0,38.1]}},
            {"id":"https://api.weather.gov/stations/NOGEO","properties":{"stationIdentifier":"KNOGEO","name":"No Geo"},
             "geometry":null}]}"#,
        )
        .unwrap();
        // Generous radius: both geolocated stations, geometry-less dropped
        // (it cannot prove it is inside the area).
        let picked = nearest_stations(&list, 38.0, -97.0, 10, 10_000.0);
        let ids: Vec<&str> = picked.iter().map(|(_, i, _, _)| i.as_str()).collect();
        assert_eq!(ids, vec!["KNEAR", "KFAR"]);
        assert_eq!(picked[0].3, Some((38.1, -97.0)), "geometry kept");
        // KFAR is ~880 km away: outside a 500 km viewing area it is not
        // requested at all.
        let in_area = nearest_stations(&list, 38.0, -97.0, 10, 500.0);
        let ids: Vec<&str> = in_area.iter().map(|(_, i, _, _)| i.as_str()).collect();
        assert_eq!(ids, vec!["KNEAR"], "radius scopes the pull");
        let capped = nearest_stations(&list, 38.0, -97.0, 1, 10_000.0);
        assert_eq!(capped.len(), 1, "max_stations bounds the fetch count");
        assert!(nearest_stations(&serde_json::json!({}), 0.0, 0.0, 3, 100.0).is_empty());
    }

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
