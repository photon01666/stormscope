//! radar-data — S3 fetch/cache/live polling + NWS alerts + site catalog.
//!
//! Workstream W2. See PLAN.md.

pub mod alerts;
pub mod cache;
pub mod outlooks;
pub mod s3;
pub mod sites_gen;

pub use alerts::AlertSource;
pub use cache::DiskCache;
pub use outlooks::OutlookSource;
pub use s3::{ArchiveVolumeRef, RealtimePoller, S3Source};
