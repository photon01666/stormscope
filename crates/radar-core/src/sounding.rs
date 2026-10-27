//! Point soundings and the thermodynamic parameters derived from them.
//!
//! The first implementation is deliberately approximate but physically
//! grounded: pseudoadiabatic parcel lifting with a 6.5 K/km moist adiabat
//! above the LCL, trapezoidal integration for CAPE/CIN and precipitable
//! water. Values are labeled "approx" in the UI; they are meant for
//! situational awareness, not for replacing a full thermodynamic workstation.

use chrono::{DateTime, Utc};

#[derive(Clone, Debug)]
pub struct SoundingLevel {
    /// Pressure, hPa.
    pub pressure_hpa: f64,
    /// Height above MSL, m.
    pub height_m: f64,
    /// Temperature, °C.
    pub temp_c: f64,
    /// Dew point, °C.
    pub dewpoint_c: f64,
    /// Relative humidity, %.
    pub rh_pct: f64,
    /// Wind speed, m/s.
    pub wind_speed_ms: f64,
    /// Wind direction, degrees (from).
    pub wind_dir_deg: f64,
    /// East-west component, m/s (meteorological convention).
    pub u_ms: f64,
    /// North-south component, m/s.
    pub v_ms: f64,
}

#[derive(Clone, Debug)]
pub struct Sounding {
    pub latitude: f64,
    pub longitude: f64,
    pub valid_time: DateTime<Utc>,
    pub source: String,
    pub levels: Vec<SoundingLevel>,

    pub surface_pressure_hpa: f64,
    pub surface_temp_c: f64,
    pub surface_dewpoint_c: f64,

    pub lcl_pressure_hpa: Option<f64>,
    pub lcl_height_m: Option<f64>,
    pub lcl_temp_c: Option<f64>,
    pub lfc_pressure_hpa: Option<f64>,
    pub el_pressure_hpa: Option<f64>,

    /// Surface-based CAPE, J/kg (approx).
    pub sb_cape_jkg: f64,
    /// Surface-based CIN, J/kg (negative).
    pub sb_cin_jkg: f64,
    /// Precipitable water, mm.
    pub pwat_mm: f64,
    /// Height of the 0 °C level, m AGL-ish (MSL).
    pub freezing_level_m: Option<f64>,
    /// Height of the 0 °C wet-bulb level, m.
    pub wet_bulb_zero_m: Option<f64>,

    /// 0-1 km bulk shear, m/s.
    pub shear_0_1km_ms: f64,
    /// 0-3 km bulk shear, m/s.
    pub shear_0_3km_ms: f64,
    /// 0-6 km bulk shear, m/s.
    pub shear_0_6km_ms: f64,
    /// 0-1 km storm-relative helicity, m²/s².
    pub srh_0_1km: f64,
    /// 0-3 km storm-relative helicity, m²/s².
    pub srh_0_3km: f64,
}

const RD: f64 = 287.05;
const CP: f64 = 1004.0;
const G: f64 = 9.80665;
const KAPPA: f64 = RD / CP;

impl Sounding {
    pub fn from_levels(
        latitude: f64,
        longitude: f64,
        valid_time: DateTime<Utc>,
        source: impl Into<String>,
        mut levels: Vec<SoundingLevel>,
    ) -> Self {
        levels.sort_by(|a, b| b.pressure_hpa.partial_cmp(&a.pressure_hpa).unwrap());
        levels.retain(|l| {
            l.pressure_hpa.is_finite() && l.height_m.is_finite() && l.temp_c.is_finite()
        });

        let mut s = Self {
            latitude,
            longitude,
            valid_time,
            source: source.into(),
            surface_pressure_hpa: levels.first().map(|l| l.pressure_hpa).unwrap_or(1000.0),
            surface_temp_c: levels.first().map(|l| l.temp_c).unwrap_or(0.0),
            surface_dewpoint_c: levels.first().map(|l| l.dewpoint_c).unwrap_or(0.0),
            levels,
            lcl_pressure_hpa: None,
            lcl_height_m: None,
            lcl_temp_c: None,
            lfc_pressure_hpa: None,
            el_pressure_hpa: None,
            sb_cape_jkg: 0.0,
            sb_cin_jkg: 0.0,
            pwat_mm: 0.0,
            freezing_level_m: None,
            wet_bulb_zero_m: None,
            shear_0_1km_ms: 0.0,
            shear_0_3km_ms: 0.0,
            shear_0_6km_ms: 0.0,
            srh_0_1km: 0.0,
            srh_0_3km: 0.0,
        };
        s.compute();
        s
    }

