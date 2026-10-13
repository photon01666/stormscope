//! SPC convective-outlook model (categorical risk areas).
//!
//! One [`Outlook`] is the full set of nested risk polygons for a single day
//! (Day 1/2/3). SPC's GeoJSON carries its own official stroke/fill colors, so
//! the renderer reproduces SPC's palette rather than inventing one.

use chrono::{DateTime, Utc};

/// One categorized risk area (TSTM/MRGL/SLGT/ENH/MDT/HIGH) with its
/// geometry and the exact colors SPC assigned it.
#[derive(Clone, Debug)]
pub struct OutlookFeature {
    /// SPC numeric category: 2=TSTM, 3=MRGL, 4=SLGT, 5=ENH, 6=MDT, 8=HIGH.
    pub dn: u16,
    /// Short category label, e.g. "SLGT".
    pub label: String,
    /// Human label, e.g. "Slight Risk".
    pub name: String,
    /// Outline color (RGB), taken verbatim from SPC.
    pub stroke: [u8; 3],
    /// Fill color (RGB), taken verbatim from SPC.
    pub fill: [u8; 3],
    /// Geometry: outer index = separate polygons (GeoJSON MultiPolygon),
    /// next index = rings of one polygon (`[exterior, hole, ...]`), and each
    /// ring is a list of `(lon, lat)` vertices.
    pub polygons: Vec<Vec<Vec<(f64, f64)>>>,
}

/// A day's categorical convective outlook.
#[derive(Clone, Debug)]
pub struct Outlook {
    /// Outlook day, `1..=3` (Days 4-8 are probability-only upstream).
    pub day: u8,
    pub issue: Option<DateTime<Utc>>,
    pub valid: Option<DateTime<Utc>>,
    pub expire: Option<DateTime<Utc>>,
    pub forecaster: String,
    pub features: Vec<OutlookFeature>,
}

impl Outlook {
    /// Features in increasing severity so higher categories paint on top of
    /// lower ones (the inner ENH/MDT/HIGH shapes stay visible).
    pub fn sorted_features(&self) -> Vec<&OutlookFeature> {
        let mut f: Vec<&OutlookFeature> = self.features.iter().collect();
        f.sort_by_key(|x| x.dn);
        f
    }
}
