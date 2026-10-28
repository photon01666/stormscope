//! MRMS multi-radar mosaic: NOAA's Multi-Radar Multi-Sensor composite, read
//! from the public `noaa-mrms-pds` bucket and decoded locally.
//!
//! MRMS files are GRIB2 with **PNG packing** (data representation template
//! 5.41): section 7 *is* a 16-bit grayscale PNG of the grid, and the physical
//! value of sample `X` is `(R + X·2^E) / 10^D`, with `R`, `E`, `D` from the
//! data-representation section. The full CONUS grid is 7000 × 3500 (24.5 M
//! points); this decoder downsamples while unpacking, because the overlay is
//! a synoptic context layer, not a radar replacement.
//!
//! The grid-definition section is template 3.0. Its earth-shape block varies
//! in length between producers, so the decoder *locates* `Ni`/`Nj` by the
//! `Ni·Nj == numberOfPoints` invariant and reads the rest at fixed offsets
//! relative to them — with sanity checks, so a different producer fails
//! loudly instead of drawing garbage.

use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};

use crate::cache::DiskCache;

/// Bucket endpoint (anonymous HTTPS; the S3 REST API is public-read).
pub const BUCKET: &str = "https://noaa-mrms-pds.s3.amazonaws.com";

/// Which MRMS product the layer shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MrmsProduct {
    /// `MergedBaseReflectivity_00.50`: the multi-radar composite reflectivity
    /// (the classic "mosaic").
    Reflectivity,
    /// `PrecipRate_00.00`: the multi-sensor surface precipitation rate.
    PrecipRate,
    /// Multi-sensor QPE accumulations (millimetres).
    MultiSensorQpe01h,
    MultiSensorQpe03h,
    MultiSensorQpe06h,
    MultiSensorQpe24h,
    /// Radar-only QPE accumulations (millimetres).
    RadarOnlyQpe01h,
    RadarOnlyQpe03h,
    RadarOnlyQpe06h,
    RadarOnlyQpe24h,
    /// FLASH flash-flood guidance: rain needed to reach guidance.
    FlashFfg01h,
    FlashFfg03h,
    FlashFfg06h,
}