    /// Heuristic 0–100 favorability for organized severe storms.
    ///
    /// This is a display aid, not a forecast: it blends CAPE, 0–6 km shear,
    /// CIN and the LCL. The exact weights are deliberately simple so the
    /// number can be explained and tuned.
    pub fn severe_potential(&self) -> f64 {
        let cape = (self.sb_cape_jkg / 2500.0).clamp(0.0, 1.0).sqrt();
        let shear = (self.shear_0_6km_ms / 25.0).clamp(0.0, 1.0);
        let cin = cin_factor(self.sb_cin_jkg);
        let lcl = self
            .lcl_height_m
            .map(|z| ((2500.0 - z) / 1500.0).clamp(0.0, 1.0))
            .unwrap_or(0.5);
        (100.0 * (0.55 * cape + 0.30 * shear + 0.10 * cin + 0.05 * lcl)).clamp(0.0, 100.0)
    }

    /// Heuristic 0–100 tornado-environment favorability.
    ///
    /// Leans on low-level SRH and shear, LCL height, CAPE and CIN. Positive
    /// SRH favors right-moving supercells; negative SRH is ignored here
    /// (the sounding panel still displays it).
    pub fn tornado_potential(&self) -> f64 {
        let srh1 = (self.srh_0_1km.max(0.0) / 200.0).clamp(0.0, 1.0);
        let srh3 = (self.srh_0_3km.max(0.0) / 300.0).clamp(0.0, 1.0);
        let shear1 = (self.shear_0_1km_ms / 15.0).clamp(0.0, 1.0);
        let cape = (self.sb_cape_jkg / 2000.0).clamp(0.0, 1.0);
        let cin = cin_factor(self.sb_cin_jkg);
        let lcl = self
            .lcl_height_m
            .map(|z| ((2200.0 - z) / 1400.0).clamp(0.0, 1.0))
            .unwrap_or(0.5);
        (100.0
            * (0.30 * srh1 + 0.20 * srh3 + 0.20 * shear1 + 0.15 * lcl + 0.10 * cape + 0.05 * cin))
            .clamp(0.0, 100.0)
    }

