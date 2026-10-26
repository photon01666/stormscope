//! HRRR simulated composite reflectivity — the data behind a TV-style
//! "Futurecast".
//!
//! NOAA's High-Resolution Rapid Refresh (HRRR) predicts precipitation
//! particles; a post-processing step turns those into simulated radar
//! reflectivity in dBZ. The `REFC` field is the column-maximum composite,
//! which is the closest public equivalent to the future-radar loop stations
//! show. It is a model forecast, **not** an observation from a radar beam.
//!
//! We never download a whole HRRR forecast file (~147 MB). A
//! `filter_hrrr_2d.pl` request returns only `REFC` for the whole CONUS
//! (typically ~200–500 KB), and the `grib` crate decodes the Lambert
//! conformal grid. Each point is splatted into a Web-Mercator texture so the
//! map can draw it exactly like its other georeferenced overlays.
//!
//! The S3 `.idx` + HTTP Range path is the fallback when NOMADS is busy: the
//! `REFC` GRIB2 message alone is ~0.4–0.6 MB instead of the full file.

use std::io::Cursor;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Datelike, Duration, TimeZone, Timelike, Utc};
use grib::LatLons;

use crate::cache::DiskCache;

const NOMADS: &str = "https://nomads.ncep.noaa.gov/cgi-bin/filter_hrrr_2d.pl";
const S3: &str = "https://noaa-hrrr-bdp-pds.s3.amazonaws.com";

/// Output texture width. HRRR's 3 km CONUS grid is 1799 columns; 1024 px
/// keeps the whole-US view sharp enough without storing a 4K texture per
/// forecast hour.
pub const TEXTURE_WIDTH: usize = 1024;
/// Every hourly HRRR run carries this many hours.
pub const SHORT_MAX_FORECAST_HOUR: u16 = 18;
/// The 00/06/12/18Z runs extend to two days.
pub const EXTENDED_MAX_FORECAST_HOUR: u16 = 48;
/// Cache the raw local GRIB subsets for both horizons (roughly 30 MB).
const CACHE_BYTES: u64 = 64 * 1024 * 1024;

/// The CONUS domain HRRR's standard product covers.
pub const CONUS: [f64; 4] = [24.0, -126.0, 50.0, -66.0];

/// The FutureCast window: HRRR's whole standard CONUS domain. The old
/// implementation fetched only a ~18° box around the selected radar, which
/// left a hard edge through the Dakotas/Minnesota whenever the map was
/// zoomed out. The full CONUS GRIB subset is only ~0.4 MB/frame; the decode
/// is the larger cost, and the app streams frames in parallel.
pub fn forecast_box(_site_lat: f64, _site_lon: f64) -> [f64; 4] {
    CONUS
}

/// Whether the selected radar is inside HRRR's standard CONUS domain.
pub fn supported(lat: f64, lon: f64) -> bool {
    (CONUS[0]..=CONUS[2]).contains(&lat) && (CONUS[1]..=CONUS[3]).contains(&lon)
}

/// One forecast step in a run's plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameInfo {
    /// Forecast hour from the model initialization (f01..f18).
    pub hour: u16,
    /// Valid time (UTC).
    pub valid: DateTime<Utc>,
}

/// A complete run plan: initialization time plus the future hours to fetch.
#[derive(Clone, Debug)]
pub struct Plan {
    pub init: DateTime<Utc>,
    pub frames: Vec<FrameInfo>,
    /// False = latest 18-hour hourly cycle; true = 48-hour extended cycle
    /// (only 00/06/12/18Z, so it can be a few hours older).
    pub extended: bool,
}

