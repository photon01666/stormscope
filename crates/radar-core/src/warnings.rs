//! NWS active-alert model (subset of the api.weather.gov CAP/GeoJSON
//! properties the app renders).

use chrono::{DateTime, Utc};

/// One active NWS alert with an optional polygon geometry.
#[derive(Clone, Debug)]
pub struct Warning {
    /// NWS alert ID (unique per event instance).
    pub id: String,
    /// Event name, e.g. "Tornado Warning", "Severe Thunderstorm Warning",
    /// "Flash Flood Warning", "Special Weather Statement".
    pub event: String,
    /// "Extreme" | "Severe" | "Moderate" | "Minor" | "Unknown".
    pub severity: String,
    /// "Expected" | "Immediate" | "Future" | "Past" | "Unknown".
    pub urgency: String,
    /// Expiration time (UTC).
    pub expires: DateTime<Utc>,
    /// Issuing WFO, e.g. "OUN".
    pub office: String,
    /// Short headline text for tooltips/list.
    pub headline: String,
    /// Full NOAA/CAP description text (multi-paragraph, `\n` separated).
    pub description: String,
    /// Public-safety instruction from NOAA ("TAKE COVER NOW…"), often empty.
    pub instruction: String,
    /// Affected counties/areas as reported, e.g. "Cleveland, OK; McClain, OK".
    pub area: String,
    /// Polygon rings as (lon, lat) pairs; None for zone-based alerts.
    /// First ring is the exterior; typically only one ring from LSR/FFW
    /// polygons.
    pub polygon: Option<Vec<Vec<(f64, f64)>>>,
}

impl Warning {
    /// Mean position of the exterior ring, if the alert has a polygon.
    /// Used for map focus and distance-to-radar filtering.
    pub fn centroid(&self) -> Option<(f64, f64)> {
        let ring = self.polygon.as_ref()?.first()?;
        if ring.is_empty() {
            return None;
        }
        let n = ring.len() as f64;
        let (sum_lon, sum_lat) = ring.iter().fold((0.0f64, 0.0f64), |(sl, sa), (lon, lat)| {
            (sl + lon, sa + lat)
        });
        Some((sum_lat / n, sum_lon / n))
    }

    /// Convenience: is this a high-priority warning (tornado/severe T-storm
    /// with tornado tag/flash flood)? Used for UI coloring priority.
    pub fn is_tornado(&self) -> bool {
        self.event.contains("Tornado")
    }
    pub fn is_severe_thunderstorm(&self) -> bool {
        self.event.contains("Severe Thunderstorm")
    }
    pub fn is_flash_flood(&self) -> bool {
        self.event.contains("Flash Flood")
    }

    /// The short-fused, storm-based winter products (the ones the radar can
    /// actually see: snow squalls, blizzards, ice storms). Freeze/frost
    /// products are excluded on purpose — they are not storms, and treating
    /// them like warnings would make the alerts badge cry wolf all winter.
    pub fn is_winter_storm(&self) -> bool {
        !self.event.contains("Advisory")
            && [
                "Winter Storm",
                "Ice Storm",
                "Blizzard",
                "Snow Squall",
                "Lake Effect Snow",
                "Heavy Snow",
            ]
            .iter()
            .any(|family| self.event.contains(family))
    }

    /// Winter-weather advisories and similar lower-urgency winter products
    /// (Winter Weather, Lake Effect Snow, Freezing Rain, Wind Chill, Frost).
    /// They get the winter color at advisory weight — visible on the map as
    /// winter — but they never pulse the alerts badge: an advisory is not a
    /// storm.
    pub fn is_winter_advisory(&self) -> bool {
        self.event.contains("Advisory")
            && [
                "Winter",
                "Snow",
                "Ice",
                "Freez",
                "Frost",
                "Wind Chill",
                "Lake Effect",
            ]
            .iter()
            .any(|family| self.event.contains(family))
    }
}
