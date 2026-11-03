//! WPC Winter Storm Severity Index (WSSI) fetch and parse.
//!
//! Source: `https://mapservices.weather.noaa.gov/vector/rest/services/outlooks/wpc_wssi/MapServer`
//! (NOAA/NWS/WPC, public). Each WSSI component/period is one ArcGIS query
//! returning GeoJSON; polygons carry an `impact` attribute from WINTER WEATHER
//! AREA through EXTREME. The service updates every two hours.

use anyhow::{Context, Result, bail};
use radar_core::wssi::{WssiComponent, WssiFeature, WssiImpact, WssiLayer, WssiPeriod};

const BASE: &str =
    "https://mapservices.weather.noaa.gov/vector/rest/services/outlooks/wpc_wssi/MapServer";

#[derive(Clone)]
pub struct WssiSource {
    client: reqwest::blocking::Client,
}

impl WssiSource {
    pub fn new(user_agent: &str) -> Result<Self> {
        let ua = user_agent.trim();
        if ua.len() < 8 {
            bail!("WPC requests need a descriptive User-Agent; got {user_agent:?}");
        }
        let client = reqwest::blocking::Client::builder()
            .user_agent(ua)
            .timeout(std::time::Duration::from_secs(25))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(Self { client })
    }

    /// Fetch one WSSI component/period layer.
    pub fn fetch(&self, component: WssiComponent, period: WssiPeriod) -> Result<WssiLayer> {
        let url = format!("{BASE}/{}/query", component.layer_id(period));
        let resp = self
            .client
            .get(&url)
            .query(&[
                ("where", "1=1"),
                ("outFields", "impact,valid_time,issue_time"),
                ("returnGeometry", "true"),
                ("outSR", "4326"),
                ("f", "geojson"),
                // WSSI is a CONUS-wide polygon product with very dense
                // boundaries; ~0.01° still looks smooth at radar zoom and
                // cuts the payload from megabytes to ~100 kB per layer.
                ("maxAllowableOffset", "0.01"),
                ("geometryPrecision", "4"),
            ])
            .header("Accept", "application/geo+json")
            .send()
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            bail!("WPC WSSI returned HTTP {status}");
        }
        let fc: serde_json::Value = resp.json().context("parsing WPC WSSI GeoJSON")?;
        parse_wssi(component, period, &fc)
    }
}

/// Parse a WSSI FeatureCollection. Separated from the network call so tests
/// can run against canned GeoJSON.
pub fn parse_wssi(
    component: WssiComponent,
    period: WssiPeriod,
    fc: &serde_json::Value,
) -> Result<WssiLayer> {
    let features_json = fc
        .get("features")
        .and_then(|f| f.as_array())
        .context("WSSI: no `features` array")?;

    let mut features = Vec::new();
    let (mut issue, mut valid) = (String::new(), String::new());
    for feature in features_json {
        let Some(props) = feature.get("properties") else {
            continue;
        };
        let s = |key: &str| {
            props
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        if issue.is_empty() {
            issue = s("issue_time");
        }
        if valid.is_empty() {
            valid = s("valid_time");
        }
        let Some(impact) = WssiImpact::from_gis(&s("impact")) else {
            continue;
        };
        let polygons = geometry_polygons(feature.get("geometry"));
        if polygons.is_empty() {
            continue;
        }
        features.push(WssiFeature { impact, polygons });
    }

    // Least severe first so the strongest impact areas draw on top.
    features.sort_by_key(|f| f.impact.rank());
    Ok(WssiLayer {
        component,
        period,
        issue,
        valid,
        features,
    })
}

/// All polygons of a GeoJSON Polygon/MultiPolygon, each with its full ring
/// list ([exterior, hole, ...]), vertices as `(lon, lat)`.
fn geometry_polygons(geometry: Option<&serde_json::Value>) -> Vec<Vec<Vec<(f64, f64)>>> {
    let Some(geometry) = geometry else {
        return Vec::new();
    };
    let Some(coords) = geometry.get("coordinates") else {
        return Vec::new();
    };
    match geometry.get("type").and_then(|t| t.as_str()) {
        Some("Polygon") => one_polygon(coords).into_iter().collect(),
        Some("MultiPolygon") => coords
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(one_polygon)
            .collect(),
        _ => Vec::new(),
    }
}

fn one_polygon(polygon: &serde_json::Value) -> Option<Vec<Vec<(f64, f64)>>> {
    let mut rings = Vec::new();
    for ring in polygon.as_array()? {
        if let Some(r) = ring_from_json(ring)
            && r.len() >= 3
        {
            rings.push(r);
        }
    }
    if rings.is_empty() { None } else { Some(rings) }
}

fn ring_from_json(ring: &serde_json::Value) -> Option<Vec<(f64, f64)>> {
    let arr = ring.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for pos in arr {
        let p = pos.as_array()?;
        out.push((p.first()?.as_f64()?, p.get(1)?.as_f64()?));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_impacts_and_sorts_least_to_most_severe() {
        let fc = serde_json::json!({
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "properties": {
                        "impact": "MAJOR",
                        "issue_time": "2026-11-02 2115Z",
                        "valid_time": "21Z 11/02/26 - 18Z 11/05/26"
                    },
                    "geometry": {
                        "type": "Polygon",
                        "coordinates": [[[-100.0, 35.0], [-99.0, 35.0], [-99.0, 36.0], [-100.0, 36.0], [-100.0, 35.0]]]
                    }
                },
                {
                    "type": "Feature",
                    "properties": {"impact": "MINOR"},
                    "geometry": {
                        "type": "MultiPolygon",
                        "coordinates": [[[[-101.0, 34.0], [-100.5, 34.0], [-100.5, 34.5], [-101.0, 34.5], [-101.0, 34.0]]]]
                    }
                },
                {
                    "type": "Feature",
                    "properties": {"impact": "NOT A CATEGORY"},
                    "geometry": {
                        "type": "Polygon",
                        "coordinates": [[[-97.0, 33.0], [-96.0, 33.0], [-96.0, 34.0], [-97.0, 34.0], [-97.0, 33.0]]]
                    }
                }
            ]
        });
        let layer =
            parse_wssi(WssiComponent::Overall, WssiPeriod::Days1to3, &fc).expect("fixture parses");
        assert_eq!(layer.features.len(), 2, "unknown impact is skipped");
        assert_eq!(layer.features[0].impact, WssiImpact::Minor);
        assert_eq!(layer.features[1].impact, WssiImpact::Major);
        assert_eq!(layer.issue, "2026-11-02 2115Z");
        assert!(layer.features[1].polygons[0][0].len() >= 4);
    }

    #[test]
    fn empty_collection_is_a_valid_empty_layer() {
        let fc = serde_json::json!({"type": "FeatureCollection", "features": []});
        let layer = parse_wssi(WssiComponent::SnowAmount, WssiPeriod::Day2, &fc).unwrap();
        assert!(layer.features.is_empty());
        assert_eq!(layer.component, WssiComponent::SnowAmount);
        assert_eq!(layer.period, WssiPeriod::Day2);
    }
}
