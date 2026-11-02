//! Bundled geographic context for the map: US county outlines.
//!
//! The data lives in a compact binary under `assets/`, built by
//! `scripts/build_geo_assets.py` from public-domain Census county geometry.
//! It is parsed once, lazily, on first use; county rings are culled by a
//! precomputed bounding box per frame.

use std::sync::LazyLock;

/// One closed county ring in `(lon, lat)` degrees with a precomputed bound.
pub struct CountyRing {
    pub points: Vec<[f32; 2]>,
    pub min_lon: f32,
    pub max_lon: f32,
    pub min_lat: f32,
    pub max_lat: f32,
}

pub static COUNTIES: LazyLock<Vec<CountyRing>> =
    LazyLock::new(|| parse_counties(include_bytes!("../assets/counties.bin")));

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

fn parse_counties(bytes: &'static [u8]) -> Vec<CountyRing> {
    let mut rd = Reader::new(bytes);
    if rd.take(4) != Some(b"SSCY") || rd.u32() != Some(1) {
        log::warn!("counties.bin: bad header");
        return Vec::new();
    }
    let count = rd.u32().unwrap_or(0) as usize;
    let mut rings = Vec::with_capacity(count);
    for _ in 0..count {
        let Some(point_count) = rd.u32().map(|n| n as usize) else {
            break;
        };
        // A corrupt length must not make a multi-gigabyte allocation.
        if point_count == 0 || point_count > 1_000_000 {
            break;
        }
        let mut points = Vec::with_capacity(point_count);
        let mut min_lon = f32::INFINITY;
        let mut max_lon = f32::NEG_INFINITY;
        let mut min_lat = f32::INFINITY;
        let mut max_lat = f32::NEG_INFINITY;
        let mut ok = true;
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
    rings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_counties_parse_and_are_plausible() {
        let rings = &*COUNTIES;
        assert!(
            rings.len() > 3000,
            "expected every US county ring: {}",
            rings.len()
        );
        assert!(rings.iter().all(|r| r.points.len() >= 3));
        let (min_lon, max_lon) = rings.iter().fold((f32::MAX, f32::MIN), |(lo, hi), r| {
            (lo.min(r.min_lon), hi.max(r.max_lon))
        });
        assert!(min_lon < -150.0 && max_lon > -70.0, "CONUS+AK/HI span");
        let (min_lat, max_lat) = rings.iter().fold((f32::MAX, f32::MIN), |(lo, hi), r| {
            (lo.min(r.min_lat), hi.max(r.max_lat))
        });
        assert!(min_lat < 20.0 && max_lat > 45.0, "CONUS+AK/HI span");
    }
}