impl MrmsProduct {
    fn dir(self) -> &'static str {
        match self {
            MrmsProduct::Reflectivity => "CONUS/MergedBaseReflectivity_00.50",
            MrmsProduct::PrecipRate => "CONUS/PrecipRate_00.00",
            MrmsProduct::MultiSensorQpe01h => "CONUS/MultiSensor_QPE_01H_Pass2_00.00",
            MrmsProduct::MultiSensorQpe03h => "CONUS/MultiSensor_QPE_03H_Pass2_00.00",
            MrmsProduct::MultiSensorQpe06h => "CONUS/MultiSensor_QPE_06H_Pass2_00.00",
            MrmsProduct::MultiSensorQpe24h => "CONUS/MultiSensor_QPE_24H_Pass2_00.00",
            MrmsProduct::RadarOnlyQpe01h => "CONUS/RadarOnly_QPE_01H_00.00",
            MrmsProduct::RadarOnlyQpe03h => "CONUS/RadarOnly_QPE_03H_00.00",
            MrmsProduct::RadarOnlyQpe06h => "CONUS/RadarOnly_QPE_06H_00.00",
            MrmsProduct::RadarOnlyQpe24h => "CONUS/RadarOnly_QPE_24H_00.00",
            MrmsProduct::FlashFfg01h => "CONUS/FLASH_QPE_FFG01H_00.00",
            MrmsProduct::FlashFfg03h => "CONUS/FLASH_QPE_FFG03H_00.00",
            MrmsProduct::FlashFfg06h => "CONUS/FLASH_QPE_FFG06H_00.00",
        }
    }

    fn file_stem(self) -> &'static str {
        match self {
            MrmsProduct::Reflectivity => "MRMS_MergedBaseReflectivity_00.50",
            MrmsProduct::PrecipRate => "MRMS_PrecipRate_00.00",
            MrmsProduct::MultiSensorQpe01h => "MRMS_MultiSensor_QPE_01H_Pass2_00.00",
            MrmsProduct::MultiSensorQpe03h => "MRMS_MultiSensor_QPE_03H_Pass2_00.00",
            MrmsProduct::MultiSensorQpe06h => "MRMS_MultiSensor_QPE_06H_Pass2_00.00",
            MrmsProduct::MultiSensorQpe24h => "MRMS_MultiSensor_QPE_24H_Pass2_00.00",
            MrmsProduct::RadarOnlyQpe01h => "MRMS_RadarOnly_QPE_01H_00.00",
            MrmsProduct::RadarOnlyQpe03h => "MRMS_RadarOnly_QPE_03H_00.00",
            MrmsProduct::RadarOnlyQpe06h => "MRMS_RadarOnly_QPE_06H_00.00",
            MrmsProduct::RadarOnlyQpe24h => "MRMS_RadarOnly_QPE_24H_00.00",
            MrmsProduct::FlashFfg01h => "MRMS_FLASH_QPE_FFG01H_00.00",
            MrmsProduct::FlashFfg03h => "MRMS_FLASH_QPE_FFG03H_00.00",
            MrmsProduct::FlashFfg06h => "MRMS_FLASH_QPE_FFG06H_00.00",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MrmsProduct::Reflectivity => "MRMS reflectivity",
            MrmsProduct::PrecipRate => "MRMS precip rate",
            MrmsProduct::MultiSensorQpe01h => "QPE 1 h (multi-sensor)",
            MrmsProduct::MultiSensorQpe03h => "QPE 3 h (multi-sensor)",
            MrmsProduct::MultiSensorQpe06h => "QPE 6 h (multi-sensor)",
            MrmsProduct::MultiSensorQpe24h => "QPE 24 h (multi-sensor)",
            MrmsProduct::RadarOnlyQpe01h => "QPE 1 h (radar only)",
            MrmsProduct::RadarOnlyQpe03h => "QPE 3 h (radar only)",
            MrmsProduct::RadarOnlyQpe06h => "QPE 6 h (radar only)",
            MrmsProduct::RadarOnlyQpe24h => "QPE 24 h (radar only)",
            MrmsProduct::FlashFfg01h => "FFG 1 h (rain needed)",
            MrmsProduct::FlashFfg03h => "FFG 3 h (rain needed)",
            MrmsProduct::FlashFfg06h => "FFG 6 h (rain needed)",
        }
    }
    pub fn is_qpe(self) -> bool {
        matches!(
            self,
            MrmsProduct::MultiSensorQpe01h
                | MrmsProduct::MultiSensorQpe03h
                | MrmsProduct::MultiSensorQpe06h
                | MrmsProduct::MultiSensorQpe24h
                | MrmsProduct::RadarOnlyQpe01h
                | MrmsProduct::RadarOnlyQpe03h
                | MrmsProduct::RadarOnlyQpe06h
                | MrmsProduct::RadarOnlyQpe24h
                | MrmsProduct::FlashFfg01h
                | MrmsProduct::FlashFfg03h
                | MrmsProduct::FlashFfg06h
        )
    }

    pub fn is_ffg(self) -> bool {
        matches!(
            self,
            MrmsProduct::FlashFfg01h | MrmsProduct::FlashFfg03h | MrmsProduct::FlashFfg06h
        )
    }

    pub fn accumulation_hours(self) -> Option<u16> {
        Some(match self {
            MrmsProduct::MultiSensorQpe01h | MrmsProduct::RadarOnlyQpe01h => 1,
            MrmsProduct::MultiSensorQpe03h | MrmsProduct::RadarOnlyQpe03h => 3,
            MrmsProduct::MultiSensorQpe06h | MrmsProduct::RadarOnlyQpe06h => 6,
            MrmsProduct::MultiSensorQpe24h | MrmsProduct::RadarOnlyQpe24h => 24,
            MrmsProduct::FlashFfg01h => 1,
            MrmsProduct::FlashFfg03h => 3,
            MrmsProduct::FlashFfg06h => 6,
            _ => return None,
        })
    }
}

/// One decoded mosaic: a regular latitude/longitude grid, north-up.
#[derive(Debug)]
pub struct Mosaic {
    pub product: MrmsProduct,
    /// Valid time of the composite (from the file name).
    pub time: DateTime<Utc>,
    pub width: usize,
    pub height: usize,
    /// Cell edges (degrees; west/east normalised to −180..180).
    pub north: f64,
    pub west: f64,
    pub south: f64,
    pub east: f64,
    /// Row-major, north row first, west column first.
    pub values: Vec<f32>,
}

