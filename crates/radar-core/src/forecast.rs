//! NWS gridpoint forecast model (subset of `api.weather.gov`'s `points` ->
//! `forecast` responses the app renders).

use chrono::{DateTime, Utc};

/// One forecast period (NWS splits a day into day/night periods).
#[derive(Clone, Debug)]
pub struct ForecastPeriod {
    /// "Today", "Tonight", "Monday", …
    pub name: String,
    /// Temperature in [`Self::unit`].
    pub temperature: f32,
    /// "F" or "C" as reported.
    pub unit: String,
    /// Wind summary, e.g. "NW 10 to 15 mph".
    pub wind: String,
    /// One-line summary, e.g. "Partly Sunny then Slight Chance Showers".
    pub short: String,
    /// Multi-sentence narrative forecast.
    pub detailed: String,
    /// Period start (UTC).
    pub start: DateTime<Utc>,
    /// Is this the daytime half of the period?
    pub is_daytime: bool,
}

/// A forecast for one point: what the API resolved it to, plus its periods.
#[derive(Clone, Debug)]
pub struct Forecast {
    /// Issuing Weather Forecast Office grid, e.g. "TOP" (Topeka).
    pub office: String,
    /// The API's own label for the location, e.g. "Alma, KS".
    pub place: String,
    /// Gridpoint coordinates the API resolved, for transparency.
    pub grid: String,
    /// When the NWS last updated the gridpoint data, if reported.
    pub updated: Option<DateTime<Utc>>,
    /// Day/night periods, soonest first.
    pub periods: Vec<ForecastPeriod>,
}

impl Forecast {
    /// The period covering now, if the list has one that has not ended.
    pub fn current(&self) -> Option<&ForecastPeriod> {
        self.periods.first()
    }
}

/// A current surface observation (the nearest NWS station's latest report).
///
/// Values are in the units the API reports (SI: °C, km/h, Pa, m); the UI
/// converts for display. `None` means the station did not report that field.
#[derive(Clone, Debug)]
pub struct Observation {
    /// Station id, e.g. "KTOP".
    pub station_id: String,
    /// Station name, e.g. "Topeka, Philip Billard Municipal Airport".
    pub station_name: String,
    /// Observation time (UTC).
    pub timestamp: Option<DateTime<Utc>>,
    /// Short text, e.g. "Light Rain", "Overcast".
    pub text: String,
    pub temperature_c: Option<f32>,
    pub dewpoint_c: Option<f32>,
    pub wind_speed_kmh: Option<f32>,
    /// Direction the wind blows *from*, degrees.
    pub wind_direction_deg: Option<f32>,
    pub wind_gust_kmh: Option<f32>,
    pub humidity_pct: Option<f32>,
    pub pressure_pa: Option<f32>,
    pub visibility_m: Option<f32>,
    /// Precipitation over the last hour, mm.
    pub precip_last_hour_mm: Option<f32>,
    pub heat_index_c: Option<f32>,
    pub wind_chill_c: Option<f32>,
}