/// One decoded forecast image, in Web Mercator, quantized to 1 dBZ.
pub struct Frame {
    pub init: DateTime<Utc>,
    pub hour: u16,
    pub valid: DateTime<Utc>,
    pub width: usize,
    pub height: usize,
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
    /// Row-major, north row first, west column first. 0 = no data (hole);
    /// otherwise dBZ + 32 (so every real value is 1..=255).
    pub dbz: Vec<u8>,
    /// HRRR categorical precipitation type at the same pixels: 0 unknown,
    /// 1 rain, 2 snow, 3 ice pellets, 4 freezing rain. Zero also means the
    /// source (the S3 fallback) did not carry type fields.
    pub ptype: Vec<u8>,
}

impl Frame {
    /// Simulated reflectivity at a geographic point, if the forecast has an
    /// echo at that pixel. `None` means no data or below the model's -10 dBZ
    /// floor; the UI's color table then draws nothing.
    pub fn dbz_at(&self, lat: f64, lon: f64) -> Option<f32> {
        let (x, y) = self.pixel_at(lat, lon)?;
        let q = self.dbz[y * self.width + x];
        (q != 0).then_some(q as f32 - 32.0)
    }

    /// HRRR categorical precipitation type at a point, when available.
    pub fn ptype_at(&self, lat: f64, lon: f64) -> Option<u8> {
        let (x, y) = self.pixel_at(lat, lon)?;
        let t = self.ptype[y * self.width + x];
        (t != 0).then_some(t)
    }

    /// Strongest echo in the frame, if any.
    pub fn max_dbz(&self) -> Option<f32> {
        self.dbz
            .iter()
            .copied()
            .filter(|q| *q != 0)
            .max()
            .map(|q| q as f32 - 32.0)
    }

    fn pixel_at(&self, lat: f64, lon: f64) -> Option<(usize, usize)> {
        if !lat.is_finite() || !lon.is_finite() || lat < self.south || lat > self.north {
            return None;
        }
        let x0 = merc_x(self.west);
        let x1 = merc_x(self.east);
        let y0 = merc_y(self.south);
        let y1 = merc_y(self.north);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let mx = merc_x(lon);
        let my = merc_y(lat);
        if mx < x0 || mx > x1 || my < y0 || my > y1 {
            return None;
        }
        let x = ((mx - x0) / (x1 - x0) * (self.width.saturating_sub(1) as f64)).round() as usize;
        let y = ((y1 - my) / (y1 - y0) * (self.height.saturating_sub(1) as f64)).round() as usize;
        Some((x.min(self.width - 1), y.min(self.height - 1)))
    }
}

/// Fetch + decode driver. One instance per background job (its HTTP client
/// is blocking, like the other overlay sources).
pub struct FuturecastSource {
    client: reqwest::blocking::Client,
    cache: DiskCache,
}

