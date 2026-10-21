//! GOES Geostationary Lightning Mapper (GLM) flashes.
//!
//! NOAA publishes GLM Level 2 LCFA files every 20 seconds to the public
//! `noaa-goes19` (GOES-East) and `noaa-goes18` (GOES-West) S3 buckets. Each
//! file is a NetCDF-4/HDF5 product with flash centroid latitude/longitude and
//! a millisecond time offset from the product start.
//!
//! The app fetches **one** 20-second file per refresh (about 280 KB) and
//! accumulates the flashes for 20 minutes on the map. That is roughly
//! 80 MB/day at a one-minute cadence, versus ~1.2 GB/day for every file.
//! Data is official NOAA public-domain, with no accounts or keys.

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Datelike, NaiveDate, Timelike, Utc};
use std::time::Duration;

/// One GLM flash centroid.
#[derive(Debug, Clone)]
pub struct GlmStrike {
    pub lat: f64,
    pub lon: f64,
    pub time: DateTime<Utc>,
}

/// The newest GLM file fetched for one satellite.
pub struct GlmFrame {
    /// "GOES-East" or "GOES-West".
    pub satellite: String,
    /// S3 key, used to avoid re-merging the same file.
    pub key: String,
    /// Product start time (from the file name).
    pub start: DateTime<Utc>,
    pub strikes: Vec<GlmStrike>,
}

pub struct GlmSource {
    client: reqwest::blocking::Client,
}

impl GlmSource {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(45))
                .user_agent(concat!("stormscope/", env!("CARGO_PKG_VERSION")))
                .build()
                .context("building the GLM HTTP client")?,
        })
    }

    /// Newest available file for the chosen satellite, falling back to the
    /// other one if that bucket has no recent files.
    pub fn latest(&self, west: bool) -> Result<GlmFrame> {
        let buckets: [(&str, &str); 2] = if west {
            [("noaa-goes18", "GOES-West"), ("noaa-goes19", "GOES-East")]
        } else {
            [("noaa-goes19", "GOES-East"), ("noaa-goes18", "GOES-West")]
        };
        let mut last_error = None;
        for (bucket, label) in buckets {
            match self.latest_from(bucket, label) {
                Ok(frame) => return Ok(frame),
                Err(error) => {
                    log::debug!("GLM {bucket}: {error:#}");
                    last_error = Some(error);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow!("no GLM satellite available")))
    }

    fn latest_from(&self, bucket: &str, label: &str) -> Result<GlmFrame> {
        let key = self.latest_key(bucket)?;
        let start = start_from_key(&key).ok_or_else(|| anyhow!("unrecognised GLM key {key}"))?;
        let url = format!("https://{bucket}.s3.amazonaws.com/{key}");
        let bytes = self
            .client
            .get(&url)
            .send()
            .context("requesting the GLM file")?
            .error_for_status()
            .with_context(|| format!("GET {url}"))?
            .bytes()
            .context("reading the GLM file")?;
        if bytes.is_empty() {
            return Err(anyhow!("empty GLM file {key}"));
        }

        // NcFile::open needs a path; overwrite one small scratch file rather
        // than caching hundreds of MB of 20-second products.
        let dir = crate::cache::DiskCache::default_root().join("glm");
        std::fs::create_dir_all(&dir).context("creating the GLM scratch dir")?;
        let path = dir.join(format!("latest-{bucket}.nc"));
        std::fs::write(&path, &bytes).context("writing the GLM scratch file")?;

        let file = netcdf_reader::NcFile::open(&path)
            .with_context(|| format!("opening GLM NetCDF {}", path.display()))?;
        let lat = file
            .read_variable_as_f64("flash_lat")
            .context("GLM flash_lat")?;
        let lon = file
            .read_variable_as_f64("flash_lon")
            .context("GLM flash_lon")?;
        let offset = file
            .read_variable_as_f64("flash_time_offset_of_first_event")
            .context("GLM flash_time_offset_of_first_event")?;
        let mut strikes = Vec::with_capacity(lat.len().min(lon.len()).min(offset.len()));
        for ((&la, &lo), &ms) in lat.iter().zip(lon.iter()).zip(offset.iter()) {
            if !la.is_finite() || !lo.is_finite() || !ms.is_finite() {
                continue;
            }
            if !(-90.0..=90.0).contains(&la) || !(-180.0..=180.0).contains(&lo) {
                continue;
            }
            // GLM offsets are milliseconds from the product start.
            let time = start + chrono::Duration::milliseconds(ms.round().max(0.0) as i64);
            strikes.push(GlmStrike {
                lat: la,
                lon: lo,
                time,
            });
        }
        log::debug!(
            "GLM {label}: {} flashes from {}",
            strikes.len(),
            key.rsplit('/').next().unwrap_or("?")
        );
        Ok(GlmFrame {
            satellite: label.to_string(),
            key,
            start,
            strikes,
        })
    }

    /// The newest `.nc` key in the current (or recent) hour folder.
    fn latest_key(&self, bucket: &str) -> Result<String> {
        let now = Utc::now();
        for back in 0..3 {
            let at = now - chrono::Duration::hours(back);
            let prefix = format!(
                "GLM-L2-LCFA/{:04}/{:03}/{:02}/",
                at.year(),
                at.ordinal(),
                at.hour()
            );
            let url = format!(
                "https://{bucket}.s3.amazonaws.com/?list-type=2&prefix={prefix}&max-keys=1000"
            );
            let xml = self
                .client
                .get(&url)
                .send()
                .context("listing the GLM bucket")?
                .error_for_status()
                .with_context(|| format!("GET {url}"))?
                .text()
                .context("reading the GLM listing")?;
            if let Some(key) = keys_from_listing(&xml).last() {
                return Ok(key.clone());
            }
        }
        Err(anyhow!("no GLM files for {bucket} in the last three hours"))
    }
}

