//! SPC convective outlooks (categorical Day 1/2/3) from the free public
//! GeoJSON products — no API key, updated a few times per day.
//!
//! Products: `https://www.spc.noaa.gov/products/outlook/dayN_otlk_cat…`.
//! The GeoJSON is RFC-7946 (WGS84 lon/lat `Polygon`/`MultiPolygon`) and each
//! feature carries SPC's own `stroke`/`fill` hex colors plus `DN`, `LABEL`,
//! `LABEL2` and ISO issue/valid/expire timestamps.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use radar_core::outlooks::{Outlook, OutlookFeature};

const BASE: &str = "https://www.spc.noaa.gov/products/outlook";

pub struct OutlookSource {
    client: reqwest::blocking::Client,
}

impl OutlookSource {
    /// Build a source with a descriptive User-Agent (SPC, like all
    /// weather.gov hosts, prefers an identifying UA).
    pub fn new(user_agent: &str) -> Result<Self> {
        let ua = user_agent.trim();
        if ua.len() < 8 {
            bail!("SPC requests need a descriptive User-Agent; got {user_agent:?}");
        }
        let client = reqwest::blocking::Client::builder()
            .user_agent(ua)
            .timeout(std::time::Duration::from_secs(20))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(Self { client })
    }

    /// Fetch the categorical outlook for `day` (1..=3).
    pub fn fetch_day(&self, day: u8) -> Result<Outlook> {
        if !(1..=3).contains(&day) {
            bail!("outlook day must be 1..=3 (got {day})");
        }
        let url = format!("{BASE}/day{day}otlk_cat.nolyr.geojson");
        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/geo+json")
            .send()
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            bail!("SPC outlook day {day} returned HTTP {status}");
        }
        let fc: serde_json::Value = resp.json().context("parsing SPC outlook GeoJSON")?;
        parse_outlook(day, &fc)
    }
}