    /// What kind of winter precipitation the thermal profile supports.
    ///
    /// "ice" means a warm nose aloft over a sub-freezing surface (freezing
    /// rain); "mix" means a weaker/shallower warm layer; "snow" means a cold
    /// column; "rain" means the surface is too warm for winter precip.
    pub fn winter_type(&self) -> &'static str {
        let Some(sfc) = self.levels.first() else {
            return "unknown";
        };
        let tw = wet_bulb_c(sfc.temp_c, sfc.rh_pct);
        if tw > 2.0 {
            return "rain";
        }
        let warm_nose = self
            .levels
            .iter()
            .filter(|l| l.height_m > sfc.height_m + 200.0 && l.height_m < sfc.height_m + 3500.0)
            .map(|l| l.temp_c)
            .fold(f64::NEG_INFINITY, f64::max);
        if warm_nose > 1.0 && sfc.temp_c <= 1.0 {
            "ice"
        } else if warm_nose > 0.5 && sfc.temp_c <= 2.0 {
            "mix"
        } else {
            "snow"
        }
    }

    /// Heuristic 0–100 favorability for winter precipitation.
    ///
    /// Cold surface wet-bulb, column moisture, low-level saturation and a
    /// saturated snow-growth layer all contribute. The type (snow/mix/ice)
    /// is reported separately by [`Self::winter_type`].
    pub fn winter_potential(&self) -> f64 {
        let Some(sfc) = self.levels.first() else {
            return 0.0;
        };
        let tw = wet_bulb_c(sfc.temp_c, sfc.rh_pct);
        if tw > 3.0 {
            return 0.0;
        }
        let cold = ((2.0 - tw) / 12.0).clamp(0.0, 1.0);
        let moisture = (self.pwat_mm / 25.0).clamp(0.0, 1.0);
        let low_level: Vec<&SoundingLevel> = self
            .levels
            .iter()
            .filter(|l| l.height_m <= sfc.height_m + 4000.0)
            .collect();
        let sat = if low_level.is_empty() {
            0.0
        } else {
            low_level.iter().map(|l| l.rh_pct).sum::<f64>() / low_level.len() as f64 / 100.0
        };
        let sg: Vec<f64> = self
            .levels
            .iter()
            .filter(|l| l.temp_c <= -8.0 && l.temp_c >= -20.0)
            .map(|l| l.rh_pct)
            .collect();
        let snow_growth = if sg.is_empty() {
            0.35
        } else {
            sg.iter().sum::<f64>() / sg.len() as f64 / 100.0
        };
        let score = 100.0 * (0.35 * cold + 0.25 * moisture + 0.25 * sat + 0.15 * snow_growth);
        if self.winter_type() == "rain" {
            0.0
        } else {
            score.clamp(0.0, 100.0)
        }
    }

    pub fn potential_label(score: f64) -> &'static str {
        match score {
            s if s < 20.0 => "minimal",
            s if s < 40.0 => "low",
            s if s < 60.0 => "moderate",
            s if s < 80.0 => "high",
            _ => "very high",
        }
    }

    fn compute(&mut self) {
        if self.levels.len() < 2 {
            return;
        }
        let (lcl_p, lcl_t) = self.lcl();
        self.lcl_pressure_hpa = Some(lcl_p);
        self.lcl_height_m = Some(interp_height(&self.levels, lcl_p));
        self.lcl_temp_c = Some(lcl_t);

        self.pwat_mm = precipitable_water(&self.levels);
        self.freezing_level_m = interp_height_at_temp(&self.levels, 0.0);
        self.wet_bulb_zero_m = wet_bulb_zero(&self.levels);

        let cape = self.cape_cin(lcl_p, lcl_t);
        self.sb_cape_jkg = cape.0;
        self.sb_cin_jkg = cape.1;
        self.lfc_pressure_hpa = cape.2;
        self.el_pressure_hpa = cape.3;

        self.shear_0_1km_ms = bulk_shear(&self.levels, 1000.0);
        self.shear_0_3km_ms = bulk_shear(&self.levels, 3000.0);
        self.shear_0_6km_ms = bulk_shear(&self.levels, 6000.0);
        self.srh_0_1km = storm_relative_helicity(&self.levels, 1000.0);
        self.srh_0_3km = storm_relative_helicity(&self.levels, 3000.0);
    }

    fn lcl(&self) -> (f64, f64) {
        let s = &self.levels[0];
        let p0 = s.pressure_hpa;
        let t0 = s.temp_c;
        let td0 = s.dewpoint_c;
        // Surface mixing ratio, conserved during dry ascent.
        let e0 = vapor_pressure_hpa(td0);
        let r0 = 0.622 * e0 / (p0 - e0).max(1e-6);

        let mut p = p0;
        let mut t_parcel = t0;
        let mut td_parcel = td0;
        while p > 100.0 {
            if t_parcel <= td_parcel + 0.05 {
                return (p, t_parcel);
            }
            p -= 1.0;
            t_parcel = (t0 + 273.15) * (p / p0).powf(KAPPA) - 273.15;
            let e = r0 * p / (0.622 + r0);
            td_parcel = dewpoint_from_vapor_pressure(e);
        }
        (100.0, t_parcel)
    }

    fn cape_cin(&self, lcl_p: f64, lcl_t: f64) -> (f64, f64, Option<f64>, Option<f64>) {
        let levels = &self.levels;
        let z_lcl = interp_height(levels, lcl_p);
        let t_lcl_k = lcl_t + 273.15;
        let p0 = self.surface_pressure_hpa;
        let t0 = self.surface_temp_c;
        let e0 = vapor_pressure_hpa(self.surface_dewpoint_c);
        let q0 = 0.622 * e0 / (p0 - e0).max(1e-6);

        // Parcel temperature: dry adiabat below the LCL, moist adiabat
        // (6.5 K/km) above it.
        let parcel_t: Vec<f64> = levels
            .iter()
            .map(|l| {
                if l.pressure_hpa >= lcl_p {
                    (t0 + 273.15) * (l.pressure_hpa / p0).powf(KAPPA) - 273.15
                } else {
                    t_lcl_k - 0.0065 * (l.height_m - z_lcl) - 273.15
                }
            })
            .collect();
        let parcel_q: Vec<f64> = levels
            .iter()
            .enumerate()
            .map(|(i, l)| {
                if l.pressure_hpa >= lcl_p {
                    q0
                } else {
                    saturation_mixing_ratio(parcel_t[i], l.pressure_hpa)
                }
            })
            .collect();

        // Buoyancy at each level (m/s²).
        let b: Vec<f64> = levels
            .iter()
            .enumerate()
            .map(|(i, l)| buoyancy(l, parcel_t[i], parcel_q[i]))
            .collect();

        // LFC: first positive layer above the LCL.
        let mut lfc_idx = None;
        for i in 1..levels.len() {
            if levels[i].height_m < z_lcl {
                continue;
            }
            let b_avg = 0.5 * (b[i - 1] + b[i]);
            if b_avg > 0.0 {
                lfc_idx = Some(i);
                break;
            }
        }

        let mut cape = 0.0;
        let mut cin = 0.0;
        let mut lfc = None;
        let mut el = None;

        match lfc_idx {
            Some(lfc_i) => {
                lfc = Some(levels[lfc_i].pressure_hpa);
                // CIN: negative area from the surface to the LFC.
                for i in 1..=lfc_i {
                    let dz = levels[i].height_m - levels[i - 1].height_m;
                    if dz <= 0.0 {
                        continue;
                    }
                    let b_avg = 0.5 * (b[i - 1] + b[i]);
                    if b_avg < 0.0 {
                        cin += b_avg * dz;
                    }
                }
                // CAPE: positive area from the LFC to the EL.
                let mut last_positive = lfc_i;
                for i in (lfc_i + 1)..levels.len() {
                    let dz = levels[i].height_m - levels[i - 1].height_m;
                    if dz <= 0.0 {
                        continue;
                    }
                    let b_avg = 0.5 * (b[i - 1] + b[i]);
                    if b_avg > 0.0 {
                        cape += b_avg * dz;
                        last_positive = i;
                    } else {
                        el = Some(levels[i].pressure_hpa);
                        break;
                    }
                }
                if el.is_none() {
                    el = Some(levels[last_positive].pressure_hpa);
                }
            }
            None => {
                // No LFC: integrate inhibition up to the LCL only, so the
                // number stays meaningful instead of accumulating through
                // the whole troposphere.
                for i in 1..levels.len() {
                    if levels[i].height_m > z_lcl {
                        break;
                    }
                    let dz = levels[i].height_m - levels[i - 1].height_m;
                    if dz <= 0.0 {
                        continue;
                    }
                    let b_avg = 0.5 * (b[i - 1] + b[i]);
                    if b_avg < 0.0 {
                        cin += b_avg * dz;
                    }
                }
            }
        }

        (cape.max(0.0), cin.min(0.0), lfc, el)
    }
}

