//! radar-data — S3 fetch/cache/live polling + NWS alerts + site catalog.
//!
//! Workstream W2. See PLAN.md.

pub mod alerts;
pub mod cache;
pub mod s3;
pub mod sites_gen;

pub use alerts::AlertSource;
pub use cache::DiskCache;
pub use s3::{ArchiveVolumeRef, S3Source};