impl FuturecastSource {
    pub fn new() -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!(
                "stormscope/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com/photon01666/stormscope)"
            ))
            .timeout(std::time::Duration::from_secs(75))
            .connect_timeout(std::time::Duration::from_secs(12))
            .build()
            .context("building the Futurecast HTTP client")?;
        let cache = DiskCache::new(DiskCache::default_root().join("futurecast"), CACHE_BYTES);
        Ok(Self { client, cache })
    }

    /// Find the newest complete HRRR cycle for the requested horizon and
    /// build its whole frame list. The UI starts on the first future frame
    /// but the strip exposes the complete run, including hours already valid.
    pub fn plan(&self, now: DateTime<Utc>, bbox: [f64; 4], extended: bool) -> Result<Plan> {
        if bbox[2] <= bbox[0] || bbox[3] <= bbox[1] {
            bail!("invalid Futurecast box {bbox:?}");
        }
        let init = self.latest_run(now, extended)?;
        plan_for(init, now, extended)
    }

    /// Download + decode one frame in the plan.
    pub fn fetch(&self, plan: &Plan, info: &FrameInfo, bbox: [f64; 4]) -> Result<Frame> {
        let bytes = self.grib_bytes(plan.init, info.hour, bbox)?;
        decode_frame(&bytes, plan.init, info, bbox)
    }

    /// Trim the FutureCast cache after a run finishes. Concurrent frame
    /// workers must not sweep while another is writing, so this is called
    /// once by the coordinator after all workers join.
    pub fn sweep_cache(&self) {
        let _ = self.cache.sweep();
    }

    fn latest_run(&self, now: DateTime<Utc>, extended: bool) -> Result<DateTime<Utc>> {
        // A completed cycle has its last forecast hour. Checking that key
        // means we never pick a run whose later frames are still uploading.
        let last_hour = if extended {
            EXTENDED_MAX_FORECAST_HOUR
        } else {
            SHORT_MAX_FORECAST_HOUR
        };
        for back in 1..=30 {
            let t = now - Duration::hours(back);
            // Extended runs exist only at 00/06/12/18 UTC.
            if extended && t.hour() % 6 != 0 {
                continue;
            }
            if !extended && back > 8 {
                break;
            }
            let init = Utc
                .with_ymd_and_hms(t.year(), t.month(), t.day(), t.hour(), 0, 0)
                .single()
                .ok_or_else(|| anyhow!("invalid HRRR cycle time"))?;
            let key = format!(
                "hrrr.{}/conus/hrrr.t{:02}z.wrfsfcf{:02}.grib2",
                init.format("%Y%m%d"),
                init.hour(),
                last_hour
            );
            let url = format!("{S3}/{key}");
            match self.client.head(&url).send() {
                Ok(r) if r.status().is_success() => {
                    log::debug!(
                        "Futurecast: latest complete HRRR run {init} (extended={extended})"
                    );
                    return Ok(init);
                }
                Ok(_) => {}
                Err(e) => log::debug!("Futurecast: HRRR f{last_hour} check failed for {init}: {e}"),
            }
        }
        bail!(
            "no complete {}-hour HRRR run found",
            if extended { 48 } else { 18 }
        )
    }

    fn grib_bytes(&self, init: DateTime<Utc>, hour: u16, bbox: [f64; 4]) -> Result<Vec<u8>> {
        let base = cache_stem(init, hour, bbox);
        let s3_rel = format!("{base}_s3_types2.grib2");
        if let Some(bytes) = self.cache.read(&s3_rel)? {
            if bytes.starts_with(b"GRIB") {
                log::debug!("Futurecast: cache hit {s3_rel}");
                return Ok(bytes);
            }
        }
        // The full-CONUS product is most efficient from S3: the REFC message
        // plus the tiny categorical-type tail is ~0.5 MB, versus ~6.4 MB for
        // NOMADS' re-encoded multi-field GRIB. NOMADS remains the fallback.
        match self.fetch_s3(init, hour) {
            Ok(bytes) if bytes.starts_with(b"GRIB") => {
                let _ = self.cache.write(&s3_rel, &bytes);
                log::debug!(
                    "Futurecast: S3 REFC+types f{:02} {} bytes",
                    hour,
                    bytes.len()
                );
                return Ok(bytes);
            }
            Ok(bytes) => {
                log::warn!(
                    "Futurecast: S3 returned {} bytes that are not GRIB; trying NOMADS",
                    bytes.len()
                );
            }
            Err(e) => {
                log::warn!("Futurecast: S3 f{:02} failed ({e:#}); trying NOMADS", hour);
            }
        }
        let nomads_rel = format!("{base}_nomads_types.grib2");
        if let Some(bytes) = self.cache.read(&nomads_rel)? {
            if bytes.starts_with(b"GRIB") {
                log::debug!("Futurecast: cache hit {nomads_rel}");
                return Ok(bytes);
            }
        }

        match self.fetch_nomads(init, hour, bbox) {
            Ok(bytes) if bytes.starts_with(b"GRIB") => {
                let _ = self.cache.write(&nomads_rel, &bytes);
                log::debug!("Futurecast: NOMADS f{:02} {} bytes", hour, bytes.len());
                Ok(bytes)
            }
            Ok(bytes) => {
                log::warn!(
                    "Futurecast: NOMADS returned {} bytes that are not GRIB; trying S3",
                    bytes.len()
                );
                self.fetch_s3(init, hour)
            }
            Err(e) => {
                log::warn!("Futurecast: NOMADS f{:02} failed ({e:#}); trying S3", hour);
                self.fetch_s3(init, hour)
            }
        }
    }

    fn fetch_nomads(&self, init: DateTime<Utc>, hour: u16, bbox: [f64; 4]) -> Result<Vec<u8>> {
        let [south, west, north, east] = bbox;
        let url = format!(
            "{NOMADS}?dir=%2Fhrrr.{date}%2Fconus\
             &file=hrrr.t{run:02}z.wrfsfcf{hour:02}.grib2\
             &var_REFC=on&var_CSNOW=on&var_CICEP=on&var_CFRZR=on&var_CRAIN=on&var_CPOFP=on\
             &subregion=\
             &leftlon={west:.3}&rightlon={east:.3}\
             &toplat={north:.3}&bottomlat={south:.3}",
            date = init.format("%Y%m%d"),
            run = init.hour(),
            hour = hour,
        );
        let bytes = self
            .client
            .get(&url)
            .send()
            .and_then(|r| r.error_for_status())
            .with_context(|| format!("GET NOMADS HRRR f{hour:02}"))?
            .bytes()
            .context("reading the NOMADS HRRR body")?
            .to_vec();
        if bytes.is_empty() {
            bail!("NOMADS returned an empty HRRR body");
        }
        Ok(bytes)
    }

    /// Fetch the `REFC` message plus the categorical-type tail from the
    /// public HRRR S3 object. The `.idx` inventory gives byte ranges, so we
    /// never download the ~147 MB file.
    fn fetch_s3(&self, init: DateTime<Utc>, hour: u16) -> Result<Vec<u8>> {
        let key = format!(
            "hrrr.{}/conus/hrrr.t{:02}z.wrfsfcf{:02}.grib2",
            init.format("%Y%m%d"),
            init.hour(),
            hour
        );
        let idx_url = format!("{S3}/{key}.idx");
        let idx = self
            .client
            .get(&idx_url)
            .send()
            .and_then(|r| r.error_for_status())
            .with_context(|| format!("GET {idx_url}"))?
            .text()
            .context("reading the HRRR .idx")?;
        let (start, end) = refc_byte_range(&idx)
            .ok_or_else(|| anyhow!("HRRR .idx has no REFC record: {idx_url}"))?;
        let url = format!("{S3}/{key}");
        let refc = self.range_bytes(&url, start, Some(end))?;
        if !refc.starts_with(b"GRIB") {
            bail!("HRRR REFC range did not return a GRIB2 message");
        }
        let mut out = refc;
        // CSNOW/CICEP/CFRZR/CRAIN are consecutive tiny records at the end
        // of the file. One bounded range covers all four; never use an
        // open-ended range from CSNOW (it can pull tens of MB of object).
        if let (Some((start, _)), Some((_, end))) = (
            field_byte_range(&idx, "CSNOW"),
            field_byte_range(&idx, "CRAIN"),
        ) {
            match self.range_bytes(&url, start, Some(end)) {
                Ok(tail) if tail.starts_with(b"GRIB") && tail.len() < 256 * 1024 => {
                    out.extend_from_slice(&tail);
                }
                Ok(tail) => log::debug!(
                    "Futurecast: ignoring {} bytes of HRRR type tail",
                    tail.len()
                ),
                Err(e) => log::debug!("Futurecast: HRRR type tail failed: {e:#}"),
            }
        }
        log::debug!(
            "Futurecast: S3 REFC+types for f{:02} {} bytes",
            hour,
            out.len()
        );
        Ok(out)
    }

    fn range_bytes(&self, url: &str, start: u64, end: Option<u64>) -> Result<Vec<u8>> {
        let range = match end {
            Some(end) => format!("bytes={start}-{end}"),
            None => format!("bytes={start}-"),
        };
        let resp = self
            .client
            .get(url)
            .header(reqwest::header::RANGE, range)
            .send()
            .and_then(|r| r.error_for_status())
            .with_context(|| format!("GET {url} bytes {start}..{end:?}"))?;
        if resp.status() == reqwest::StatusCode::OK
            && resp.content_length().is_some_and(|n| n > 8 * 1024 * 1024)
        {
            bail!(
                "HRRR range request was ignored ({} bytes)",
                resp.content_length().unwrap_or(0)
            );
        }
        let bytes = resp.bytes().context("reading the HRRR range")?.to_vec();
        if bytes.is_empty() {
            bail!("HRRR range returned an empty body");
        }
        Ok(bytes)
    }
}

