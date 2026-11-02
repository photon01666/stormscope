//! Bundled geographic context for the map: US county outlines keyed by NWS
//! county-zone code.
//!
//! The data lives in a compact binary under `assets/`, built by
//! `scripts/build_geo_assets.py` from public-domain Census county geometry.
//! It is parsed once, lazily, on first use; county rings are culled by a
//! precomputed bounding box per frame. Zone-based watches are filled from
//! this same geometry, so their county grid renders without one API request
//! per county.

use std::collections::HashMap;
use std::sync::LazyLock;

/// One closed county ring in `(lon, lat)` degrees with a precomputed bound.
pub struct CountyRing {
    pub points: Vec<[f32; 2]>,
    pub min_lon: f32,
    pub max_lon: f32,
    pub min_lat: f32,
    pub max_lat: f32,
}

/// One county and its NWS zone code (e.g. `OKC003`).
pub struct County {
    pub zone: [u8; 6],
    pub rings: Vec<CountyRing>,
}

pub static COUNTIES: LazyLock<Vec<County>> =
    LazyLock::new(|| parse_counties(include_bytes!("../assets/counties.bin")));

static ZONE_INDEX: LazyLock<HashMap<[u8; 6], usize>> = LazyLock::new(|| {
    COUNTIES
        .iter()
        .enumerate()
        .map(|(index, county)| (county.zone, index))
        .collect()
});

/// County rings for an NWS zone URL such as
/// `https://api.weather.gov/zones/county/OKC003`. Returns `None` for
/// non-county zones or a code the bundled set does not cover.
pub fn rings_for_zone_url(url: &str) -> Option<Vec<Vec<(f64, f64)>>> {
    let code = url.rsplit('/').next()?;
    if code.len() != 6 {
        return None;
    }
    let mut key = [0u8; 6];
    key.copy_from_slice(code.as_bytes());
    let county = COUNTIES.get(*ZONE_INDEX.get(&key)?)?;
    Some(
        county
            .rings
            .iter()
            .map(|ring| {
                ring.points
                    .iter()
                    .map(|&[lon, lat]| (lon as f64, lat as f64))
                    .collect()
            })
            .collect(),
    )
}

/// Fill zone-only alerts with bundled county geometry.
///
/// `watches_only` keeps the nationwide set cheap: only watches are merged
/// into the map from `all_alerts`, so the browser's hundreds of advisories
/// do not need geometry until they arrive through the station-radius list.
pub fn enrich_zone_warnings(warnings: &mut [radar_core::warnings::Warning], watches_only: bool) {
    for warning in warnings {
        if !warning.zone_only
            || warning.polygon.is_some()
            || warning.zone_urls.is_empty()
            || (watches_only && !warning.event.contains("Watch"))
        {
            continue;
        }
        let mut rings = Vec::new();
        for url in &warning.zone_urls {
            if let Some(mut found) = rings_for_zone_url(url) {
                rings.append(&mut found);
            }
        }
        if !rings.is_empty() {
            warning.polygon = Some(rings);
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let out = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(out)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
}

fn parse_counties(bytes: &'static [u8]) -> Vec<County> {
    let mut rd = Reader::new(bytes);
    if rd.take(4) != Some(b"SSCY") || rd.u32() != Some(3) {
        log::warn!("counties.bin: bad header");
        return Vec::new();
    }
    let count = rd.u32().unwrap_or(0) as usize;
    let mut counties = Vec::with_capacity(count);
    for _ in 0..count {
        let Some(zone) = rd.take(6) else {
            break;
        };
        let mut key = [0u8; 6];
        key.copy_from_slice(zone);
        let Some(ring_count) = rd.u32().map(|n| n as usize) else {
            break;
        };
        let mut rings = Vec::with_capacity(ring_count);
        let mut ok = true;
        for _ in 0..ring_count {
            let Some(point_count) = rd.u32().map(|n| n as usize) else {
                ok = false;
                break;
            };
            // A corrupt length must not make a multi-gigabyte allocation.
            if point_count == 0 || point_count > 1_000_000 {
                ok = false;
                break;
            }
            let mut points = Vec::with_capacity(point_count);
            let mut min_lon = f32::INFINITY;
            let mut max_lon = f32::NEG_INFINITY;
            let mut min_lat = f32::INFINITY;
            let mut max_lat = f32::NEG_INFINITY;
            for _ in 0..point_count {
                let (Some(lon), Some(lat)) = (rd.f32(), rd.f32()) else {
                    ok = false;
                    break;
                };
                min_lon = min_lon.min(lon);
                max_lon = max_lon.max(lon);
                min_lat = min_lat.min(lat);
                max_lat = max_lat.max(lat);
                points.push([lon, lat]);
            }
            if !ok {
                break;
            }
            rings.push(CountyRing {
                points,
                min_lon,
                max_lon,
                min_lat,
                max_lat,
            });
        }
        if !ok {
            break;
        }
        counties.push(County { zone: key, rings });
    }
    counties
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_counties_parse_and_are_plausible() {
        let counties = &*COUNTIES;
        assert!(
            counties.len() > 3000,
            "expected every US county: {}",
            counties.len()
        );
        assert!(counties.iter().all(|c| !c.rings.is_empty()));
        let (min_lon, max_lon) = counties
            .iter()
            .flat_map(|c| &c.rings)
            .fold((f32::MAX, f32::MIN), |(lo, hi), r| {
                (lo.min(r.min_lon), hi.max(r.max_lon))
            });
        assert!(min_lon < -150.0 && max_lon > -70.0, "CONUS+AK/HI span");
        let (min_lat, max_lat) = counties
            .iter()
            .flat_map(|c| &c.rings)
            .fold((f32::MAX, f32::MIN), |(lo, hi), r| {
                (lo.min(r.min_lat), hi.max(r.max_lat))
            });
        assert!(min_lat < 20.0 && max_lat > 45.0, "CONUS+AK/HI span");
    }

    #[test]
    fn nws_county_zone_urls_resolve_to_rings() {
        let rings = rings_for_zone_url("https://api.weather.gov/zones/county/OKC003")
            .expect("Alfalfa County, OK is in the bundled set");
        assert!(!rings.is_empty());
        assert!(rings[0].len() >= 3);
        assert!(rings_for_zone_url("https://api.weather.gov/zones/forecast/OKZ001").is_none());
    }

    #[test]
    fn zone_enrichment_keeps_one_ring_group_per_county() {
        let mut warning = radar_core::warnings::Warning {
            id: "test".into(),
            event: "Tornado Watch".into(),
            severity: "Moderate".into(),
            urgency: "Expected".into(),
            expires: chrono::Utc::now(),
            office: "OUN".into(),
            headline: String::new(),
            description: String::new(),
            instruction: String::new(),
            area: String::new(),
            polygon: None,
            zone_urls: vec![
                "https://api.weather.gov/zones/county/OKC003".into(),
                "https://api.weather.gov/zones/county/OKC009".into(),
            ],
            zone_only: true,
        };
        enrich_zone_warnings(std::slice::from_mut(&mut warning), true);
        assert_eq!(
            warning.polygon.as_ref().map(Vec::len),
            Some(2),
            "two counties stay two rings"
        );
    }
}
