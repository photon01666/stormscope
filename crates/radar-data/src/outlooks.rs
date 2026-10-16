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

    /// Fetch a day's outlook as a given product type. The categorical areas
    /// exist for Days 1–3; Tornado/Wind/Hail probability GeoJSONs exist for
    /// Days 1–2 only (verified against the live server), and asking for day 3
    /// probabilities fails fast instead of hitting a 404.
    pub fn fetch(&self, day: u8, ty: radar_core::outlooks::OutlookType) -> Result<Outlook> {
        if !(1..=3).contains(&day) {
            bail!("outlook day must be 1..=3 (got {day})");
        }
        if ty.is_probability() && !radar_core::outlooks::probabilities_available(day) {
            bail!("SPC publishes no {} outlooks for day {day}", ty.label());
        }
        let url = format!("{BASE}/day{day}otlk{}.nolyr.geojson", ty.file_key());
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
        parse_outlook(day, ty, &fc)
    }

    /// The categorical outlook for `day` (1..=3) — the layer's default.
    pub fn fetch_day(&self, day: u8) -> Result<Outlook> {
        self.fetch(day, radar_core::outlooks::OutlookType::Categorical)
    }

    /// The forecaster's full discussion for `day` (1..=3): the plain-text
    /// outlook product SPC publishes alongside the GeoJSON, with its WMO
    /// header trimmed so it starts at the outlook title.
    pub fn discussion(&self, day: u8) -> Result<String> {
        if !(1..=3).contains(&day) {
            bail!("outlook day must be 1..=3 (got {day})");
        }
        let url = format!("{BASE}/day{day}otlk.txt");
        let resp = self
            .client
            .get(&url)
            .send()
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            bail!("SPC outlook discussion day {day} returned HTTP {status}");
        }
        Ok(strip_wmo_header(&resp.text()?))
    }
}

/// Drop the WMO routing block (`ZCZC …` / `ACUS01 KWNS …` / `SPC AC …`) that
/// precedes the outlook title, so the popup opens on the human-readable text.
pub(crate) fn strip_wmo_header(text: &str) -> String {
    let mut out = String::new();
    let mut seen_title = false;
    for line in text.lines() {
        if !seen_title {
            let trimmed = line.trim_start();
            if trimmed.starts_with("Day ") && trimmed.contains("Convective Outlook") {
                seen_title = true;
            } else {
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    if seen_title { out } else { text.to_string() }
}

/// Parse an SPC categorical FeatureCollection into an [`Outlook`]. Separated
/// from the network call so tests can run on a canned fixture.
pub fn parse_outlook(
    day: u8,
    ty: radar_core::outlooks::OutlookType,
    fc: &serde_json::Value,
) -> Result<Outlook> {
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
        ty,
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

    /// The discussion popup should open on the outlook title, not the WMO
    /// routing block; unrecognized input passes through untouched.
    #[test]
    fn discussion_strips_the_wmo_header() {
        let raw = "ZCZC SPCSWODY1 ALL\nACUS01 KWNS 151957\nSPC AC 151957\n\n\
                   Day 1 Convective Outlook  \nNWS Storm Prediction Center Norman OK\n\
                   Valid 152000Z - 161200Z\n\n...SUMMARY...\nIsolated severe storms possible.\n";
        let body = strip_wmo_header(raw);
        assert!(body.starts_with("Day 1 Convective Outlook"), "got {body:?}");
        assert!(body.contains("...SUMMARY..."));
        assert!(!body.contains("ZCZC"));
        // No recognizable title: return the input unchanged.
        let odd = "ZCZC x\nsomething else\n";
        assert_eq!(strip_wmo_header(odd), odd);
    }

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

        let o = parse_outlook(1, radar_core::outlooks::OutlookType::Categorical, &fc).unwrap();
        assert_eq!(o.ty, radar_core::outlooks::OutlookType::Categorical);
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

    const TORN: &str = include_str!("../tests/fixtures/outlook_day1_torn.geojson");
    const HAIL_NO_AREAS: &str = include_str!("../tests/fixtures/outlook_day1_hail.geojson");

    /// The probability GeoJSON is the categorical shape with a fraction
    /// `LABEL` and SPC's own human `LABEL2` — colors included.
    #[test]
    fn parses_probability_outlook() {
        let fc: serde_json::Value = serde_json::from_str(TORN).unwrap();
        let o = parse_outlook(1, radar_core::outlooks::OutlookType::Tornado, &fc).unwrap();
        assert_eq!(o.ty, radar_core::outlooks::OutlookType::Tornado);
        assert_eq!(o.features.len(), 1);
        let f = &o.features[0];
        assert_eq!(f.label, "0.02");
        assert_eq!(f.probability_text(), "2%");
        assert_eq!(f.name, "2% Tornado Risk");
        assert_eq!(f.stroke, [0x1A, 0x73, 0x1D], "SPC's own stroke parsed");
        assert_eq!(f.fill, [0x79, 0xBA, 0x7A]);
        assert!(!f.polygons.is_empty(), "geometry carried");
        assert_eq!(o.forecaster, "Dean");
    }

    /// A "no areas" probability product (hail here) draws nothing: its
    /// GeometryCollection has no Polygon/MultiPolygon, so the feature is
    /// skipped — the UI shows "no areas" instead of an empty map.
    #[test]
    fn probability_outlook_without_areas_yields_no_features() {
        let fc: serde_json::Value = serde_json::from_str(HAIL_NO_AREAS).unwrap();
        let o = parse_outlook(1, radar_core::outlooks::OutlookType::Hail, &fc).unwrap();
        assert_eq!(o.ty, radar_core::outlooks::OutlookType::Hail);
        assert!(o.features.is_empty(), "no drawable areas");
        assert_eq!(o.forecaster, "Dean", "metadata still parsed");
    }

    #[test]
    fn rejects_bad_day() {
        let src = OutlookSource::new("stormscope-test (dev@example.com)").unwrap();
        assert!(src.fetch_day(0).is_err());
        assert!(src.fetch_day(4).is_err());
        // Day 3 has no probability products: fail before hitting the server.
        assert!(
            src.fetch(3, radar_core::outlooks::OutlookType::Tornado)
                .is_err()
        );
    }
}