/// Pure run-plan arithmetic, separated so it can be tested without network.
///
/// This deliberately includes **all 18 hourly frames** of the run, even the
/// first hours that are already valid. The UI selects the first future frame
/// when the layer is opened, and the scrubber can still walk back to the
/// start of the model run.
pub fn plan_for(init: DateTime<Utc>, _now: DateTime<Utc>, extended: bool) -> Result<Plan> {
    let last = if extended {
        EXTENDED_MAX_FORECAST_HOUR
    } else {
        SHORT_MAX_FORECAST_HOUR
    };
    let frames: Vec<FrameInfo> = (1..=last)
        .map(|hour| FrameInfo {
            hour,
            valid: init + Duration::hours(hour as i64),
        })
        .collect();
    if frames.is_empty() {
        bail!("HRRR run {init} has no forecast frames");
    }
    Ok(Plan {
        init,
        frames,
        extended,
    })
}

/// Cache path stem for one run/hour/box. The box is rounded to 0.01° so
/// every site gets a stable name.
fn cache_stem(init: DateTime<Utc>, hour: u16, bbox: [f64; 4]) -> String {
    format!(
        "{}/f{:02}_{:.2}_{:.2}_{:.2}_{:.2}",
        init.format("%Y%m%d%H"),
        hour,
        bbox[0],
        bbox[1],
        bbox[2],
        bbox[3]
    )
}

