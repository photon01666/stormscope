//! WPC Probabilistic Winter Precipitation Forecasts (PWPF).
//!
//! PWPF is distributed as KMZ (a zip containing one KML file) for each
//! product/day. Snow products are vector polygons/contours; ice products are
//! raster GroundOverlays in the same KMZ. This module holds the decoded types
//! shared by the fetcher and the map.

/// One PWPF product exposed in Map Controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PwpfProduct {
    SnowAmount50,
    SnowProb4,
    SnowProb8,
    SnowProb12,
    IceAmount50,
    IceProb025,
}

impl PwpfProduct {
    pub const ALL: [Self; 6] = [
        Self::SnowAmount50,
        Self::SnowProb4,
        Self::SnowProb8,
        Self::SnowProb12,
        Self::IceAmount50,
        Self::IceProb025,
    ];

    /// File-name prefix in WPC's `latest_kml_GE` directory.
    pub fn prefix(self) -> &'static str {
        match self {
            Self::SnowAmount50 => "percentile_24hsnow50",
            Self::SnowProb4 => "prb_24hsnow_ge04",
            Self::SnowProb8 => "prb_24hsnow_ge08",
            Self::SnowProb12 => "prb_24hsnow_ge12",
            Self::IceAmount50 => "percentile_24hicez50",
            Self::IceProb025 => "prb_24hicez_ge.25",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::SnowAmount50 => "Snow amount 50th",
            Self::SnowProb4 => "Snow ≥4\"",
            Self::SnowProb8 => "Snow ≥8\"",
            Self::SnowProb12 => "Snow ≥12\"",
            Self::IceAmount50 => "Ice amount 50th",
            Self::IceProb025 => "Ice ≥0.25\"",
        }
    }

    pub fn is_probability(self) -> bool {
        matches!(
            self,
            Self::SnowProb4 | Self::SnowProb8 | Self::SnowProb12 | Self::IceProb025
        )
    }

    pub fn unit(self) -> &'static str {
        if self.is_probability() { "%" } else { "in" }
    }
}

/// PWPF forecast day (24-hour accumulation windows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PwpfDay {
    Day1,
    Day2,
    Day3,
}

impl PwpfDay {
    pub const ALL: [Self; 3] = [Self::Day1, Self::Day2, Self::Day3];

    pub fn label(self) -> &'static str {
        match self {
            Self::Day1 => "Day 1",
            Self::Day2 => "Day 2",
            Self::Day3 => "Day 3",
        }
    }

    /// End of the 24-hour window (forecast hour in WPC's file names).
    pub fn forecast_hour(self) -> u16 {
        match self {
            Self::Day1 => 24,
            Self::Day2 => 48,
            Self::Day3 => 72,
        }
    }
}

/// One vector PWPF band (amount range or probability threshold).
#[derive(Clone, Debug)]
pub struct PwpfFeature {
    pub label: String,
    pub color: [u8; 3],
    pub polygons: Vec<Vec<Vec<(f64, f64)>>>,
}

/// A raster PWPF GroundOverlay (used by the ice products).
#[derive(Clone, Debug)]
pub struct PwpfRaster {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    /// `[south, west, north, east]` in degrees.
    pub bounds: [f64; 4],
}

#[derive(Clone, Debug)]
pub enum PwpfGeometry {
    Vector(Vec<PwpfFeature>),
    Raster(PwpfRaster),
}

/// One fetched PWPF product/day layer.
#[derive(Clone, Debug)]
pub struct PwpfLayer {
    pub product: PwpfProduct,
    pub day: PwpfDay,
    pub issue: String,
    pub valid: String,
    pub geometry: PwpfGeometry,
}
