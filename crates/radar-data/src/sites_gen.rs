//! Regenerates the full radar site catalog in radar-core from the NWS
//! API (`https://api.weather.gov/radar/stations`). Run:
//!
//! ```sh
//! cargo run -p radar-data --bin gen-sites > crates/radar-core/src/sites.rs
//! ```
//!
//! Only `WSR-88D` stations are emitted: they are the sites that carry Level
//! II data on the NOAA/Unidata S3 buckets this app fetches from. (TDWR and
//! Profiler stations from the same API have no L2 feed.) (W2)

use anyhow::{Context, Result};

/// Authoritative NWS radar-station catalog (GeoJSON FeatureCollection).
pub const RADARS_URL: &str = "https://api.weather.gov/radar/stations";

/// api.weather.gov rejects generic user agents; keep this descriptive.
pub const DEFAULT_USER_AGENT: &str = "stormscope/0.1 (radar catalog generator; dev@stormscope.local)";

/// One WSR-88D site in the generated catalog.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogSite {
    pub id: String,
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub elevation_m: f64,
}

/// Fetch the full WSR-88D catalog from api.weather.gov, sorted by id.
pub fn fetch_catalog(user_agent: &str) -> Result<Vec<CatalogSite>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(user_agent)
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp = client
        .get(RADARS_URL)
        .header("Accept", "application/geo+json")
        .send()
        .with_context(|| format!("fetching {RADARS_URL}"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("api.weather.gov radar stations returned HTTP {status}");
    }
    let fc: serde_json::Value = resp.json()?;
    Ok(parse_catalog(&fc))
}