fn cin_factor(cin_jkg: f64) -> f64 {
    // No inhibition is best; -150 J/kg or stronger shuts the door.
    if cin_jkg >= -25.0 {
        1.0
    } else if cin_jkg <= -150.0 {
        0.0
    } else {
        (cin_jkg + 150.0) / 125.0
    }
}

fn buoyancy(level: &SoundingLevel, parcel_temp_c: f64, parcel_q: f64) -> f64 {
    let te = level.temp_c + 273.15;
    let tp = parcel_temp_c + 273.15;
    let q_env = mixing_ratio_kgkg(level.temp_c, level.rh_pct, level.pressure_hpa);
    let tv_env = te * (1.0 + 0.61 * q_env);
    let tv_parcel = tp * (1.0 + 0.61 * parcel_q);
    G * (tv_parcel - tv_env) / tv_env.max(1.0)
}

fn saturation_mixing_ratio(temp_c: f64, p_hpa: f64) -> f64 {
    let e = vapor_pressure_hpa(temp_c);
    0.622 * e / (p_hpa - e).max(1e-6)
}

fn mixing_ratio_kgkg(temp_c: f64, rh_pct: f64, p_hpa: f64) -> f64 {
    let e = vapor_pressure_hpa(temp_c) * (rh_pct / 100.0).clamp(0.0, 1.0);
    0.622 * e / (p_hpa - e).max(1e-6)
}

