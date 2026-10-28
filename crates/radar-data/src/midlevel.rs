//! HRRR 500 mb wind and height fields for the mid-level wind overlay.
//!
//! NOMADS returns a small GRIB subset for U/V/height at 500 mb. We decode it
//! into a Mercator grid so the map can draw a speed wash, height contours and
//! wind barbs without another projection step.

use std::io::Cursor;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Timelike, Utc};
use grib::LatLons;

use crate::futurecast::{CONUS, FuturecastSource};

/// Grid width; height follows the CONUS Mercator aspect.
pub const WIDTH: usize = 640;
/// Two-hourly steps through the 18-hour HRRR window.
pub const HOURS: &[u16] = &[0, 2, 4, 6, 8, 10, 12, 14, 16, 18];

#[derive(Clone, Debug)]
pub struct MidLevelFrame {
    pub init: DateTime<Utc>,
    pub hour: u16,
    pub valid: DateTime<Utc>,
    pub width: usize,
    pub height: usize,
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
    /// Row-major, north row first, west column first.
    pub u_ms: Vec<f32>,
    pub v_ms: Vec<f32>,
    pub speed_kt: Vec<f32>,
    pub height_m: Vec<f32>,
}

#[derive(Clone, Debug)]
pub struct MidLevelRun {
    pub init: DateTime<Utc>,
    pub frames: Vec<MidLevelFrame>,
}

pub struct MidLevelSource {
    client: reqwest::blocking::Client,
}

impl MidLevelSource {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::blocking::Client::builder()
                .user_agent(concat!(
                    "stormscope/",
                    env!("CARGO_PKG_VERSION"),
                    " (https://github.com/photon01666/stormscope)"
                ))
                .timeout(std::time::Duration::from_secs(60))
                .connect_timeout(std::time::Duration::from_secs(12))
                .build()?,
        })
    }

    pub fn latest_run(&self) -> Result<MidLevelRun> {
        let plan = FuturecastSource::new()?.plan(Utc::now(), CONUS, false)?;
        let mut frames = Vec::with_capacity(HOURS.len());
        for &hour in HOURS {
            frames.push(self.fetch_frame(plan.init, hour)?);
        }
        Ok(MidLevelRun {
            init: plan.init,
            frames,
        })
    }

    fn fetch_frame(&self, init: DateTime<Utc>, hour: u16) -> Result<MidLevelFrame> {
        let mut params = vec![
            (
                "dir".to_string(),
                format!("/hrrr.{}/conus", init.format("%Y%m%d")),
            ),
            (
                "file".to_string(),
                format!("hrrr.t{:02}z.wrfsfcf{hour:02}.grib2", init.hour()),
            ),
            ("var_UGRD".to_string(), "on".to_string()),
            ("var_VGRD".to_string(), "on".to_string()),
            ("var_HGT".to_string(), "on".to_string()),
            ("lev_500_mb".to_string(), "on".to_string()),
            ("subregion".to_string(), String::new()),
            ("leftlon".to_string(), format!("{:.3}", CONUS[1])),
            ("rightlon".to_string(), format!("{:.3}", CONUS[3])),
            ("toplat".to_string(), format!("{:.3}", CONUS[2])),
            ("bottomlat".to_string(), format!("{:.3}", CONUS[0])),
        ];
        params.sort_by(|a, b| a.0.cmp(&b.0));
        let query = params
            .into_iter()
            .map(|(k, v)| format!("{k}={}", urlencode(&v)))
            .collect::<Vec<_>>()
            .join("&");
        let url = format!("https://nomads.ncep.noaa.gov/cgi-bin/filter_hrrr_2d.pl?{query}");
        let bytes = self
            .client
            .get(&url)
            .send()
            .and_then(|r| r.error_for_status())
            .with_context(|| format!("GET HRRR 500 mb f{hour:02}"))?
            .bytes()
            .context("reading HRRR 500 mb field")?;
        if !bytes.starts_with(b"GRIB") {
            bail!("NOMADS returned {} non-GRIB bytes", bytes.len());
        }
        decode_frame(&bytes, init, hour)
    }
}