/// Find the byte range of the first `REFC` message in a `.idx` inventory.
fn refc_byte_range(idx: &str) -> Option<(u64, u64)> {
    let lines: Vec<&str> = idx.lines().collect();
    let pos = lines
        .iter()
        .position(|line| line.split(':').nth(3) == Some("REFC"))?;
    let start: u64 = lines[pos].split(':').nth(1)?.parse().ok()?;
    let next: Option<u64> = lines
        .get(pos + 1)
        .and_then(|line| line.split(':').nth(1))
        .and_then(|s| s.parse().ok());
    // If REFC is the last record (it normally is not), 4 MB is safely more
    // than the message while still far below a full HRRR file.
    let end = next.map_or(start + 4 * 1024 * 1024, |n| n.saturating_sub(1));
    (end > start).then_some((start, end))
}

/// Byte range of a named `.idx` record, using the next record's offset or
/// an open-ended range when it is the last record.
fn field_byte_range(idx: &str, name: &str) -> Option<(u64, u64)> {
    let lines: Vec<&str> = idx.lines().collect();
    let pos = lines
        .iter()
        .position(|line| line.split(':').nth(3) == Some(name))?;
    let start: u64 = lines[pos].split(':').nth(1)?.parse().ok()?;
    let next: Option<u64> = lines
        .get(pos + 1)
        .and_then(|line| line.split(':').nth(1))
        .and_then(|s| s.parse().ok());
    let end = next.map_or(start + 64 * 1024, |n| n.saturating_sub(1));
    (end > start).then_some((start, end))
}