/// `<Key>...` values from an S3 ListBucketResult XML, in listing (time) order.
fn keys_from_listing(xml: &str) -> Vec<String> {
    xml.split("<Key>")
        .skip(1)
        .filter_map(|chunk| chunk.split("</Key>").next())
        .filter(|key| key.ends_with(".nc"))
        .map(str::to_string)
        .collect()
}

/// `OR_GLM-L2-LCFA_G19_s20262940600000_e...nc` -> 2026-10-21 06:00:00 UTC.
/// The stamp is YYYYJJJHHMMSS (year, day-of-year, hour, minute, second).
fn start_from_key(key: &str) -> Option<DateTime<Utc>> {
    let name = key.rsplit('/').next()?;
    let at = name.find("_s")? + 2;
    let stamp = name.get(at..at + 13)?;
    let year: i32 = stamp.get(0..4)?.parse().ok()?;
    let day: u32 = stamp.get(4..7)?.parse().ok()?;
    let hour: u32 = stamp.get(7..9)?.parse().ok()?;
    let minute: u32 = stamp.get(9..11)?.parse().ok()?;
    let second: u32 = stamp.get(11..13)?.parse().ok()?;
    let date = NaiveDate::from_yo_opt(year, day)?;
    let naive = date.and_hms_opt(hour, minute, second)?;
    Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_glm_start_stamp() {
        let key = "GLM-L2-LCFA/2026/294/06/OR_GLM-L2-LCFA_G19_s20262940600000_e20262940600200_c20262940600215.nc";
        let start = start_from_key(key).expect("a start");
        assert_eq!(start.to_rfc3339(), "2026-10-21T06:00:00+00:00");
    }

    #[test]
    fn extracts_keys_from_a_listing() {
        let xml = "<ListBucketResult><Contents><Key>GLM-L2-LCFA/2026/294/06/a.nc</Key></Contents>\
                   <Contents><Key>GLM-L2-LCFA/2026/294/06/b.nc</Key></Contents></ListBucketResult>";
        assert_eq!(
            keys_from_listing(xml),
            vec![
                "GLM-L2-LCFA/2026/294/06/a.nc".to_string(),
                "GLM-L2-LCFA/2026/294/06/b.nc".to_string()
            ]
        );
    }

    /// Real fetch + decode (network, ignored by default).
    #[test]
    #[ignore = "needs network access to noaa-goes19"]
    fn fetches_and_decodes_a_real_glm_frame() {
        let frame = GlmSource::new()
            .expect("client")
            .latest(false)
            .expect("a GLM frame");
        eprintln!(
            "{} {}Z: {} flashes from {}",
            frame.satellite,
            frame.start.format("%H:%M:%S"),
            frame.strikes.len(),
            frame.key.rsplit('/').next().unwrap_or("?")
        );
        for strike in frame.strikes.iter().take(3) {
            assert!((-90.0..=90.0).contains(&strike.lat));
            assert!((-180.0..=180.0).contains(&strike.lon));
        }
    }
}
