//! Terrarium terrain tiles from the AWS Open Data elevation set, assembled
//! into a small grid around a radar site for beam-blockage masking.

use anyhow::{Context, Result, bail};
use radar_core::terrain::TerrainGrid;

const TILE_SIZE: usize = 256;
const GRID_TILES: i32 = 5;

pub struct TerrainSource {
    client: reqwest::blocking::Client,
}

impl TerrainSource {
    pub fn new(user_agent: &str) -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(user_agent)
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(Self { client })
    }

    /// Fetch a 5×5 tile grid around the site. Tries z=8 (≈475 m/px), then
    /// falls back to z=7 if the higher-resolution fetch fails.
    pub fn for_site(&self, lat: f64, lon: f64) -> Result<TerrainGrid> {
        let mut last = None;
        for z in [8u32, 7] {
            match self.fetch_grid(lat, lon, z) {
                Ok(g) => return Ok(g),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("terrain fetch failed")))
    }

    fn fetch_grid(&self, lat: f64, lon: f64, z: u32) -> Result<TerrainGrid> {
        let n = 1i64 << z;
        let (cx, cy) = lonlat_to_tile(lat, lon, z);
        let x0 = cx.floor() as i64 - (GRID_TILES / 2) as i64;
        let y0 = cy.floor() as i64 - (GRID_TILES / 2) as i64;
        let width = TILE_SIZE * GRID_TILES as usize;
        let height = width;
        let mut elevations = vec![0.0f32; width * height];
        for dy in 0..GRID_TILES {
            for dx in 0..GRID_TILES {
                let tx = (x0 + dx as i64).rem_euclid(n);
                let ty = y0 + dy as i64;
                if ty < 0 || ty >= n {
                    bail!("terrain tile row outside the world");
                }
                let url = format!(
                    "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{tx}/{ty}.png"
                );
                let bytes = self
                    .client
                    .get(&url)
                    .send()
                    .with_context(|| format!("GET {url}"))?
                    .error_for_status()
                    .with_context(|| format!("GET {url}"))?
                    .bytes()
                    .context("reading terrain tile")?;
                let img = image::load_from_memory(&bytes)
                    .context("decoding Terrarium PNG")?
                    .to_rgb8();
                if img.width() as usize != TILE_SIZE || img.height() as usize != TILE_SIZE {
                    bail!("terrain tile was {}x{}", img.width(), img.height());
                }
                for py in 0..TILE_SIZE {
                    for px in 0..TILE_SIZE {
                        let p = img.get_pixel(px as u32, py as u32);
                        let elev =
                            p[0] as f32 * 256.0 + p[1] as f32 + p[2] as f32 / 256.0 - 32768.0;
                        let gx = dx as usize * TILE_SIZE + px;
                        let gy = dy as usize * TILE_SIZE + py;
                        elevations[gy * width + gx] = elev;
                    }
                }
            }
        }
        let west = tile_lon(x0, z);
        let east = tile_lon(x0 + GRID_TILES as i64, z);
        let north = tile_lat(y0, z);
        let south = tile_lat(y0 + GRID_TILES as i64, z);
        Ok(TerrainGrid {
            north,
            south,
            west,
            east,
            width,
            height,
            elevations,
        })
    }
}

fn lonlat_to_tile(lat: f64, lon: f64, z: u32) -> (f64, f64) {
    let n = (1u64 << z) as f64;
    let x = (lon + 180.0) / 360.0 * n;
    let lat_rad = lat.to_radians();
    let y = (1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n;
    (x, y)
}

fn tile_lon(x: i64, z: u32) -> f64 {
    let n = (1u64 << z) as f64;
    x as f64 / n * 360.0 - 180.0
}

fn tile_lat(y: i64, z: u32) -> f64 {
    let n = (1u64 << z) as f64;
    let n2 = std::f64::consts::PI * (1.0 - 2.0 * y as f64 / n);
    n2.sinh().atan().to_degrees()
}
