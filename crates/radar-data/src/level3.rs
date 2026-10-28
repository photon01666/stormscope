//! Official NEXRAD Level III N0Q base-reflectivity composite from the Iowa
//! Environmental Mesonet's WMS-T service.
//!
//! This is deliberately an image overlay, not a local algorithm: IEM builds
//! the N0Q composite from the official Level III feed, and their WMS returns
//! a georeferenced Web-Mercator PNG. That makes it a useful cross-check for
//! Stormscope's own derived/Level-II products.

use anyhow::{Context, Result};

/// Requested image width (height follows the Mercator box aspect).
pub const WIDTH: u32 = 2048;

/// One N0Q frame, already coloured by IEM.
#[derive(Debug)]
pub struct Level3Image {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    pub time: chrono::DateTime<chrono::Utc>,
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
}

pub struct Level3Source {
    client: reqwest::blocking::Client,
}

impl Level3Source {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::blocking::Client::builder()
                .user_agent(concat!(
                    "stormscope/",
                    env!("CARGO_PKG_VERSION"),
                    " (https://github.com/photon01666/stormscope)"
                ))
                .timeout(std::time::Duration::from_secs(45))
                .build()
                .context("building the Level III HTTP client")?,
        })
    }

    pub fn latest_n0q(&self, bbox: [f64; 4]) -> Result<Level3Image> {
        let [south, west, north, east] = bbox;
        let x0 = mercator_x(west);
        let x1 = mercator_x(east);
        let y0 = mercator_y(south);
        let y1 = mercator_y(north);
        let height = ((f64::from(WIDTH) * (y1 - y0) / (x1 - x0)).round() as u32).max(2);
        let url = format!(
            "https://mesonet.agron.iastate.edu/cgi-bin/wms/nexrad/n0q-t.cgi\
             ?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS=nexrad-n0q-wmst\
             &STYLES=&CRS=EPSG:3857&BBOX={x0:.0},{y0:.0},{x1:.0},{y1:.0}\
             &WIDTH={WIDTH}&HEIGHT={height}&FORMAT=image/png"
        );
        let png = self
            .client
            .get(&url)
            .send()
            .and_then(|r| r.error_for_status())
            .with_context(|| format!("downloading {url}"))?
            .bytes()
            .context("reading the IEM N0Q body")?;
        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .context("decoding the IEM N0Q PNG")?
            .to_rgba8();
        let (width, height) = (decoded.width() as usize, decoded.height() as usize);
        let mut rgba = decoded.into_raw();
        // IEM's PNG is RGB; its no-data background is black. Keep black
        // readable as a low echo but make the empty background transparent so
        // the basemap is not washed out by an opaque black sheet.
        for px in rgba.chunks_exact_mut(4) {
            if px[0] < 8 && px[1] < 8 && px[2] < 8 {
                px[3] = 0;
            }
        }
        Ok(Level3Image {
            width,
            height,
            rgba,
            time: chrono::Utc::now(),
            south: bbox[0],
            west: bbox[1],
            north: bbox[2],
            east: bbox[3],
        })
    }
}

fn mercator_x(lon: f64) -> f64 {
    6_378_137.0 * lon.to_radians()
}

fn mercator_y(lat: f64) -> f64 {
    6_378_137.0
        * (std::f64::consts::PI / 4.0 + lat.to_radians() / 2.0)
            .tan()
            .ln()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn n0q_request_is_mercator() {
        let bbox = crate::local_overlay_box(35.0, -97.0);
        let x0 = mercator_x(bbox[1]);
        let y0 = mercator_y(bbox[0]);
        assert!(x0 < 0.0);
        assert!(y0 > 0.0);
        let _ = bbox;
    }
}