/// Parse the `radar/stations` FeatureCollection, keeping WSR-88D sites with
/// usable geometry. Separated from the fetch so tests can use a fixture.
pub fn parse_catalog(fc: &serde_json::Value) -> Vec<CatalogSite> {
    let Some(features) = fc.get("features").and_then(|f| f.as_array()) else {
        return Vec::new();
    };
    let mut sites: Vec<CatalogSite> = features
        .iter()
        .filter_map(|f| {
            let props = f.get("properties")?;
            if props.get("stationType").and_then(|t| t.as_str())? != "WSR-88D" {
                return None;
            }
            let id = props.get("id").and_then(|v| v.as_str())?.to_string();
            if id.len() != 4 {
                return None;
            }
            let coords = f.get("geometry")?.get("coordinates")?.as_array()?;
            let longitude = coords.first()?.as_f64()?;
            let latitude = coords.get(1)?.as_f64()?;
            let elevation_m = props
                .get("elevation")
                .and_then(|e| e.get("value"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            Some(CatalogSite {
                id,
                name: props
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                latitude,
                longitude,
                elevation_m,
            })
        })
        .collect();
    sites.sort_by(|a, b| a.id.cmp(&b.id));
    sites.dedup_by(|a, b| a.id == b.id);
    sites
}

/// Render a complete `radar-core/src/sites.rs` (same macro format and
/// `site()`/`nearest()`/`haversine_km()` helpers) for the given catalog.
pub fn render_sites_rs(sites: &[CatalogSite]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    out.push_str(
        "//! WSR-88D radar site catalog (generated — do not edit by hand).\n\
         //!\n\
         //! Full CONUS/Alaska/Hawaii/Puerto Rico WSR-88D catalog from the NWS\n\
         //! station API (`https://api.weather.gov/radar/stations`, WSR-88D\n\
         //! stations only). Regenerate with:\n\
         //!\n\
         //! ```sh\n\
         //! cargo run -p radar-data --bin gen-sites > crates/radar-core/src/sites.rs\n\
         //! ```\n\n",
    );
    out.push_str(
        "pub struct RadarSite {\n\
         \x20   /// Four-character ICAO-style ID used in S3 keys, e.g. \"KTLX\".\n\
         \x20   pub id: &'static str,\n\
         \x20   /// Human name, e.g. \"Norman, OK\".\n\
         \x20   pub name: &'static str,\n\
         \x20   pub latitude: f64,\n\
         \x20   pub longitude: f64,\n\
         \x20   /// Site elevation in meters MSL.\n\
         \x20   pub elevation_m: f64,\n\
         }\n\n",
    );
    out.push_str(
        "macro_rules! site {\n\
         \x20   ($id:literal, $name:literal, $lat:expr, $lon:expr, $elev:expr) => {\n\
         \x20       RadarSite { id: $id, name: $name, latitude: $lat, longitude: $lon, elevation_m: $elev }\n\
         \x20   };\n\
         }\n\n",
    );
    writeln!(out, "pub static SITES: &[RadarSite] = &[").unwrap();
    for s in sites {
        writeln!(
            out,
            "    site!({}, {}, {:.5}, {:.5}, {:.1}),",
            escape_literal(&s.id),
            escape_literal(&s.name),
            s.latitude,
            s.longitude,
            s.elevation_m
        )
        .unwrap();
    }
    out.push_str("];\n\n");
    out.push_str(
        "/// Look up a site by ID (case-sensitive, e.g. \"KTLX\").\n\
         pub fn site(id: &str) -> Option<&'static RadarSite> {\n\
         \x20   SITES.iter().find(|s| s.id == id)\n\
         }\n\n\
         /// Nearest site to a lat/lon, useful for \"locate me\" style startup.\n\
         pub fn nearest(lat: f64, lon: f64) -> Option<&'static RadarSite> {\n\
         \x20   SITES\n\
         \x20       .iter()\n\
         \x20       .min_by(|a, b| {\n\
         \x20           haversine_km(lat, lon, a.latitude, a.longitude)\n\
         \x20               .partial_cmp(&haversine_km(lat, lon, b.latitude, b.longitude))\n\
         \x20               .unwrap_or(std::cmp::Ordering::Equal)\n\
         \x20       })\n\
         }\n\n\
         /// Great-circle distance in kilometers.\n\
         pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {\n\
         \x20   const R: f64 = 6371.0;\n\
         \x20   let dlat = (lat2 - lat1).to_radians();\n\
         \x20   let dlon = (lon2 - lon1).to_radians();\n\
         \x20   let a = (dlat / 2.0).sin().powi(2)\n\
         \x20       + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);\n\
         \x20   2.0 * R * a.sqrt().atan2(1.0 - a.sqrt())\n\
         }\n",
    );
    out
}

/// Rust string-literal escaping, quotes included (`{:?}` semantics) so the
/// result is a valid `literal` for the `site!` macro.
fn escape_literal(s: &str) -> String {
    format!("{s:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wsr88d_only_and_renders_valid_shape() {
        let fc: serde_json::Value = serde_json::from_str(
            r#"{
              "features": [
                { "type": "Feature",
                  "geometry": { "type": "Point", "coordinates": [-97.27775, 35.33305] },
                  "properties": { "id": "KTLX", "name": "Norman", "stationType": "WSR-88D",
                                  "elevation": { "unitCode": "wmoUnit:m", "value": 369.72 } } },
                { "type": "Feature",
                  "geometry": { "type": "Point", "coordinates": [-97.3, 35.4] },
                  "properties": { "id": "TLX", "name": "Twin Lakes", "stationType": "TDWR",
                                  "elevation": { "unitCode": "wmoUnit:m", "value": 300.0 } } },
                { "type": "Feature", "geometry": null,
                  "properties": { "id": "KXXX", "name": "Broken", "stationType": "WSR-88D" } }
              ]
            }"#,
        )
        .unwrap();
        let sites = parse_catalog(&fc);
        assert_eq!(sites.len(), 1, "only WSR-88D with geometry survives");
        assert_eq!(sites[0].id, "KTLX");
        assert_eq!(sites[0].name, "Norman");
        assert_eq!(sites[0].latitude, 35.33305);

        let rendered = render_sites_rs(&sites);
        assert!(rendered.contains("site!(\"KTLX\", \"Norman\", 35.33305, -97.27775, 369.7),"));
        assert!(!rendered.contains("TLX\", \"Twin Lakes"));
        assert!(rendered.contains("macro_rules! site"));
        assert!(rendered.contains("pub fn site("));
        assert!(rendered.contains("pub fn nearest("));
        assert!(rendered.contains("pub fn haversine_km("));
    }

    #[test]
    fn escapes_quotes_in_names() {
        assert_eq!(escape_literal("Plain"), "\"Plain\"");
        assert_eq!(escape_literal("Say \"hi\""), "\"Say \\\"hi\\\"\"");
    }
}