impl Mosaic {
    /// Nearest-cell value at a latitude/longitude, if the point is inside the
    /// decoded grid.
    pub fn value_at(&self, lat: f64, lon: f64) -> Option<f32> {
        if self.width == 0 || self.height == 0 {
            return None;
        }
        if !(self.south..=self.north).contains(&lat) || !(self.west..=self.east).contains(&lon) {
            return None;
        }
        let x = ((lon - self.west) / (self.east - self.west)
            * (self.width.saturating_sub(1) as f64))
            .round()
            .clamp(0.0, self.width.saturating_sub(1) as f64) as usize;
        let y = ((self.north - lat) / (self.north - self.south)
            * (self.height.saturating_sub(1) as f64))
            .round()
            .clamp(0.0, self.height.saturating_sub(1) as f64) as usize;
        let value = *self.values.get(y * self.width + x)?;
        value.is_finite().then_some(value)
    }
}

/// Fetch + cache driver for the mosaic.
pub struct MrmsSource {
    cache: DiskCache,
    client: reqwest::blocking::Client,
}

/// How many cached MRMS files to keep (each is a few MB, and only the newest
/// is useful).
const CACHE_FILES: usize = 4;

impl MrmsSource {
    pub fn new(cache_root: PathBuf) -> Self {
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!(
                "stormscope/",
                env!("CARGO_PKG_VERSION"),
                " (https://github.com/photon01666/stormscope)"
            ))
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self {
            cache: DiskCache::new(cache_root, 64 * 1024 * 1024),
            client,
        }
    }

    /// The newest mosaic available (this hour, else up to three hours back —
    /// the feed lags by a few minutes and can hiccup).
    ///
    /// `crop` is `(south, west, north, east)`: the overlay only ever shows a
    /// window around the radar, so decoding the whole CONUS grid at native
    /// resolution would be 98 MB of values for a ~6 MB texture. Cropping
    /// during the unpack keeps the mosaic at its true 1 km/px.
    pub fn latest(
        &self,
        product: MrmsProduct,
        step: usize,
        crop: Option<[f64; 4]>,
    ) -> Result<Mosaic> {
        let mut key = None;
        for back in 0..4 {
            let t = Utc::now() - chrono::Duration::hours(back);
            let prefix = format!(
                "{}/{}/{}_{}",
                product.dir(),
                t.format("%Y%m%d"),
                product.file_stem(),
                t.format("%Y%m%d-%H")
            );
            if let Some(k) = self.list_latest(&prefix)? {
                key = Some(k);
                break;
            }
        }
        let key = key.ok_or_else(|| anyhow!("no MRMS file listed in the last 4 hours"))?;
        let name = key.rsplit('/').next().unwrap_or(&key).to_string();
        let rel = format!("mrms/{name}");

        let gz = match self.cache.read(&rel)? {
            Some(bytes) => bytes,
            None => {
                let url = format!("{BUCKET}/{key}");
                let bytes = self
                    .client
                    .get(&url)
                    .send()
                    .and_then(|r| r.error_for_status())
                    .with_context(|| format!("downloading {url}"))?
                    .bytes()
                    .context("reading the MRMS body")?
                    .to_vec();
                self.cache.write(&rel, &bytes)?;
                self.prune();
                bytes
            }
        };
        let mut mosaic = decode(&gz, product, step, crop)?;
        mosaic.time = time_from_key(&key).unwrap_or_else(Utc::now);
        Ok(mosaic)
    }

    /// The newest key under `prefix` (S3 lists keys ascending; the last one
    /// is the newest). One request per hour tried.
    fn list_latest(&self, prefix: &str) -> Result<Option<String>> {
        let url = format!("{BUCKET}/?list-type=2&max-keys=1000&prefix={prefix}");
        let xml = self
            .client
            .get(&url)
            .send()
            .and_then(|r| r.error_for_status())
            .with_context(|| format!("listing {prefix}"))?
            .text()
            .context("reading the MRMS listing")?;
        let mut last = None;
        let mut rest = xml.as_str();
        while let Some(start) = rest.find("<Key>") {
            let after = &rest[start + 5..];
            let Some(end) = after.find("</Key>") else {
                break;
            };
            last = Some(after[..end].to_string());
            rest = &after[end..];
        }
        Ok(last)
    }

    /// Keep only the newest few cached files.
    fn prune(&self) {
        let Ok(dir) = std::fs::read_dir(self.cache.path_for("mrms/").unwrap_or_default()) else {
            return;
        };
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
        for entry in dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|x| x.to_str()) != Some("gz") {
                continue;
            }
            let Some(modified) = entry.metadata().ok().and_then(|m| m.modified().ok()) else {
                continue;
            };
            files.push((modified, path));
        }
        files.sort_by_key(|(t, _)| *t);
        let excess = files.len().saturating_sub(CACHE_FILES);
        for (_, path) in files.into_iter().take(excess) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Parse `MRMS_...._YYYYMMDD-HHMMSS.grib2.gz` into its valid time.