/// Parse an SPC categorical FeatureCollection into an [`Outlook`]. Separated
/// from the network call so tests can run on a canned fixture.
pub fn parse_outlook(day: u8, fc: &serde_json::Value) -> Result<Outlook> {
    let features_json = fc
        .get("features")
        .and_then(|f| f.as_array())
        .context("outlook: no `features` array")?;

    let mut features = Vec::new();
    let (mut issue, mut valid, mut expire) = (None, None, None);
    let mut forecaster = String::new();

    for f in features_json {
        let Some(props) = f.get("properties") else {
            continue;
        };
        let s = |k: &str| {
            props
                .get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let dn = props.get("DN").and_then(|v| v.as_u64()).unwrap_or(0) as u16;

        // Metadata is repeated on every feature; take the first non-empty.
        if issue.is_none() {
            issue = iso(&s("ISSUE_ISO"));
        }
        if valid.is_none() {
            valid = iso(&s("VALID_ISO"));
        }
        if expire.is_none() {
            expire = iso(&s("EXPIRE_ISO"));
        }
        if forecaster.is_empty() {
            forecaster = s("FORECASTER");
        }

        let polygons = geometry_polygons(f.get("geometry"));
        if polygons.is_empty() {
            continue; // no geometry => nothing to draw
        }
        features.push(OutlookFeature {
            dn,
            label: s("LABEL"),
            name: s("LABEL2"),
            stroke: hex_rgb(&s("stroke")).unwrap_or([180, 180, 180]),
            fill: hex_rgb(&s("fill")).unwrap_or([120, 120, 120]),
            polygons,
        });
    }

    // Least severe first so higher categories draw on top.
    features.sort_by_key(|f| f.dn);
    Ok(Outlook {
        day,
        issue,
        valid,
        expire,
        forecaster,
        features,
    })
}

/// All polygons of a GeoJSON Polygon/MultiPolygon geometry, each with its
/// full ring list (`[exterior, hole, ...]`), vertices as `(lon, lat)`.
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

/// One GeoJSON polygon (a list of rings) as `Vec<ring>`; `None` if unusable.
fn one_polygon(polygon: &serde_json::Value) -> Option<Vec<Vec<(f64, f64)>>> {
    let mut rings = Vec::new();
    for ring in polygon.as_array()? {
        if let Some(r) = ring_from_json(ring) {
            if r.len() >= 3 {
                rings.push(r);
            }
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

/// `#RRGGBB` -> `[r, g, b]`.
fn hex_rgb(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    Some([
        u8::from_str_radix(&s[0..2], 16).ok()?,
        u8::from_str_radix(&s[2..4], 16).ok()?,
        u8::from_str_radix(&s[4..6], 16).ok()?,
    ])
}

fn iso(s: &str) -> Option<DateTime<Utc>> {
    if s.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_categorical_features_in_severity_order() {
        let fc: serde_json::Value = serde_json::from_str(
            r##"{
              "type": "FeatureCollection",
              "features": [
                {
                  "type": "Feature",
                  "properties": {
                    "DN": 4, "LABEL": "SLGT", "LABEL2": "Slight Risk",
                    "stroke": "#FFE066", "fill": "#FFF7CC",
                    "ISSUE_ISO": "2026-10-13T05:22:00+00:00",
                    "VALID_ISO": "2026-10-13T12:00:00+00:00",
                    "EXPIRE_ISO": "2026-10-14T12:00:00+00:00",
                    "FORECASTER": "Dean/Chalmers"
                  },
                  "geometry": { "type": "Polygon", "coordinates": [[
                    [-98.0, 36.0], [-97.0, 36.0], [-97.0, 37.0], [-98.0, 37.0], [-98.0, 36.0]
                  ]]}
                },
                {
                  "type": "Feature",
                  "properties": { "DN": 2, "LABEL": "TSTM", "LABEL2": "General Thunderstorms Risk",
                    "stroke": "#55BB55", "fill": "#C1E9C1" },
                  "geometry": { "type": "MultiPolygon", "coordinates": [
                    [[[-100.0, 35.0], [-99.0, 35.0], [-99.0, 36.0], [-100.0, 35.0]]],
                    [[[-95.0, 34.0], [-94.0, 34.0], [-94.0, 35.0], [-95.0, 34.0]]]
                  ]}
                }
              ]
            }"##,
        )
        .unwrap();

        let o = parse_outlook(1, &fc).unwrap();
        assert_eq!(o.day, 1);
        assert_eq!(o.forecaster, "Dean/Chalmers");
        assert_eq!(
            o.issue,
            Some("2026-10-13T05:22:00Z".parse::<DateTime<Utc>>().unwrap())
        );
        assert_eq!(o.features.len(), 2);
        // Sorted least -> most severe.
        assert_eq!(o.features[0].label, "TSTM");
        assert_eq!(o.features[1].label, "SLGT");
        assert_eq!(o.features[1].stroke, [0xFF, 0xE0, 0x66]);
        assert_eq!(o.features[1].fill, [0xFF, 0xF7, 0xCC]);
        // SLGT is a single polygon with one ring of 5 vertices; TSTM is a
        // MultiPolygon with two polygons.
        assert_eq!(o.features[1].polygons.len(), 1);
        assert_eq!(o.features[1].polygons[0].len(), 1);
        assert_eq!(o.features[1].polygons[0][0].len(), 5);
        assert_eq!(o.features[0].polygons.len(), 2);
        // (lon, lat) order preserved.
        assert_eq!(o.features[1].polygons[0][0][0], (-98.0, 36.0));
    }

    #[test]
    fn hex_and_iso_edge_cases() {
        assert_eq!(hex_rgb("#00ff10"), Some([0, 255, 16]));
        assert_eq!(hex_rgb("00ff10"), None); // missing '#'
        assert_eq!(hex_rgb("#abc"), None); // too short
        assert_eq!(hex_rgb("#gggggg"), None); // non-hex
        assert_eq!(iso(""), None);
        assert_eq!(iso("not a date"), None);
    }

    #[test]
    fn rejects_bad_day() {
        let src = OutlookSource::new("stormscope-test (dev@example.com)").unwrap();
        assert!(src.fetch_day(0).is_err());
        assert!(src.fetch_day(4).is_err());
    }
}
