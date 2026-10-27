//! Small HRRR model-field subsets for the map: QPF, CAPE and 0–6 km shear.
//!
//! NOMADS' `filter_hrrr_2d.pl` returns only the requested variable over a
//! local box. That is a few tens of KB, which keeps the layer practical on a
//! phone; the full HRRR GRIB file is ~130 MB.

use std::io::Cursor;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Timelike, Utc};
use grib::LatLons;

use crate::futurecast::{CONUS, FuturecastSource};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HrrrFieldKind {
    Qpf,
    Cape,
    Shear,
}

impl HrrrFieldKind {
    pub fn label(self) -> &'static str {
        match self {
            HrrrFieldKind::Qpf => "QPF (accumulated rain)",
            HrrrFieldKind::Cape => "CAPE",
            HrrrFieldKind::Shear => "0–6 km shear",
        }
    }
    pub fn unit(self) -> &'static str {
        match self {
            HrrrFieldKind::Qpf => "in",
            HrrrFieldKind::Cape => "J/kg",
            HrrrFieldKind::Shear => "m/s",
        }
    }
}

#[derive(Clone, Debug)]
pub struct HrrrField {
    pub kind: HrrrFieldKind,
    pub init: DateTime<Utc>,
    pub hour: u16,
    pub valid: DateTime<Utc>,
    pub width: usize,
    pub height: usize,
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
    pub values: Vec<f32>,
}

pub struct HrrrFieldSource {
    client: reqwest::blocking::Client,
}

impl HrrrFieldSource {
    pub fn new() -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!(
                "stormscope/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com/photon01666/stormscope)"
            ))
            .timeout(std::time::Duration::from_secs(60))
            .connect_timeout(std::time::Duration::from_secs(12))
            .build()?;
        Ok(Self { client })
    }

    /// Latest complete HRRR run, then one field for `hour` over the local box.
    pub fn fetch_latest(
        &self,
        kind: HrrrFieldKind,
        hour: u16,
        bbox: [f64; 4],
    ) -> Result<HrrrField> {
        let plan = FuturecastSource::new()?.plan(Utc::now(), CONUS, false)?;
        self.fetch(kind, plan.init, hour, bbox)
    }

    pub fn fetch(
        &self,
        kind: HrrrFieldKind,
        init: DateTime<Utc>,
        hour: u16,
        bbox: [f64; 4],
    ) -> Result<HrrrField> {
        let [south, west, north, east] = bbox;
        let mut params = vec![
            (
                "dir".to_string(),
                format!("/hrrr.{}/conus", init.format("%Y%m%d")),
            ),
            (
                "file".to_string(),
                format!("hrrr.t{:02}z.wrfsfcf{hour:02}.grib2", init.hour()),
            ),
        ];
        for (k, v) in match kind {
            HrrrFieldKind::Qpf => vec![("var_APCP", "on"), ("lev_surface", "on")],
            HrrrFieldKind::Cape => vec![("var_CAPE", "on"), ("lev_surface", "on")],
            HrrrFieldKind::Shear => vec![
                ("var_VUCSH", "on"),
                ("var_VVCSH", "on"),
                ("lev_0-6000_m_above_ground", "on"),
            ],
        } {
            params.push((k.to_string(), v.to_string()));
        }
        for (k, v) in [
            ("subregion", ""),
            ("leftlon", &format!("{west:.3}")),
            ("rightlon", &format!("{east:.3}")),
            ("toplat", &format!("{north:.3}")),
            ("bottomlat", &format!("{south:.3}")),
        ] {
            params.push((k.to_string(), v.to_string()));
        }
        let query: Vec<String> = params
            .into_iter()
            .map(|(k, v)| format!("{}={}", k, urlencode(&v)))
            .collect();
        let url = format!(
            "https://nomads.ncep.noaa.gov/cgi-bin/filter_hrrr_2d.pl?{}",
            query.join("&")
        );
        let bytes = self
            .client
            .get(&url)
            .send()
            .and_then(|r| r.error_for_status())
            .with_context(|| format!("GET HRRR field {kind:?} f{hour:02}"))?
            .bytes()
            .context("reading HRRR field")?;
        if !bytes.starts_with(b"GRIB") {
            bail!("NOMADS returned {} non-GRIB bytes", bytes.len());
        }
        decode_field(&bytes, kind, init, hour, bbox)
    }
}

fn decode_field(
    bytes: &[u8],
    kind: HrrrFieldKind,
    init: DateTime<Utc>,
    hour: u16,
    bbox: [f64; 4],
) -> Result<HrrrField> {
    let [south, west, north, east] = bbox;
    let width = 640usize;
    let (mx0, mx1) = (merc_x(west), merc_x(east));
    let (my0, my1) = (merc_y(south), merc_y(north));
    if mx1 <= mx0 || my1 <= my0 {
        bail!("invalid HRRR field box");
    }
    let height = ((width as f64) * (my1 - my0) / (mx1 - mx0))
        .round()
        .max(2.0) as usize;
    let mut values = vec![f32::NAN; width * height];
    let mut u = vec![f32::NAN; width * height];
    let mut v = vec![f32::NAN; width * height];
    let grib2 = grib::from_reader(Cursor::new(bytes)).context("parsing HRRR field GRIB2")?;
    let mut found = false;
    for ((_mi, _si), sub) in grib2.iter() {
        let pd = sub.prod_def();
        let field = match (pd.parameter_category(), pd.parameter_number()) {
            (Some(1), Some(8)) => 1,  // APCP
            (Some(7), Some(6)) => 2,  // CAPE
            (Some(2), Some(15)) => 3, // VUCSH
            (Some(2), Some(16)) => 4, // VVCSH
            _ => continue,
        };
        let latlons = sub.latlons().context("computing HRRR field lat/lons")?;
        let decoder =
            grib::Grib2SubmessageDecoder::from(sub).context("building HRRR field decoder")?;
        let vals = decoder.dispatch().context("decoding HRRR field values")?;
        for ((lat, lon), value) in latlons.zip(vals) {
            if !value.is_finite() || value < -100.0 {
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
            let x = ((mx - mx0) / (mx1 - mx0) * (width.saturating_sub(1) as f64)).round() as usize;
            let y = ((my1 - my) / (my1 - my0) * (height.saturating_sub(1) as f64)).round() as usize;
            let idx = y.min(height - 1) * width + x.min(width - 1);
            match field {
                1 if kind == HrrrFieldKind::Qpf => {
                    values[idx] = value;
                    found = true;
                }
                2 if kind == HrrrFieldKind::Cape => {
                    values[idx] = value;
                    found = true;
                }
                3 if kind == HrrrFieldKind::Shear => {
                    u[idx] = value;
                    found = true;
                }
                4 if kind == HrrrFieldKind::Shear => {
                    v[idx] = value;
                    found = true;
                }
                _ => {}
            }
        }
    }
    if !found {
        bail!("HRRR field {kind:?} was missing from the GRIB subset");
    }
    if kind == HrrrFieldKind::Shear {
        for (i, out) in values.iter_mut().enumerate() {
            if u[i].is_finite() && v[i].is_finite() {
                *out = (u[i] * u[i] + v[i] * v[i]).sqrt();
            }
        }
    }
    Ok(HrrrField {
        kind,
        init,
        hour,
        valid: init + Duration::hours(i64::from(hour)),
        width,
        height,
        south,
        west,
        north,
        east,
        values,
    })
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
fn urlencode(s: &str) -> String {
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('&', "%26")
        .replace('=', "%3D")
        .replace('/', "%2F")
        .replace(':', "%3A")
}
