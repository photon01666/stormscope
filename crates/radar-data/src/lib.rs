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
pub mod ambient;
pub mod cache;
pub mod ensemble;
pub mod forecast;
pub mod futurecast;
pub mod glm;
pub mod goes;
pub mod hrrr_field;
pub mod level3;
pub mod lsrs;
pub mod mcds;
pub mod metar;
pub mod midlevel;
pub mod mrms;
pub mod outlooks;
pub mod raob;
pub mod rap;
pub mod s3;
pub mod sites_gen;
pub mod sounding;
pub mod synoptic;
pub mod terrain;
pub mod update;
pub mod verify;
pub mod warnings_archive;

pub use alerts::AlertSource;
pub use ambient::{AmbientSource, AmbientStation};
pub use cache::DiskCache;
pub use ensemble::{EnsembleField, EnsembleSeries, EnsembleSource};
pub use forecast::{ForecastSource, StationTotals};
pub use hrrr_field::{FieldModel, HrrrField, HrrrFieldKind, HrrrFieldSource};
pub use level3::{Level3Image, Level3Source};
pub use lsrs::LsrSource;
pub use mcds::McdSource;
pub use metar::{MetarHistory, MetarObservation, MetarSource};
pub use midlevel::{MidLevelFrame, MidLevelRun, MidLevelSource};
pub use outlooks::OutlookSource;
pub use raob::RaobSource;
pub use rap::RapSource;
pub use s3::{ArchiveVolumeRef, RealtimePoller, S3Source};
pub use sounding::{SoundingSeries, SoundingSource};
pub use synoptic::{SynopticSource, SynopticStation};
pub use terrain::TerrainSource;
pub use verify::{QpeGaugePair, QpeVerification};
