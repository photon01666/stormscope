//! GOES satellite overlay through NASA GIBS.
//!
//! One WMS `GetMap` request per refresh returns a PNG of the caller's box
//! (the radar's local window) **in Web Mercator** — the exact projection the
//! map draws in — so the overlay is a plain georeferenced rect with no
//! reprojection step. GOES-East covers the eastern US and GOES-West the west;
//! the caller picks by site longitude.
//!
//! Layers: the clean 10.3 µm infrared band (cloud tops, day and night) and
//! GeoColor (the natural-colour composite). GIBS serves the latest frame when
//! no `TIME` is given; the overlay labels the fetch time.

use anyhow::{Context, Result};

/// Requested image width (height follows the box's Mercator aspect).
pub const WIDTH: u32 = 2048;

/// Which satellite product the overlay shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GoesLayer {
    /// Band 13 (10.3 µm) clean infrared: cloud tops, always available.
    #[default]
    Infrared,
    /// Natural-colour composite (night side is IR-derived).
    GeoColor,
}

impl GoesLayer {
    pub fn label(self) -> &'static str {
        match self {
            GoesLayer::Infrared => "Infrared",
            GoesLayer::GeoColor => "GeoColor",
        }
    }

    fn layer_id(self, west: bool) -> String {
        let satellite = if west { "GOES-West" } else { "GOES-East" };
        let product = match self {
            GoesLayer::Infrared => "ABI_Band13_Clean_Infrared",
            GoesLayer::GeoColor => "ABI_GeoColor",
        };
        format!("{satellite}_{product}")
    }
}

/// Mercator x/y in metres (EPSG:3857), matching the map's projection.
pub fn mercator_x(lon: f64) -> f64 {
    6_378_137.0 * lon.to_radians()
}

pub fn mercator_y(lat: f64) -> f64 {
    6_378_137.0
        * (std::f64::consts::PI / 4.0 + lat.to_radians() / 2.0)
            .tan()
            .ln()
}

/// One fetched satellite frame: raw RGBA plus the box it covers.
#[derive(Debug)]
pub struct GoesImage {
    pub layer: GoesLayer,
    pub west_satellite: bool,
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    /// When the frame was fetched (GIBS returns the latest available).
    pub time: chrono::DateTime<chrono::Utc>,
    /// Geographic box the image covers.
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
}

/// The WMS request for one image over `bbox` (`[south, west, north, east]`).
pub fn wms_url(layer: GoesLayer, west_satellite: bool, width: u32, bbox: [f64; 4]) -> String {
    let [south, west, north, east] = bbox;
    let x0 = mercator_x(west);
    let x1 = mercator_x(east);
    let y0 = mercator_y(south);
    let y1 = mercator_y(north);
    let height = ((f64::from(width) * (y1 - y0) / (x1 - x0)).round() as u32).max(2);
    format!(
        "https://gibs.earthdata.nasa.gov/wms/epsg3857/best/wms.cgi?SERVICE=WMS&VERSION=1.3.0\
         &REQUEST=GetMap&LAYERS={}&STYLES=&CRS=EPSG:3857&BBOX={x0:.0},{y0:.0},{x1:.0},{y1:.0}\
         &WIDTH={width}&HEIGHT={height}&FORMAT=image/png",
        layer.layer_id(west_satellite),
    )
}

/// Fetch driver (its own HTTP client, like the other overlay sources).
pub struct GoesSource {
    client: reqwest::blocking::Client,
}

impl GoesSource {
    pub fn new() -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!(
                "stormscope/",
                env!("CARGO_PKG_VERSION"),
                " (https://github.com/photon01666/stormscope)"
            ))
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .context("building the GOES HTTP client")?;
        Ok(Self { client })
    }

    /// The latest frame of `layer` from the chosen satellite.
    pub fn latest(
        &self,
        layer: GoesLayer,
        west_satellite: bool,
        bbox: [f64; 4],
    ) -> Result<GoesImage> {
        let url = wms_url(layer, west_satellite, WIDTH, bbox);
        let png = self
            .client
            .get(&url)
            .send()
            .and_then(|r| r.error_for_status())
            .with_context(|| format!("downloading {url}"))?
            .bytes()
            .context("reading the GIBS body")?;
        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .context("decoding the GIBS PNG")?
            .to_rgba8();
        Ok(GoesImage {
            layer,
            west_satellite,
            width: decoded.width() as usize,
            height: decoded.height() as usize,
            rgba: decoded.into_raw(),
            time: chrono::Utc::now(),
            south: bbox[0],
            west: bbox[1],
            north: bbox[2],
            east: bbox[3],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The request is in the map's projection, over the documented box, with
    /// the aspect ratio of that box (so the PNG is not squashed).
    #[test]
    fn wms_request_is_mercator_and_aspect_correct() {
        let bbox = crate::local_overlay_box(39.0, -96.2);
        let url = wms_url(GoesLayer::Infrared, false, 1024, bbox);
        assert!(
            url.contains("LAYERS=GOES-East_ABI_Band13_Clean_Infrared"),
            "{url}"
        );
        assert!(url.contains("CRS=EPSG:3857"), "{url}");
        assert!(url.contains("FORMAT=image/png"), "{url}");
        // The BBOX is the box's Mercator extent, not lon/lat.
        assert!(
            url.contains("BBOX=-11599491,4028802,-9818379,5465442"),
            "{url}"
        );
        // Mercator y matches the usual reference values.
        assert!((mercator_y(0.0)).abs() < 1e-6);
        assert!((mercator_y(20.0) - 2_273_031.0).abs() < 2.0);
        assert!((mercator_y(55.0) - 7_361_866.0).abs() < 2.0);
        // 1024 px over 7794 km → ~6826 km tall? No: the box aspect decides.
        let height: u32 = url
            .split("HEIGHT=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!((height as i64 - 826).abs() <= 2, "height {height}");
        // GeoColor and the west satellite are just different layer names.
        let url = wms_url(GoesLayer::GeoColor, true, 512, bbox);
        assert!(url.contains("GOES-West_ABI_GeoColor"), "{url}");
    }

    /// Real fetch (network, ignored by default): both layers decode to the
    /// documented box.
    #[test]
    #[ignore = "needs network access to gibs.earthdata.nasa.gov"]
    fn fetches_a_real_goes_frame() {
        let src = GoesSource::new().expect("client");
        let bbox = crate::local_overlay_box(39.0, -96.2);
        for layer in [GoesLayer::Infrared, GoesLayer::GeoColor] {
            let img = src.latest(layer, false, bbox).expect("a frame");
            eprintln!(
                "{}: {}x{} at {}",
                layer.label(),
                img.width,
                img.height,
                img.time
            );
            assert_eq!(img.width, WIDTH as usize);
            assert!(img.height > 1400 && img.height < 2000, "{}", img.height);
            assert_eq!(img.rgba.len(), img.width * img.height * 4);
            // Some pixels are transparent (off-disk), some are opaque cloud.
            let opaque = img.rgba.chunks_exact(4).filter(|px| px[3] > 200).count();
            assert!(opaque > img.width * img.height / 10, "opaque {opaque}");
        }
    }
}