fn decode_frame(bytes: &[u8], init: DateTime<Utc>, hour: u16) -> Result<MidLevelFrame> {
    let [south, west, north, east] = CONUS;
    let (mx0, mx1) = (merc_x(west), merc_x(east));
    let (my0, my1) = (merc_y(south), merc_y(north));
    let height = ((WIDTH as f64) * (my1 - my0) / (mx1 - mx0))
        .round()
        .max(2.0) as usize;
    let mut u = vec![f32::NAN; WIDTH * height];
    let mut v = vec![f32::NAN; WIDTH * height];
    let mut hgt = vec![f32::NAN; WIDTH * height];
    let grib2 = grib::from_reader(Cursor::new(bytes)).context("parsing HRRR 500 mb GRIB2")?;
    let mut found = [false; 3];
    for ((_mi, _si), sub) in grib2.iter() {
        let pd = sub.prod_def();
        let (Some(cat), Some(num)) = (pd.parameter_category(), pd.parameter_number()) else {
            continue;
        };
        let Some((fs, _)) = pd.fixed_surfaces() else {
            continue;
        };
        if fs.surface_type != 100 || (fs.value() - 50_000.0).abs() > 1.0 {
            continue;
        }
        let slot = match (cat, num) {
            (2, 2) => 0,
            (2, 3) => 1,
            (3, 5) => 2,
            _ => continue,
        };
        let latlons = sub.latlons().context("HRRR 500 mb lat/lons")?;
        let decoder =
            grib::Grib2SubmessageDecoder::from(sub).context("building HRRR 500 mb decoder")?;
        let values = decoder.dispatch().context("decoding HRRR 500 mb values")?;
        for ((lat, lon), value) in latlons.zip(values) {
            if !lat.is_finite() || !lon.is_finite() || !value.is_finite() {
                continue;
            }
            if lat < south || lat > north || lon < west || lon > east {
                continue;
            }
            let mx = merc_x(lon);
            let my = merc_y(lat);
            if mx < mx0 || mx > mx1 || my < my0 || my > my1 {
                continue;
            }
            let x = ((mx - mx0) / (mx1 - mx0) * (WIDTH.saturating_sub(1) as f64)).round() as usize;
            let y = ((my1 - my) / (my1 - my0) * (height.saturating_sub(1) as f64)).round() as usize;
            let idx = y.min(height - 1) * WIDTH + x.min(WIDTH - 1);
            match slot {
                0 => u[idx] = value,
                1 => v[idx] = value,
                2 => hgt[idx] = value,
                _ => {}
            }
            found[slot] = true;
        }
    }
    if found.iter().any(|v| !*v) {
        bail!("HRRR 500 mb subset was missing U, V or height");
    }
    let mut speed = vec![f32::NAN; u.len()];
    for i in 0..u.len() {
        if u[i].is_finite() && v[i].is_finite() {
            speed[i] = (u[i] * u[i] + v[i] * v[i]).sqrt() * 1.943_844;
        }
    }
    Ok(MidLevelFrame {
        init,
        hour,
        valid: init + Duration::hours(i64::from(hour)),
        width: WIDTH,
        height,
        south,
        west,
        north,
        east,
        u_ms: u,
        v_ms: v,
        speed_kt: speed,
        height_m: hgt,
    })
}

/// Map a fractional grid coordinate back to lon/lat.
pub fn grid_lonlat(frame: &MidLevelFrame, gx: f32, gy: f32) -> (f64, f64) {
    let mx0 = merc_x(frame.west);
    let mx1 = merc_x(frame.east);
    let my0 = merc_y(frame.south);
    let my1 = merc_y(frame.north);
    let x = mx0 + f64::from(gx) / (frame.width.saturating_sub(1) as f64).max(1.0) * (mx1 - mx0);
    let y = my1 - f64::from(gy) / (frame.height.saturating_sub(1) as f64).max(1.0) * (my1 - my0);
    (inv_merc_x(x), inv_merc_y(y))
}

/// Wind speed colour ramp in knots, tuned to the 20–80+ kt broadcast look.
pub fn wind_rgba(kt: f32) -> [u8; 4] {
    if !kt.is_finite() || kt < 10.0 {
        return [0, 0, 0, 0];
    }
    const STOPS: &[(f32, [u8; 4])] = &[
        (10.0, [20, 55, 150, 0]),
        (20.0, [20, 80, 210, 80]),
        (30.0, [40, 170, 230, 150]),
        (40.0, [80, 230, 210, 185]),
        (50.0, [140, 220, 120, 205]),
        (60.0, [250, 220, 70, 220]),
        (70.0, [255, 130, 80, 235]),
        (80.0, [220, 60, 220, 245]),
        (100.0, [255, 240, 255, 255]),
    ];
    if kt <= STOPS[0].0 {
        return STOPS[0].1;
    }
    for pair in STOPS.windows(2) {
        let (a, ca) = pair[0];
        let (b, cb) = pair[1];
        if kt <= b {
            let t = ((kt - a) / (b - a)).clamp(0.0, 1.0);
            let mut out = [0u8; 4];
            for i in 0..4 {
                out[i] = (ca[i] as f32 + (cb[i] as f32 - ca[i] as f32) * t)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
            return out;
        }
    }
    STOPS.last().unwrap().1
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
fn inv_merc_x(x: f64) -> f64 {
    (x / 6_378_137.0).to_degrees()
}
fn inv_merc_y(y: f64) -> f64 {
    (2.0 * (y / 6_378_137.0).exp().atan() - std::f64::consts::FRAC_PI_2).to_degrees()
}
fn urlencode(s: &str) -> String {
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('&', "%26")
        .replace('=', "%3D")
        .replace('/', "%2F")
        .replace(':', "%3A")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramp_is_transparent_below_ten_knots() {
        assert_eq!(wind_rgba(0.0)[3], 0);
        assert!(wind_rgba(50.0)[3] > 0);
    }

    #[test]
    fn mercator_roundtrip() {
        let lat = 35.0;
        let lon = -97.0;
        assert!((inv_merc_x(merc_x(lon)) - lon).abs() < 1e-9);
        assert!((inv_merc_y(merc_y(lat)) - lat).abs() < 1e-9);
    }
}
