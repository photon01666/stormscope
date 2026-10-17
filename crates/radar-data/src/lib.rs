//! radar-data — S3 fetch/cache/live polling + NWS alerts + site catalog.
//!

pub mod alerts;
pub mod cache;
pub mod forecast;
pub mod lsrs;
pub mod mcds;
pub mod outlooks;
pub mod s3;
pub mod sites_gen;
pub mod warnings_archive;

pub use alerts::AlertSource;
pub use cache::DiskCache;
pub use forecast::{ForecastSource, StationTotals};
pub use lsrs::LsrSource;
pub use mcds::McdSource;
pub use outlooks::OutlookSource;
pub use s3::{ArchiveVolumeRef, RealtimePoller, S3Source};
