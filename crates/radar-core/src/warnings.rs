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
}