fn decode_frame(
    bytes: &[u8],
    init: DateTime<Utc>,
    info: &FrameInfo,
    bbox: [f64; 4],
) -> Result<Frame> {
    if !bytes.starts_with(b"GRIB") {
        bail!("HRRR REFC body is not GRIB2");
    }
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Field {
        Refc,
        Rain,
        Snow,
        Ice,
        Fzra,
        Pofp,
    }
    fn priority(t: u8) -> u8 {
        match t {
            4 => 4, // freezing rain
            3 => 3, // ice pellets
            2 => 2, // snow
            1 => 1, // rain
            _ => 0,
        }
    }

    let [south, west, north, east] = bbox;
    let (x0, x1) = (merc_x(west), merc_x(east));
    let (y0, y1) = (merc_y(south), merc_y(north));
    if x1 <= x0 || y1 <= y0 {
        bail!("invalid Futurecast Mercator box");
    }
    let width = TEXTURE_WIDTH;
    let height = ((width as f64 * (y1 - y0) / (x1 - x0)).round() as usize).clamp(64, 2048);
    let mut dbz = vec![0u8; width * height];
    let mut ptype = vec![0u8; width * height];
    let mut frozen_pct = vec![0u8; width * height];
    let mut points = 0usize;
    let mut inside = 0usize;
    let mut found_refc = false;

    let grib2 = grib::from_reader(Cursor::new(bytes)).context("parsing HRRR GRIB2")?;
    for ((_mi, _si), sub) in grib2.iter() {
        let pd = sub.prod_def();
        let field = match (pd.parameter_category(), pd.parameter_number()) {
            (Some(16), Some(196)) => Field::Refc,
            (Some(1), Some(33)) => Field::Rain,
            (Some(1), Some(34)) => Field::Fzra,
            (Some(1), Some(35)) => Field::Ice,
            (Some(1), Some(36)) => Field::Snow,
            (Some(1), Some(39)) => Field::Pofp,
            _ => continue,
        };
        if field == Field::Refc {
            found_refc = true;
        }
        let latlons = sub.latlons().context("computing HRRR grid lat/lons")?;
        let decoder =
            grib::Grib2SubmessageDecoder::from(sub).context("building the HRRR GRIB decoder")?;
        let values = decoder.dispatch().context("decoding HRRR field values")?;
        for ((lat, lon), value) in latlons.zip(values) {
            points += 1;
            if !value.is_finite() || value < -90.0 {
                continue;
            }
            if lat < south || lat > north || lon < west || lon > east {
                continue;
            }
            let mx = merc_x(lon);
            let my = merc_y(lat);
            if mx < x0 || mx > x1 || my < y0 || my > y1 {
                continue;
            }
            let x = ((mx - x0) / (x1 - x0) * (width.saturating_sub(1) as f64)).round() as usize;
            let y = ((y1 - my) / (y1 - y0) * (height.saturating_sub(1) as f64)).round() as usize;
            if x >= width || y >= height {
                continue;
            }
            let i = y * width + x;
            match field {
                Field::Refc => {
                    // 0 is reserved for "no grid sample"; real values 1..255.
                    dbz[i] = ((value + 32.0).clamp(1.0, 255.0)).round() as u8;
                    inside += 1;
                }
                Field::Rain | Field::Snow | Field::Ice | Field::Fzra => {
                    if value >= 0.5 {
                        let t = match field {
                            Field::Rain => 1,
                            Field::Snow => 2,
                            Field::Ice => 3,
                            Field::Fzra => 4,
                            _ => 0,
                        };
                        if priority(t) > priority(ptype[i]) {
                            ptype[i] = t;
                        }
                    }
                }
                Field::Pofp => {
                    frozen_pct[i] = value.clamp(0.0, 100.0).round() as u8;
                }
            }
        }
    }
    if !found_refc {
        bail!("HRRR file has no REFC message");
    }
    if inside == 0 {
        bail!("HRRR REFC had no grid points inside the box");
    }
    fill_holes(&mut dbz, width, height, 3);
    // Categorical fields sometimes leave a pixel unset. Use the frozen
    // percentage as a winter fallback, then fill small gaps from neighbours.
    for i in 0..ptype.len() {
        if ptype[i] == 0 && dbz[i] != 0 && frozen_pct[i] >= 50 {
            ptype[i] = 2;
        }
    }
    fill_type_holes(&mut ptype, &dbz, width, height, 3);
    log::debug!(
        "Futurecast: f{:02} {}x{} from {points} points ({inside} inside box, {} typed)",
        info.hour,
        width,
        height,
        ptype.iter().filter(|t| **t != 0).count()
    );
    Ok(Frame {
        init,
        hour: info.hour,
        valid: info.valid,
        width,
        height,
        south,
        west,
        north,
        east,
        dbz,
        ptype,
    })
}

