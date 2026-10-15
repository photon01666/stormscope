//! NWS active alerts via api.weather.gov.
//!
//! `AlertSource::active_near` fetches
//! `https://api.weather.gov/alerts/active?status=actual` (one request per
//! call — comfortably inside the ~1 req/s guidance) and filters client-side
//! by polygon-centroid distance. api.weather.gov rejects generic User-Agents,
//! so [`AlertSource::new`] requires a descriptive one.

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use radar_core::sites::haversine_km;
use radar_core::warnings::Warning;

const ALERTS_URL: &str = "https://api.weather.gov/alerts/active";

pub struct AlertSource {
    client: reqwest::blocking::Client,
}

impl AlertSource {
    /// Build a source with a descriptive User-Agent (required by
    /// api.weather.gov — include something identifying the app and a
    /// contact, e.g. `"stormscope/0.1 (contact@example.com)"`).
    pub fn new(user_agent: &str) -> Result<Self> {
        let ua = user_agent.trim();
        // "Descriptive" = non-trivial and identifies the app + a way to reach
        // the operator (parenthesized contact info is the NWS convention).
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

    /// Active alerts whose polygon centroid is within `radius_km` of
    /// (lat, lon). Zone-only alerts (no polygon geometry) are skipped here.
    pub fn active_near(&self, lat: f64, lon: f64, radius_km: f64) -> Result<Vec<Warning>> {
        Ok(alerts_near_in(&self.fetch_active()?, lat, lon, radius_km))
    }

    /// Active alerts that apply to an exact point — a few hundred *bytes* of
    /// JSON rather than the megabyte-scale nationwide listing, because the
    /// API filters server-side (`?point=`). This is the fast lane (see the
    /// coordinator's poll): alerts whose polygon or zone contains the station
    /// are exactly the ones worth flashing a badge about.
    pub fn active_at_point(&self, lat: f64, lon: f64) -> Result<Vec<Warning>> {
        let point = format!("{lat:.4},{lon:.4}");
        let resp = self
            .client
            .get(ALERTS_URL)
            .query(&[("status", "actual"), ("point", point.as_str())])
            .header("Accept", "application/geo+json")
            .send()?;
        let status = resp.status();
        if !status.is_success() {
            bail!("api.weather.gov alerts endpoint returned HTTP {status}");
        }
        let fc: serde_json::Value = resp.json()?;
        Ok(alerts_all_in(&fc))
    }

    /// Every active alert nationwide (several hundred on a busy day), newest
    /// sort order applied. Feeds the alerts browser; the map still draws only
    /// the near-station subset.
    pub fn active_all(&self) -> Result<Vec<Warning>> {
        Ok(alerts_all_in(&self.fetch_active()?))
    }

    /// GET the nationwide active-alerts FeatureCollection.
    fn fetch_active(&self) -> Result<serde_json::Value> {
        let resp = self
            .client
            .get(ALERTS_URL)
            .query(&[("status", "actual")])
            .header("Accept", "application/geo+json")
            .send()?;
        let status = resp.status();
        if !status.is_success() {
            bail!("api.weather.gov alerts endpoint returned HTTP {status}");
        }
        Ok(resp.json()?)
    }
}

/// Parse a `FeatureCollection` of NWS alerts into [`Warning`]s, most urgent
/// first. Separated from the network call so unit tests can run on a canned
/// fixture.
pub(crate) fn alerts_all_in(fc: &serde_json::Value) -> Vec<Warning> {
    let mut out = Vec::new();
    let Some(features) = fc.get("features").and_then(|f| f.as_array()) else {
        return out;
    };
    for feature in features {
        if let Some(w) = parse_feature(feature) {
            out.push(w);
        }
    }
    sort_urgent_first(&mut out);
    out
}

/// Keep the alerts whose polygon centroid lies within `radius_km` of
/// (lat, lon). Zone-only alerts (no polygon) cannot be placed, so they are
/// excluded.
pub(crate) fn alerts_near_in(
    fc: &serde_json::Value,
    lat: f64,
    lon: f64,
    radius_km: f64,
) -> Vec<Warning> {
    let mut out: Vec<Warning> = alerts_all_in(fc)
        .into_iter()
        .filter(|w| {
            w.centroid()
                .is_some_and(|(clat, clon)| haversine_km(lat, lon, clat, clon) <= radius_km)
        })
        .collect();
    sort_urgent_first(&mut out);
    out
}

/// Most urgent first for stable UI ordering.
fn sort_urgent_first(alerts: &mut [Warning]) {
    alerts.sort_by(|a, b| severity_rank(&b.severity).cmp(&severity_rank(&a.severity)));
}

fn severity_rank(s: &str) -> u8 {
    match s {
        "Extreme" => 4,
        "Severe" => 3,
        "Moderate" => 2,
        "Minor" => 1,
        _ => 0,
    }
}

fn parse_feature(feature: &serde_json::Value) -> Option<Warning> {
    let props = feature.get("properties")?;
    let s = |k: &str| -> String {
        props
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let expires = props
        .get("expires")
        .and_then(|v| v.as_str())
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .map(|d| d.with_timezone(&Utc))
        .or_else(|| {
            props
                .get("effective")
                .and_then(|v| v.as_str())
                .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
                .map(|d| d.with_timezone(&Utc))
        })
        .unwrap_or_else(Utc::now);
    let id = s("id");
    if id.is_empty() {
        return None;
    }
    Some(Warning {
        id,
        event: s("event"),
        severity: s("severity"),
        urgency: s("urgency"),
        expires,
        office: issuing_office(props),
        headline: s("headline"),
        description: s("description"),
        instruction: s("instruction"),
        area: s("areaDesc"),
        polygon: geometry_rings(feature.get("geometry")),
    })
}

/// Issuing WFO. api.weather.gov no longer populates `senderId`, so fall back
/// to the VTEC office id inside `parameters.VTEC` (e.g.
/// `/O.NEW.KABQ.SV.W.…` -> "ABQ") and then the AWIPS identifier PIL
/// (e.g. "SVRABQ" -> "ABQ"). Falls back to a parse of `senderName`
/// ("NWS Albuquerque NM" has no WFO, so usually "" there).
fn issuing_office(props: &serde_json::Value) -> String {
    if let Some(id) = props.get("senderId").and_then(|v| v.as_str()) {
        let id = id.trim();
        if !id.is_empty() {
            return id.to_string();
        }
    }
    let params = props.get("parameters");
    if let Some(vtec) = params
        .and_then(|p| p.get("VTEC"))
        .and_then(|v| v.as_array())
    {
        for entry in vtec {
            // "/O.NEW.KABQ.SV.W.0326.261012T0100Z-261012T0130Z/"
            let Some(line) = entry.as_str() else { continue };
            let Some(tuple) = line.split('/').nth(1) else {
                continue;
            };
            // Fields: mode/act, action (NEW/CON/EXP), office, phenomenon,
            // significance, product number, times.
            let office = tuple.split('.').nth(2);
            if let Some(office) = office {
                if office.len() == 4 && office.chars().all(|c| c.is_ascii_uppercase()) {
                    return office_from_vtec_tuple(office);
                }
            }
        }
    }
    // AWIPS identifier PIL: "SVRABQ" -> "ABQ".
    if let Some(pil) = params
        .and_then(|p| p.get("AWIPSidentifier"))
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
    {
        let pil = pil.trim();
        if pil.len() >= 4 {
            return pil[pil.len() - 3..].to_string();
        }
    }
    String::new()
}

/// Extract polygon rings from GeoJSON geometry as (lon, lat) pairs.
/// Polygons contribute each exterior ring; MultiPolygons contribute each
/// polygon's exterior ring. Other geometry types yield `None`.
fn geometry_rings(geometry: Option<&serde_json::Value>) -> Option<Vec<Vec<(f64, f64)>>> {
    let geometry = geometry?;
    let coords = geometry.get("coordinates")?;
    let mut rings = Vec::new();
    match geometry.get("type").and_then(|t| t.as_str())? {
        "Polygon" => {
            if let Some(ring) = ring_from_json(coords) {
                rings.push(ring);
            }
        }
        "MultiPolygon" => {
            for polygon in coords.as_array().into_iter().flatten() {
                if let Some(ring) = ring_from_json(polygon) {
                    rings.push(ring);
                }
            }
            if rings.is_empty() {
                return None;
            }
        }
        _ => return None,
    }
    if rings.is_empty() { None } else { Some(rings) }
}

/// First (exterior) ring of one GeoJSON polygon `coordinates` value.
fn ring_from_json(polygon_coords: &serde_json::Value) -> Option<Vec<(f64, f64)>> {
    let ring = polygon_coords.as_array()?.first()?.as_array()?;
    let mut out = Vec::with_capacity(ring.len());
    for pos in ring {
        let lon = pos.as_array()?.first()?.as_f64()?;
        let lat = pos.as_array()?.get(1)?.as_f64()?;
        out.push((lon, lat));
    }
    if out.len() < 3 {
        return None;
    }
    Some(out)
}

/// Average of ring vertices — good enough as a filter centroid for the
/// mostly-convex warning polygons the LSR/FFW pipeline emits. Ring pairs
/// are `(lon, lat)` per `Warning::polygon`.

// Keep the WFO strip logic honest: US WFOs are Kxxx CONUS / Txxx TDWR /
// PxNN Alaska-ish; the first letter is a region prefix that is dropped for
// the 4-char "Kxxx" case only.
fn office_from_vtec_tuple(office: &str) -> String {
    if office.len() == 4 && office.starts_with('K') {
        office[1..].to_string()
    } else {
        office.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_polygon_alert_and_filters_by_centroid() {
        // Ring centered ~(-97.3, 35.3): inside 200 km of OKC, far from Seattle.
        let fc: serde_json::Value = serde_json::from_str(
            r#"{
              "type": "FeatureCollection",
              "features": [
                {
                  "type": "Feature",
                  "properties": {
                    "id": "urn:oid:test.1",
                    "event": "Tornado Warning",
                    "severity": "Extreme",
                    "urgency": "Immediate",
                    "expires": "2026-10-11T19:30:00-06:00",
                    "headline": "Tornado Warning issued October 11 at 7:00PM CDT by NWS Norman OK",
                    "senderName": "NWS Norman OK",
                    "areaDesc": "Cleveland, OK; McClain, OK",
                    "description": "At 7:00 PM CDT, a confirmed tornado was located near Newcastle, moving northeast at 30 mph.",
                    "instruction": "TAKE COVER NOW! Move to a basement or an interior room on the lowest floor.",
                    "parameters": { "VTEC": ["/O.NEW.KOUN.T.W.0001.261012T0100Z-261012T0130Z/"],
                                    "AWIPSidentifier": ["TOROUN"] }
                  },
                  "geometry": { "type": "Polygon", "coordinates": [[
                    [-97.4, 35.2], [-97.2, 35.2], [-97.2, 35.4], [-97.4, 35.4], [-97.4, 35.2]
                  ]]}
                },
                {
                  "type": "Feature",
                  "properties": { "id": "urn:oid:test.2", "event": "Flood Advisory",
                    "severity": "Minor", "urgency": "Expected",
                    "expires": "2026-10-11T19:30:00-06:00" },
                  "geometry": { "type": "Polygon", "coordinates": [[
                    [-122.4, 47.5], [-122.2, 47.5], [-122.2, 47.7], [-122.4, 47.7], [-122.4, 47.5]
                  ]]}
                },
                {
                  "type": "Feature",
                  "properties": { "id": "urn:oid:test.3", "event": "Heat Advisory",
                    "severity": "Moderate", "urgency": "Expected",
                    "expires": "2026-10-11T19:30:00-06:00" },
                  "geometry": null
                }
              ]
            }"#,
        )
        .unwrap();

        let near = alerts_near_in(&fc, 35.3331, -97.2778, 200.0);
        assert_eq!(near.len(), 1, "only the OKC-area polygon should match");
        let w = &near[0];
        assert_eq!(w.id, "urn:oid:test.1");
        assert_eq!(w.event, "Tornado Warning");
        assert!(w.is_tornado());
        assert_eq!(w.severity, "Extreme");
        assert_eq!(w.urgency, "Immediate");
        assert_eq!(
            w.expires,
            "2026-10-12T01:30:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(w.office, "OUN");
        assert!(w.headline.contains("Tornado Warning"));
        let rings = w.polygon.as_ref().unwrap();
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].len(), 5);
        assert_eq!(rings[0][0], (-97.4, 35.2), "GeoJSON is (lon, lat)");

        // Zone-only alert (null geometry) never surfaces here.
        assert!(!near.iter().any(|w| w.event == "Heat Advisory"));

        // The nationwide parse keeps everything (including zone-only alerts)
        // and carries the affected-area text through.
        let all = alerts_all_in(&fc);
        assert_eq!(all.len(), 3);
        assert!(all.iter().any(|w| w.event == "Heat Advisory"));
        assert!(all.iter().any(|w| w.event == "Flood Advisory"));
        let t = all.iter().find(|w| w.id == "urn:oid:test.1").unwrap();
        assert_eq!(t.area, "Cleveland, OK; McClain, OK");
        assert!(
            t.description.contains("confirmed tornado"),
            "NOAA text parsed"
        );
        assert!(
            t.instruction.starts_with("TAKE COVER"),
            "NOAA instruction parsed"
        );
        assert_eq!(
            t.centroid().map(|(lat, _)| (lat * 10.0).round()),
            Some(353.0)
        );
        // Most urgent first, so the browser opens on the tornado.
        assert_eq!(all[0].event, "Tornado Warning");
    }

    /// Geometry-less alerts have no placeable centroid (the station tab and
    /// the map-focus action both rely on this being None, not a fake point).
    /// The point query returns the same FeatureCollection shape, so the same
    /// parser handles it; a zone-only alert (no geometry) is kept, because
    /// "this alert applies here" is the API's answer, not something we infer
    /// from a centroid.
    #[test]
    fn parses_point_alerts_including_zone_only_ones() {
        let fc: serde_json::Value = serde_json::from_str(
            r##"{"type":"FeatureCollection","features":[
                {"type":"Feature","properties":{"id":"urn:oid:p1","event":"Tornado Warning",
                  "severity":"Extreme","urgency":"Immediate","areaDesc":"Pottawatomie, KS",
                  "expires":"2026-10-11T19:30:00-06:00"},
                 "geometry":{"type":"Polygon","coordinates":[[
                   [-96.6,39.1],[-96.1,39.1],[-96.1,39.4],[-96.6,39.4],[-96.6,39.1]]]}},
                {"type":"Feature","properties":{"id":"urn:oid:p2","event":"Flood Watch",
                  "severity":"Moderate","areaDesc":"Shawnee, KS",
                  "expires":"2026-10-12T07:00:00-06:00"},
                 "geometry":null}]}"##,
        )
        .unwrap();
        let alerts = alerts_all_in(&fc);
        assert_eq!(alerts.len(), 2);
        assert!(alerts[0].is_tornado());
        assert!(alerts[0].centroid().is_some());
        assert!(alerts[1].centroid().is_none(), "zone alert has no polygon");
        assert_eq!(alerts[1].area, "Shawnee, KS");
    }

    #[test]
    fn zone_alerts_have_no_centroid() {
        let fc: serde_json::Value = serde_json::from_str(
            r#"{"type":"FeatureCollection","features":[{
                 "type":"Feature",
                 "properties":{"id":"z1","event":"Heat Advisory","severity":"Moderate",
                               "areaDesc":"Somewhere, OK"},
                 "geometry":null}]}"#,
        )
        .unwrap();
        let all = alerts_all_in(&fc);
        assert_eq!(all.len(), 1);
        assert!(all[0].centroid().is_none());
        assert_eq!(all[0].area, "Somewhere, OK");
        assert!(all[0].description.is_empty() && all[0].instruction.is_empty());
    }

    #[test]
    fn rejects_generic_user_agent() {
        assert!(AlertSource::new("").is_err());
        assert!(AlertSource::new("curl").is_err());
        assert!(AlertSource::new("stormscope-test (dev@stormscope.local)").is_ok());
    }

    #[test]
    fn vtec_office_edge_cases() {
        // 4-char Alaska office without K prefix stays intact.
        assert_eq!(office_from_vtec_tuple("PAFC"), "PAFC");
        assert_eq!(office_from_vtec_tuple("KOUN"), "OUN");
    }
}