fn vapor_pressure_hpa(temp_c: f64) -> f64 {
    6.112 * ((17.67 * temp_c) / (temp_c + 243.5)).exp()
}

fn dewpoint_from_vapor_pressure(e_hpa: f64) -> f64 {
    let ln_e = (e_hpa / 6.112).ln();
    243.5 * ln_e / (17.67 - ln_e)
}

pub fn dewpoint(temp_c: f64, rh_pct: f64) -> f64 {
    let e = vapor_pressure_hpa(temp_c) * (rh_pct / 100.0).clamp(0.01, 1.0);
    dewpoint_from_vapor_pressure(e)
}

fn interp_height(levels: &[SoundingLevel], pressure_hpa: f64) -> f64 {
    if levels.is_empty() {
        return 0.0;
    }
    if pressure_hpa >= levels[0].pressure_hpa {
        return levels[0].height_m;
    }
    if pressure_hpa <= levels.last().unwrap().pressure_hpa {
        return levels.last().unwrap().height_m;
    }
    for i in 0..levels.len() - 1 {
        let a = &levels[i];
        let b = &levels[i + 1];
        if pressure_hpa <= a.pressure_hpa && pressure_hpa >= b.pressure_hpa {
            let la = a.pressure_hpa.ln();
            let lb = b.pressure_hpa.ln();
            let lp = pressure_hpa.ln();
            let t = (la - lp) / (la - lb);
            return a.height_m + t * (b.height_m - a.height_m);
        }
    }
    levels.last().unwrap().height_m
}

fn interp_height_at_temp(levels: &[SoundingLevel], target_c: f64) -> Option<f64> {
    for i in 0..levels.len() - 1 {
        let a = &levels[i];
        let b = &levels[i + 1];
        if (a.temp_c - target_c) * (b.temp_c - target_c) <= 0.0 {
            let dt = b.temp_c - a.temp_c;
            if dt.abs() < 1e-9 {
                return Some(a.height_m);
            }
            let frac = (target_c - a.temp_c) / dt;
            return Some(a.height_m + frac * (b.height_m - a.height_m));
        }
    }
    None
}

fn precipitable_water(levels: &[SoundingLevel]) -> f64 {
    let mut pw = 0.0;
    for i in 0..levels.len() - 1 {
        let a = &levels[i];
        let b = &levels[i + 1];
        let qa = mixing_ratio_kgkg(a.temp_c, a.rh_pct, a.pressure_hpa);
        let qb = mixing_ratio_kgkg(b.temp_c, b.rh_pct, b.pressure_hpa);
        let q = 0.5 * (qa + qb);
        let dp = a.pressure_hpa - b.pressure_hpa;
        pw += dp * 100.0 / G * q;
    }
    pw.max(0.0)
}

fn wet_bulb_c(temp_c: f64, rh_pct: f64) -> f64 {
    // Stull (2011) wet-bulb approximation.
    let t = temp_c;
    let rh = rh_pct.clamp(1.0, 100.0);
    t * (0.151977 * (rh + 8.313659).sqrt()).atan() + (t + rh).atan() - (rh - 1.676331).atan()
        + 0.00391838 * rh.powf(1.5) * (0.023101 * rh).atan()
        - 4.686035
}

fn wet_bulb_zero(levels: &[SoundingLevel]) -> Option<f64> {
    let wbs: Vec<f64> = levels
        .iter()
        .map(|l| wet_bulb_c(l.temp_c, l.rh_pct))
        .collect();
    for i in 0..levels.len() - 1 {
        if (wbs[i] >= 0.0 && wbs[i + 1] <= 0.0) || (wbs[i] <= 0.0 && wbs[i + 1] >= 0.0) {
            let dt = wbs[i + 1] - wbs[i];
            if dt.abs() < 1e-9 {
                return Some(levels[i].height_m);
            }
            let frac = (0.0 - wbs[i]) / dt;
            return Some(levels[i].height_m + frac * (levels[i + 1].height_m - levels[i].height_m));
        }
    }
    None
}