pub fn time_from_key(key: &str) -> Option<DateTime<Utc>> {
    let name = key.rsplit('/').next()?;
    // Use the last `_2` so the `24H` product suffix is not mistaken for
    // the start of the timestamp.
    let idx = name.rfind("_2")? + 1;
    let stamp = name.get(idx..idx + 15)?; // YYYYMMDD-HHMMSS
    let naive = NaiveDateTime::parse_from_str(stamp, "%Y%m%d-%H%M%S").ok()?;
    Some(Utc.from_utc_datetime(&naive))
}

/// Gunzip and decode one MRMS file.
pub fn decode(
    gz: &[u8],
    product: MrmsProduct,
    step: usize,
    crop: Option<[f64; 4]>,
) -> Result<Mosaic> {
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_end(&mut raw)
        .context("gunzipping the MRMS file")?;
    decode_grib2(&raw, product, step, crop)
}

/// Decode raw GRIB2 bytes (PNG-packed, template 3.0).
pub fn decode_grib2(
    raw: &[u8],
    product: MrmsProduct,
    step: usize,
    crop: Option<[f64; 4]>,
) -> Result<Mosaic> {
    if raw.len() < 16 || &raw[..4] != b"GRIB" || raw[7] != 2 {
        bail!("not a GRIB2 stream");
    }
    let step = step.max(1);
    let mut sec: [Option<&[u8]>; 8] = [None; 8];
    let mut pos = 16usize;
    while pos + 5 <= raw.len() {
        let len = u32::from_be_bytes([raw[pos], raw[pos + 1], raw[pos + 2], raw[pos + 3]]) as usize;
        if len < 5 || pos + len > raw.len() {
            bail!("bad GRIB2 section length at {pos}");
        }
        let num = raw[pos + 4] as usize;
        if num < sec.len() {
            sec[num].get_or_insert(&raw[pos + 5..pos + len]);
        }
        pos += len;
    }
    let (Some(grid), Some(drs), Some(data)) = (sec[3], sec[5], sec[7]) else {
        bail!("MRMS GRIB2 is missing a grid/packing/data section");
    };

    // --- grid -----------------------------------------------------------
    let npts = u32::from_be_bytes([grid[1], grid[2], grid[3], grid[4]]) as u64;
    let mut found = None;
    for k in 6..grid.len().saturating_sub(8) {
        let ni = u32::from_be_bytes([grid[k], grid[k + 1], grid[k + 2], grid[k + 3]]) as u64;
        let nj = u32::from_be_bytes([grid[k + 4], grid[k + 5], grid[k + 6], grid[k + 7]]) as u64;
        if ni >= 2 && nj >= 2 && ni * nj == npts {
            found = Some((k, ni as usize, nj as usize));
            break;
        }
    }
    let Some((k, ni, nj)) = found else {
        bail!("MRMS grid definition not recognised ({npts} points)");
    };
    // Offsets relative to Ni, measured on MRMS' template-3.0 files and
    // checked below.
    let i32_at = |o: usize| i32::from_be_bytes([grid[o], grid[o + 1], grid[o + 2], grid[o + 3]]);
    let u32_at = |o: usize| u32::from_be_bytes([grid[o], grid[o + 1], grid[o + 2], grid[o + 3]]);
    let (north, south) = (i32_at(k + 16) as f64 / 1e6, i32_at(k + 25) as f64 / 1e6);
    // MRMS stores longitudes 0..360 (230.005 = 130°W); normalise before the
    // crop maths so a −104° box can intersect the grid.
    let west = normalise_lon(i32_at(k + 20) as f64 / 1e6);
    let east = normalise_lon(i32_at(k + 29) as f64 / 1e6);
    let (di, dj) = (u32_at(k + 33) as f64 / 1e6, u32_at(k + 37) as f64 / 1e6);
    if !(-90.0..=90.0).contains(&north)
        || !(-90.0..=90.0).contains(&south)
        || di <= 0.0
        || dj <= 0.0
        || east <= west
    {
        bail!("MRMS grid bounds are not sane");
    }

    // --- packing (template 5.41 = PNG) -----------------------------------
    let nvals = u32::from_be_bytes([drs[0], drs[1], drs[2], drs[3]]) as usize;
    let template = u16::from_be_bytes([drs[4], drs[5]]);
    if template != 41 {
        bail!("unsupported MRMS data representation template 5.{template}");
    }
    let reference = f32::from_be_bytes([drs[6], drs[7], drs[8], drs[9]]);
    let binary_scale = i16::from_be_bytes([drs[10], drs[11]]);
    let decimal_scale = i16::from_be_bytes([drs[12], drs[13]]);
    if nvals != ni * nj {
        bail!("packing says {nvals} values, grid says {}", ni * nj);
    }

    // --- PNG payload ------------------------------------------------------
    let image = image::load_from_memory_with_format(data, image::ImageFormat::Png)
        .context("decoding the MRMS PNG payload")?;
    let (w, h) = (image.width() as usize, image.height() as usize);
    if w != ni || h != nj {
        bail!("PNG {w}x{h} does not match the grid {ni}x{nj}");
    }
    // Most MRMS products use 16-bit grayscale PNG. The FLASH guidance
    // products use an RGB8 PNG carrying a 24-bit sample in R,G,B order.
    let samples: Vec<u32> = match image {
        image::DynamicImage::ImageLuma16(buf) => {
            buf.into_raw().into_iter().map(u32::from).collect()
        }
        image::DynamicImage::ImageLuma8(buf) => buf.into_raw().into_iter().map(u32::from).collect(),
        image::DynamicImage::ImageRgb8(buf) => {
            let raw = buf.into_raw();
            raw.chunks_exact(3)
                .map(|p| (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2]))
                .collect()
        }
        _ => bail!("unexpected MRMS PNG colour type"),
    };

    let binary = 2f32.powi(i32::from(binary_scale));
    let decimal = 10f32.powi(i32::from(decimal_scale));

    // Output window: the crop in grid coordinates, or the whole grid.
    let [south_req, west_req, north_req, east_req] = crop.unwrap_or([south, west, north, east]);
    // The `+ 1e-6` keeps an exact pixel-boundary crop (e.g. 39.03° on a
    // 0.01° grid) from falling a pixel short to floating-point noise.
    let col_of = |lon: f64| {
        ((((lon - west) / di + 1e-6).floor()) as isize).clamp(0, ni as isize - 1) as usize
    };
    let row_of = |lat: f64| {
        ((((north - lat) / dj + 1e-6).floor()) as isize).clamp(0, nj as isize - 1) as usize
    };
    let (col0, col1) = (col_of(west_req), col_of(east_req));
    let (row0, row1) = (row_of(north_req), row_of(south_req));
    let (out_w, out_h) = ((col1 - col0) / step + 1, (row1 - row0) / step + 1);
    let mut values = Vec::with_capacity(out_w * out_h);
    for y in (row0..=row1).step_by(step) {
        let row = &samples[y * ni..(y + 1) * ni];
        for x in (col0..=col1).step_by(step) {
            values.push((reference + row[x] as f32 * binary) / decimal);
        }
    }
    Ok(Mosaic {
        product,
        time: Utc::now(),
        width: out_w,
        height: out_h,
        // The texture covers the pixels actually kept.
        north: north - row0 as f64 * dj,
        west: normalise_lon(west + col0 as f64 * di),
        south: north - row1 as f64 * dj,
        east: normalise_lon(west + col1 as f64 * di),
        values,
    })
}

