//! A small sampled terrain grid for beam-blockage masking.
//!
//! The grid is fetched as Terrarium elevation tiles by `radar-data`; this
//! module only stores and samples it. Resolution is deliberately coarse: the
//! mask answers "is the gate centre below the terrain?" not a full
//! ray-tracing obstruction calculation.

#[derive(Clone, Debug)]
pub struct TerrainGrid {
    pub north: f64,
    pub south: f64,
    pub west: f64,
    pub east: f64,
    pub width: usize,
    pub height: usize,
    /// Row-major elevations, metres MSL, north-to-south.
    pub elevations: Vec<f32>,
}

impl TerrainGrid {
    /// Bilinear terrain elevation at a coordinate, or `None` outside the grid.
    pub fn sample(&self, lat: f64, lon: f64) -> Option<f32> {
        if self.width < 2 || self.height < 2 || self.elevations.len() < self.width * self.height {
            return None;
        }
        if lat > self.north || lat < self.south || lon < self.west || lon > self.east {
            return None;
        }
        let u =
            ((lon - self.west) / (self.east - self.west)).clamp(0.0, 1.0) * (self.width - 1) as f64;
        let v = ((self.north - lat) / (self.north - self.south)).clamp(0.0, 1.0)
            * (self.height - 1) as f64;
        let x0 = u.floor() as usize;
        let y0 = v.floor() as usize;
        let x1 = (x0 + 1).min(self.width - 1);
        let y1 = (y0 + 1).min(self.height - 1);
        let fx = (u - x0 as f64) as f32;
        let fy = (v - y0 as f64) as f32;
        let a = self.elevations[y0 * self.width + x0];
        let b = self.elevations[y0 * self.width + x1];
        let c = self.elevations[y1 * self.width + x0];
        let d = self.elevations[y1 * self.width + x1];
        Some(a * (1.0 - fx) * (1.0 - fy) + b * fx * (1.0 - fy) + c * (1.0 - fx) * fy + d * fx * fy)
    }
}