fn bulk_shear(levels: &[SoundingLevel], height_m: f64) -> f64 {
    let bottom = &levels[0];
    let top = levels
        .iter()
        .min_by(|a, b| {
            (a.height_m - height_m)
                .abs()
                .partial_cmp(&(b.height_m - height_m).abs())
                .unwrap()
        })
        .unwrap_or(bottom);
    let du = top.u_ms - bottom.u_ms;
    let dv = top.v_ms - bottom.v_ms;
    (du * du + dv * dv).sqrt()
}

fn storm_motion(levels: &[SoundingLevel]) -> (f64, f64) {
    let top_idx = levels
        .iter()
        .position(|l| l.height_m >= 6000.0)
        .unwrap_or(levels.len() - 1)
        .max(1);
    let mean_u = levels[..=top_idx].iter().map(|l| l.u_ms).sum::<f64>() / (top_idx + 1) as f64;
    let mean_v = levels[..=top_idx].iter().map(|l| l.v_ms).sum::<f64>() / (top_idx + 1) as f64;
    let shear_u = levels[top_idx].u_ms - levels[0].u_ms;
    let shear_v = levels[top_idx].v_ms - levels[0].v_ms;
    let mag = (shear_u * shear_u + shear_v * shear_v).sqrt().max(1e-6);
    // Right-moving storm motion: 7.5 m/s to the right of the shear vector.
    let perp_u = shear_v / mag;
    let perp_v = -shear_u / mag;
    (mean_u + 7.5 * perp_u, mean_v + 7.5 * perp_v)
}

fn storm_relative_helicity(levels: &[SoundingLevel], height_m: f64) -> f64 {
    let (cm_u, cm_v) = storm_motion(levels);
    let mut srh = 0.0;
    for i in 0..levels.len() - 1 {
        let a = &levels[i];
        let b = &levels[i + 1];
        if a.height_m > height_m {
            break;
        }
        let top = b.height_m.min(height_m);
        if top <= a.height_m {
            continue;
        }
        let frac = (top - a.height_m) / (b.height_m - a.height_m).max(1e-6);
        let u_b = a.u_ms + frac * (b.u_ms - a.u_ms);
        let v_b = a.v_ms + frac * (b.v_ms - a.v_ms);
        srh += (a.u_ms - cm_u) * (v_b - a.v_ms) - (a.v_ms - cm_v) * (u_b - a.u_ms);
    }
    srh
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lvl(p: f64, z: f64, t: f64, rh: f64) -> SoundingLevel {
        SoundingLevel {
            pressure_hpa: p,
            height_m: z,
            temp_c: t,
            dewpoint_c: dewpoint(t, rh),
            rh_pct: rh,
            wind_speed_ms: 10.0,
            wind_dir_deg: 200.0,
            u_ms: -6.0,
            v_ms: 8.0,
        }
    }

    fn sounding(levels: Vec<SoundingLevel>) -> Sounding {
        Sounding::from_levels(0.0, 0.0, Utc::now(), "test", levels)
    }

    #[test]
    fn winter_type_detects_snow_ice_and_rain() {
        let snow = sounding(vec![
            lvl(1000.0, 300.0, -5.0, 90.0),
            lvl(850.0, 1500.0, -8.0, 90.0),
            lvl(700.0, 3000.0, -15.0, 80.0),
            lvl(500.0, 5500.0, -30.0, 60.0),
        ]);
        assert_eq!(snow.winter_type(), "snow");
        assert!(snow.winter_potential() > 0.0);

        let ice = sounding(vec![
            lvl(1000.0, 300.0, -2.0, 90.0),
            lvl(850.0, 1500.0, 2.0, 90.0),
            lvl(700.0, 3000.0, -5.0, 80.0),
            lvl(500.0, 5500.0, -20.0, 60.0),
        ]);
        assert_eq!(ice.winter_type(), "ice");

        let rain = sounding(vec![
            lvl(1000.0, 300.0, 15.0, 80.0),
            lvl(850.0, 1500.0, 10.0, 80.0),
            lvl(700.0, 3000.0, 2.0, 70.0),
            lvl(500.0, 5500.0, -15.0, 50.0),
        ]);
        assert_eq!(rain.winter_type(), "rain");
        assert_eq!(rain.winter_potential(), 0.0);
    }
}