/// Fill the small gaps left when a 3 km source grid is splatted into a
/// higher-resolution Mercator texture. Clear areas stay transparent: only
/// holes with an echo within three pixels get a value.
fn fill_holes(dbz: &mut [u8], width: usize, height: usize, radius: i32) {
    let source = dbz.to_vec();
    for y in 0..height {
        for x in 0..width {
            let i = y * width + x;
            if source[i] != 0 {
                continue;
            }
            let mut best = 0u8;
            let mut best_d = i32::MAX;
            for dy in -radius..=radius {
                let ny = y as i32 + dy;
                if ny < 0 || ny >= height as i32 {
                    continue;
                }
                for dx in -radius..=radius {
                    let nx = x as i32 + dx;
                    if nx < 0 || nx >= width as i32 {
                        continue;
                    }
                    let v = source[ny as usize * width + nx as usize];
                    if v == 0 {
                        continue;
                    }
                    let d = dx * dx + dy * dy;
                    if d < best_d {
                        best_d = d;
                        best = v;
                    }
                }
            }
            if best != 0 {
                dbz[i] = best;
            }
        }
    }
}

/// Fill type gaps only where there is an echo, copying the nearest known
/// categorical type. A type-free S3 fallback stays all zeros (reflectivity
/// colours are used instead).
fn fill_type_holes(ptype: &mut [u8], dbz: &[u8], width: usize, height: usize, radius: i32) {
    let source = ptype.to_vec();
    for y in 0..height {
        for x in 0..width {
            let i = y * width + x;
            if source[i] != 0 || dbz[i] == 0 {
                continue;
            }
            let mut best = 0u8;
            let mut best_d = i32::MAX;
            for dy in -radius..=radius {
                let ny = y as i32 + dy;
                if ny < 0 || ny >= height as i32 {
                    continue;
                }
                for dx in -radius..=radius {
                    let nx = x as i32 + dx;
                    if nx < 0 || nx >= width as i32 {
                        continue;
                    }
                    let v = source[ny as usize * width + nx as usize];
                    if v == 0 {
                        continue;
                    }
                    let d = dx * dx + dy * dy;
                    if d < best_d {
                        best_d = d;
                        best = v;
                    }
                }
            }
            if best != 0 {
                ptype[i] = best;
            }
        }
    }
}

fn merc_x(lon: f64) -> f64 {
    6_378_137.0 * lon.to_radians()
}

