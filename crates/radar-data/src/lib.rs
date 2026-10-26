//! radar-data — S3 fetch/cache/live polling + NWS alerts + site catalog.
//!

/// The local window the data overlays (MRMS, GOES) fetch around a radar:
/// ±5° latitude and ±8° longitude — roughly 1100 × 900 km, so the imagery is
/// sharp at the map's working zoom instead of a blocky continental mosaic.
pub fn local_overlay_box(site_lat: f64, site_lon: f64) -> [f64; 4] {
    [
        site_lat - 5.0,
        site_lon - 8.0,
        site_lat + 5.0,
        site_lon + 8.0,
    ]
}

pub mod ai;
pub mod alerts;
pub mod cache;
pub mod forecast;
pub mod glm;
pub mod goes;
pub mod lsrs;
pub mod mcds;
pub mod mrms;
pub mod outlooks;
pub mod s3;
pub mod sites_gen;
pub mod update;
pub mod warnings_archive;

pub use alerts::AlertSource;
pub use cache::DiskCache;
pub use forecast::{ForecastSource, StationTotals};
pub use lsrs::LsrSource;
pub use mcds::McdSource;
pub use outlooks::OutlookSource;
pub use s3::{ArchiveVolumeRef, RealtimePoller, S3Source};
