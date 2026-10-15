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