fn merc_y(lat: f64) -> f64 {
    6_378_137.0
        * (std::f64::consts::PI / 4.0 + lat.to_radians() / 2.0)
            .tan()
            .ln()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forecast_box_is_the_whole_conus() {
        assert_eq!(forecast_box(39.0, -96.0), CONUS);
        assert_eq!(forecast_box(25.0, -125.0), CONUS);
        assert!(supported(39.0, -96.0));
        assert!(!supported(61.0, -149.0));
    }

    #[test]
    fn plan_starts_at_the_next_future_hour_and_caps_at_18() {
        let init = Utc.with_ymd_and_hms(2026, 10, 26, 5, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 10, 26, 6, 53, 0).unwrap();
        let plan = plan_for(init, now, false).unwrap();
        assert_eq!(plan.frames.first().unwrap().hour, 1);
        assert_eq!(plan.frames.first().unwrap().valid.hour(), 6);
        assert_eq!(plan.frames.last().unwrap().hour, 18);
        assert_eq!(plan.frames.len(), 18);
        // The plan is independent of wall-clock age: every run exposes its
        // whole window so the strip can walk the full model run.
        let old = Utc.with_ymd_and_hms(2026, 10, 25, 12, 0, 0).unwrap();
        let plan = plan_for(old, now, false).unwrap();
        assert_eq!(plan.frames.len(), 18);
        assert_eq!(plan.frames[0].hour, 1);
        // The extended horizon carries the complete 48-hour run.
        let plan = plan_for(init, now, true).unwrap();
        assert_eq!(plan.frames.len(), 48);
        assert_eq!(plan.frames.last().unwrap().hour, 48);
        assert!(plan.extended);
    }

    #[test]
    fn parses_the_refc_range_from_an_idx() {
        let idx = "1:0:d=2026102605:REFC:entire atmosphere:6 hour fcst:\n\
                   2:611317:d=2026102605:RETOP:cloud top:6 hour fcst:\n";
        assert_eq!(refc_byte_range(idx), Some((0, 611_316)));
        assert!(refc_byte_range("1:0:d=2026102605:RETOP:cloud top:6 hour fcst:\n").is_none());
    }

    #[test]
    fn frame_sampling_is_in_mercator_pixels() {
        let f = Frame {
            init: Utc::now(),
            hour: 3,
            valid: Utc::now(),
            width: 4,
            height: 3,
            south: 38.0,
            west: -98.0,
            north: 40.0,
            east: -96.0,
            dbz: vec![0, 30, 40, 0, 50, 0, 60, 0, 0, 70, 0, 80],
            ptype: vec![0; 12],
        };
        assert_eq!(f.dbz_at(39.0, -97.0), Some(f.dbz[1 * 4 + 2] as f32 - 32.0));
        assert_eq!(f.dbz_at(41.0, -97.0), None);
        assert_eq!(f.max_dbz(), Some(80.0 - 32.0));
    }

    /// Live plan (network): the extended cycle is only 00/06/12/18Z and must
    /// have its full f48 product.
    #[test]
    #[ignore = "needs network access to NOAA S3"]
    fn plans_a_complete_48_hour_run() {
        let src = FuturecastSource::new().expect("source");
        let bbox = forecast_box(39.05, -95.68);
        let plan = src
            .plan(Utc::now(), bbox, true)
            .expect("an extended run plan");
        eprintln!(
            "HRRR extended run {}: {} frames ({:?}..{:?})",
            plan.init,
            plan.frames.len(),
            plan.frames.first(),
            plan.frames.last()
        );
        assert!(plan.extended);
        assert_eq!(plan.frames.len(), 48);
        assert_eq!(plan.frames[0].hour, 1);
        assert_eq!(plan.frames[47].hour, 48);
        assert_eq!(plan.init.hour() % 6, 0);
    }

    /// Live fetch (network): the newest complete run, the next two frames,
    /// and a real radar-sized grid.
    #[test]
    #[ignore = "needs network access to NOAA NOMADS/S3"]
    fn fetches_a_recent_refc_frame() {
        let src = FuturecastSource::new().expect("source");
        let bbox = forecast_box(39.05, -95.68);
        let plan = src.plan(Utc::now(), bbox, false).expect("a run plan");
        eprintln!(
            "HRRR run {}: {} frames {:?}",
            plan.init,
            plan.frames.len(),
            plan.frames
        );
        for info in plan.frames.iter().take(2) {
            let frame = src.fetch(&plan, info, bbox).expect("a frame");
            eprintln!(
                "f{:02} {}x{} max {:?} dBZ, {} typed pixels",
                frame.hour,
                frame.width,
                frame.height,
                frame.max_dbz(),
                frame.ptype.iter().filter(|t| **t != 0).count()
            );
            assert!(frame.width > 300 && frame.height > 200);
            assert!(frame.dbz.iter().any(|q| *q != 0), "no grid sample");
            assert!(frame.max_dbz().unwrap_or(-99.0) >= 5.0);
        }
    }
}