fn normalise_lon(lon: f64) -> f64 {
    if lon > 180.0 { lon - 360.0 } else { lon }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;
    use image::codecs::png::PngEncoder;

    /// Build a synthetic PNG-packed GRIB2 exactly like MRMS': template 3.0
    /// grid, template 5.41 packing, section 7 = PNG.
    fn synthetic_grib2(ni: usize, nj: usize, samples: &[u16], r: f32, e: i16, d: i16) -> Vec<u8> {
        // Grid section body (67 bytes, the MRMS layout): 0.01° per pixel
        // starting at 39.04°N, 96.24°W.
        let mut g = vec![0u8; 67];
        g[0] = 0; // source
        g[1..5].copy_from_slice(&((ni * nj) as u32).to_be_bytes());
        g[5..7].copy_from_slice(&0u16.to_be_bytes()); // template 3.0
        g[7] = 0; // spherical earth
        // Ni/Nj at 25/29.
        g[25..29].copy_from_slice(&(ni as u32).to_be_bytes());
        g[29..33].copy_from_slice(&(nj as u32).to_be_bytes());
        // La1, Lo1, flags, La2, Lo2, Di, Dj, scan mode.
        g[41..45].copy_from_slice(&39_040_000i32.to_be_bytes());
        g[45..49].copy_from_slice(&(-96_240_000i32).to_be_bytes());
        g[49] = 0;
        g[50..54].copy_from_slice(&(39_040_000 - nj as i32 * 10_000).to_be_bytes());
        g[54..58].copy_from_slice(&(-96_240_000 + ni as i32 * 10_000).to_be_bytes());
        g[58..62].copy_from_slice(&10_000u32.to_be_bytes());
        g[62..66].copy_from_slice(&10_000u32.to_be_bytes());
        g[66] = 0x40;
        let mut out = Vec::new();
        out.extend_from_slice(b"GRIB");
        out.extend_from_slice(&[0, 0, 0, 2]); // discipline 0, edition 2
        out.extend_from_slice(&0u64.to_be_bytes()); // total length patched later
        let mut section = |num: u8, body: &[u8]| {
            out.extend_from_slice(&((body.len() + 5) as u32).to_be_bytes());
            out.push(num);
            out.extend_from_slice(body);
        };
        section(3, &g);
        // Packing section: nvals, template 41, R, E, D, nbits, original type.
        let mut p = Vec::new();
        p.extend_from_slice(&((ni * nj) as u32).to_be_bytes());
        p.extend_from_slice(&41u16.to_be_bytes());
        p.extend_from_slice(&r.to_be_bytes());
        p.extend_from_slice(&e.to_be_bytes());
        p.extend_from_slice(&d.to_be_bytes());
        p.push(16);
        p.push(0);
        section(5, &p);
        // PNG payload.
        // `ImageEncoder::write_image` expects native-endian 16-bit samples
        // (the encoder does the network-order conversion PNG requires).
        let mut raw = Vec::with_capacity(ni * nj * 2);
        for s in samples {
            raw.extend_from_slice(&s.to_ne_bytes());
        }
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(&raw, ni as u32, nj as u32, image::ExtendedColorType::L16)
            .expect("encode png");
        section(7, &png);
        out.extend_from_slice(b"7777");
        let total = out.len() as u64;
        out[8..16].copy_from_slice(&total.to_be_bytes());
        out
    }

    #[test]
    fn decodes_png_packed_grib2_and_downsamples() {
        let (ni, nj) = (8usize, 4usize);
        let samples: Vec<u16> = (0..ni * nj).map(|i| 10_000 + i as u16).collect();
        // value = (R + X) / 10 → R=-9990, D=1, E=0.
        let raw = synthetic_grib2(ni, nj, &samples, -9990.0, 0, 1);
        let m = decode_grib2(&raw, MrmsProduct::Reflectivity, 2, None).expect("decoded");
        assert_eq!((m.width, m.height), (4, 2));
        assert!((m.north - 39.04).abs() < 1e-6, "north {}", m.north);
        assert!((m.west + 96.24).abs() < 1e-6, "west {}", m.west);
        assert!((m.south - 39.01).abs() < 1e-6, "south {}", m.south);
        assert!((m.east + 96.17).abs() < 1e-6, "east {}", m.east);
        // Row 0, col 0 is sample 0 → (10_000 - 9990)/10 = 1.0 dBZ; columns
        // stride by 2 samples (4 columns) and rows by 16 (2 rows).
        assert!((m.values[0] - 1.0).abs() < 1e-4, "{}", m.values[0]);
        assert!((m.values[1] - 1.2).abs() < 1e-4, "{}", m.values[1]);
        assert!((m.values[3] - 1.6).abs() < 1e-4, "{}", m.values[3]);
        // The second output row starts at sample 16.
        assert!((m.values[4] - 2.6).abs() < 1e-4, "{}", m.values[4]);
        // A crop keeps the grid's native resolution and its own bounds.
        let raw = synthetic_grib2(ni, nj, &samples, -9990.0, 0, 1);
        let crop = Some([39.01, -96.22, 39.03, -96.18]);
        let m = decode_grib2(&raw, MrmsProduct::Reflectivity, 1, crop).expect("cropped");
        assert!((m.north - 39.03).abs() < 1e-6, "north {}", m.north);
        assert!((m.south - 39.01).abs() < 1e-6, "south {}", m.south);
        assert!((m.west + 96.22).abs() < 1e-6, "west {}", m.west);
        assert!((m.east + 96.18).abs() < 1e-6, "east {}", m.east);
        // Columns 2..=6 and rows 1..=3 of the little grid.
        assert_eq!((m.width, m.height), (5, 3));

        // Scaling: E=-1 halves the sample's contribution.
        let raw = synthetic_grib2(2, 2, &[100, 200, 300, 400], 0.0, -1, 1);
        let m = decode_grib2(&raw, MrmsProduct::PrecipRate, 1, None).expect("decoded");
        assert_eq!(m.values, vec![5.0, 10.0, 15.0, 20.0]);
    }

    #[test]
    fn rejects_foreign_grib_and_unknown_packing() {
        assert!(decode_grib2(b"not grib at all", MrmsProduct::Reflectivity, 1, None).is_err());
        let raw = synthetic_grib2(2, 2, &[1, 2, 3, 4], 0.0, 0, 1);
        // Corrupt the packing template (41 → 40).
        let mut bad = raw.clone();
        let pos = bad.windows(2).position(|w| w == [0x00, 0x29]).unwrap();
        bad[pos + 1] = 40;
        let err = decode_grib2(&bad, MrmsProduct::Reflectivity, 1, None).unwrap_err();
        assert!(format!("{err}").contains("template"), "{err}");
    }

    /// The real path: list the newest file, download it, decode it. Ignored
    /// by default (network), run with `--ignored --nocapture`.
    #[test]
    #[ignore = "needs network access to noaa-mrms-pds"]
    fn fetches_and_decodes_the_latest_mosaic() {
        let src = MrmsSource::new(std::env::temp_dir().join("stormscope-mrms-test"));
        // The app's real paths: the CONUS-wide image and the sharp window.
        let m = src
            .latest(MrmsProduct::Reflectivity, 3, None)
            .expect("a mosaic");
        let crop = crate::local_overlay_box(39.0, -96.2);
        let near = src
            .latest(MrmsProduct::Reflectivity, 1, Some(crop))
            .expect("the radar window");
        assert!((near.south - crop[0]).abs() < 0.05, "south {}", near.south);
        assert!((near.north - crop[2]).abs() < 0.05, "north {}", near.north);
        assert!((near.west - crop[1]).abs() < 0.05, "west {}", near.west);
        assert!((near.east - crop[3]).abs() < 0.05, "east {}", near.east);
        assert!(
            near.width > 1400 && near.width < 1800,
            "width {}",
            near.width
        );
        eprintln!(
            "{}: {}x{} · {:.3}..{:.3}N {:.3}..{:.3}E · {}",
            m.product.label(),
            m.width,
            m.height,
            m.south,
            m.north,
            m.west,
            m.east,
            m.time
        );
        let finite = m.values.iter().filter(|v| v.is_finite()).count();
        let max = m.values.iter().copied().fold(f32::MIN, f32::max);
        eprintln!("finite {finite}/{} max {max:.1} dBZ", m.width * m.height);
        assert!(m.width > 500 && m.height > 300, "downsampled grid");
        assert!(max > 5.0 && max < 100.0, "reflectivity range: {max}");
    }

    #[test]
    fn file_names_carry_the_valid_time() {
        let t = time_from_key(
            "CONUS/MergedBaseReflectivity_00.50/20261020/MRMS_MergedBaseReflectivity_00.50_20261020-032219.grib2.gz",
        )
        .expect("a time");
        assert_eq!(
            t.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-10-20 03:22:19"
        );
        assert!(time_from_key("garbage").is_none());
    }
}
