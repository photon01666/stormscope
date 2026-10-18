//! Sampling a decoded volume: "what is at this point of the scene?".
//!
//! A NEXRAD volume is a polar measurement grid (azimuth × slant range × cut
//! elevation), so every consumer that wants a value at an arbitrary point —
//! the 3D data probe, the vertical cross-section, the isosurface grid, the
//! derived products — first has to answer where in the polar grid that point
//! lands. This module is that shared answer, in radar-core so both the
//! product code ([`crate::derived`]) and the renderer use one implementation.
//!
//! Two shapes of the same machinery:
//!
//! * [`VolumeSampler`] — arbitrary scene points `(x, y, z)` km, ENU with the
//!   radar at the origin. Nearest ray, linear between gates, linear between
//!   the two cuts bracketing the point's elevation.
//! * [`Profile`] — one azimuth, prepared once: per cut the nearest ray is
//!   picked up front, so a whole vertical swath (a cross-section column, an
//!   isosurface column, a VIL/Echo-Tops column) costs a gate interpolation
//!   per query instead of an azimuth search per query.

use crate::{Moment, MomentData, Volume};

/// Standard radar beam height: 4/3-earth effective-radius model, the same
/// approximation the raster path uses for its gates.
///
/// `h = r·sin(θ) + r² / (2·k·R)` with k = 4/3 and R = 6371 km. At 0.5° and
/// 100 km that is 0.87 + 0.59 ≈ 1.46 km — the classic "height of the 0.5°
/// beam at 100 km" number forecasters quote.
pub fn beam_height_km(slant_range_km: f32, elevation_deg: f32) -> f32 {
    const K_RE: f32 = (4.0 / 3.0) * 6371.0;
    let e = elevation_deg.to_radians();
    slant_range_km * e.sin() + (slant_range_km * slant_range_km) / (2.0 * K_RE)
}

/// Shortest signed difference `a − b` in degrees, wrapped to (−180, 180].
pub fn angle_delta(a: f32, b: f32) -> f32 {
    (a - b + 180.0).rem_euclid(360.0) - 180.0
}

/// Index and signed distance of the ray nearest `az_deg` in an azimuth ring.
///
/// Ascending rings (every real volume) binary-search; a ring that is not
/// sorted falls back to a scan. Rays are a fraction of a degree apart, so the
/// neighbor comparison after the search is exact for any ring that covers a
/// contiguous arc.
fn nearest_ray(azimuths: &[f32], sorted: bool, az_deg: f32) -> Option<(usize, f32)> {
    let n = azimuths.len();
    if n == 0 {
        return None;
    }
    let t = az_deg.rem_euclid(360.0);
    if !sorted {
        let mut best = 0usize;
        let mut best_d = angle_delta(azimuths[0], t);
        for (i, a) in azimuths.iter().enumerate().skip(1) {
            let d = angle_delta(*a, t);
            if d.abs() < best_d.abs() {
                best = i;
                best_d = d;
            }
        }
        return Some((best, best_d));
    }
    let idx = azimuths.partition_point(|a| *a < t);
    // The insertion point and its neighbors. When the search lands past the
    // end the wrap candidate (ray 0) is what "just under 0°" means, so the
    // index walk is modular.
    let mut best = 0;
    let mut best_d = f32::MAX;
    for k in [-1i64, 0, 1] {
        let i = (idx as i64 + k).rem_euclid(n as i64) as usize;
        let d = angle_delta(azimuths[i], t);
        if d.abs() < best_d {
            best = i;
            best_d = d;
        }
    }
    // Pathological spacing (a sparse, uneven ring) can hide the true nearest
    // outside the ±1 window; past 5° the scan is the honest answer.
    if best_d.abs() > 5.0 {
        return nearest_ray(azimuths, false, az_deg);
    }
    Some((best, best_d))
}

/// Per-cut sampling state: the moment's data plus its azimuth ring.
struct CutSampler<'a> {
    elevation_deg: f32,
    data: &'a MomentData,
    azimuths: &'a [f32],
    sorted: bool,
}

impl<'a> CutSampler<'a> {
    fn elevation(volume: &'a Volume, sweep_index: usize) -> f32 {
        let s = &volume.sweeps[sweep_index];
        if s.elevation_measured_deg.is_finite() && s.elevation_measured_deg != 0.0 {
            s.elevation_measured_deg
        } else {
            s.elevation_deg
        }
    }

    /// Value at one cut for a slant range, for an already-chosen ray:
    /// linear between the two nearest gates, `None` outside the data or in a
    /// no-echo gap.
    fn value_at_slant(&self, ray: usize, slant_km: f32) -> Option<f32> {
        let m = self.data;
        if m.gate_count == 0 {
            return None;
        }
        let slant_m = slant_km * 1000.0;
        let gf = (slant_m - m.range_start_m) / m.gate_size_m;
        if gf < -0.5 || gf > m.gate_count as f32 - 0.5 {
            return None;
        }
        let g0 = gf
            .floor()
            .clamp(0.0, (m.gate_count.saturating_sub(2)) as f32) as usize;
        let t = (gf - g0 as f32).clamp(0.0, 1.0);
        let v0 = m.value(ray, g0)?;
        let v1 = m.value(ray, g0 + 1).unwrap_or(v0);
        Some(v0 + (v1 - v0) * t)
    }
}

/// Interpolating sampler over one moment of a volume: the echo at an
/// arbitrary scene point (km ENU, X east / Y north / Z beam height).
///
/// Sampling is nearest-ray + linear between gates, then linear between the
/// two cuts bracketing the point's elevation; the (r, z) ↔ (slant, elev)
/// mapping ignores earth curvature (it is ~2 km of error at 200 km — far
/// below the grid the callers sample on).
pub struct VolumeSampler<'a> {
    cuts: Vec<CutSampler<'a>>,
}

impl<'a> VolumeSampler<'a> {
    pub fn new(volume: &'a Volume, moment: Moment) -> Self {
        let mut cuts: Vec<CutSampler<'a>> = volume
            .sweeps
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                s.moment(moment).map(|m| CutSampler {
                    elevation_deg: CutSampler::elevation(volume, i),
                    data: m,
                    azimuths: &s.azimuths_deg,
                    sorted: s.azimuths_deg.windows(2).all(|w| w[1] >= w[0]),
                })
            })
            .collect();
        cuts.sort_by(|a, b| {
            a.elevation_deg
                .partial_cmp(&b.elevation_deg)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Self { cuts }
    }

    /// Longest slant range covered by any cut (km).
    pub fn max_range_km(&self) -> f32 {
        self.cuts
            .iter()
            .map(|c| {
                (c.data.range_start_m + c.data.gate_count as f32 * c.data.gate_size_m) / 1000.0
            })
            .fold(0.0, f32::max)
    }

    /// Highest cut elevation (deg).
    pub fn top_elevation_deg(&self) -> f32 {
        self.cuts.last().map(|c| c.elevation_deg).unwrap_or(0.0)
    }

    /// Number of cuts carrying the moment.
    pub fn cut_count(&self) -> usize {
        self.cuts.len()
    }

    /// The value at a scene point, or `None` outside the data (below the
    /// lowest cut, above the highest, beyond range, or in a no-echo gap).
    pub fn sample(&self, x_km: f32, y_km: f32, z_km: f32) -> Option<f32> {
        if self.cuts.is_empty() {
            return None;
        }
        let ground = x_km.hypot(y_km);
        let slant = ground.hypot(z_km);
        let elev = z_km.atan2(ground).to_degrees();
        let az = x_km.atan2(y_km).to_degrees().rem_euclid(360.0);

        // Strictly within the sampled elevation range (a hair of tolerance
        // for floating point on "exactly the top cut"): above the highest
        // cut there is no data to sample, so the answer is None — the
        // cross-section and isosurface callers rely on that to stop.
        let lo = self.cuts.first().map(|c| c.elevation_deg).unwrap_or(0.0);
        let hi = self.cuts.last().map(|c| c.elevation_deg).unwrap_or(0.0);
        if elev < lo - 0.05 || elev > hi + 0.05 {
            return None;
        }

        // Bracket the point's elevation between the two nearest cuts.
        let mut above = None;
        let mut below = None;
        for (i, c) in self.cuts.iter().enumerate() {
            if c.elevation_deg <= elev {
                below = Some(i);
            } else {
                above = Some(i);
                break;
            }
        }
        match (below, above) {
            (Some(b), Some(a)) => {
                let cb = self.sample_cut(b, az, slant)?;
                let ca = self.sample_cut(a, az, slant)?;
                let t = (elev - self.cuts[b].elevation_deg)
                    / (self.cuts[a].elevation_deg - self.cuts[b].elevation_deg);
                Some(cb + (ca - cb) * t)
            }
            (Some(b), None) => self.sample_cut(b, az, slant),
            (None, Some(a)) => self.sample_cut(a, az, slant),
            (None, None) => None,
        }
    }

    /// One cut: nearest ray, linear between the two nearest gates.
    fn sample_cut(&self, cut: usize, az_deg: f32, slant_km: f32) -> Option<f32> {
        let c = &self.cuts[cut];
        let (ray, _) = nearest_ray(c.azimuths, c.sorted, az_deg)?;
        c.value_at_slant(ray, slant_km)
    }

    /// Prepare one azimuth for repeated vertical sampling (cross-sections,
    /// isosurface columns, derived-product columns): the nearest ray of every
    /// cut is resolved once.
    pub fn profile(&self, az_deg: f32) -> Profile<'_> {
        let cuts = self
            .cuts
            .iter()
            .filter_map(|c| {
                nearest_ray(c.azimuths, c.sorted, az_deg).map(|(ray, _)| ProfileCut {
                    elevation_deg: c.elevation_deg,
                    data: c.data,
                    ray,
                })
            })
            .collect();
        Profile { cuts }
    }
}

/// One cut inside a [`Profile`].
struct ProfileCut<'a> {
    elevation_deg: f32,
    data: &'a MomentData,
    ray: usize,
}

/// A prepared vertical column at one azimuth: value at any (slant range,
/// height), like [`VolumeSampler::sample`] but without the per-query azimuth
/// search. Cheap to query thousands of times.
pub struct Profile<'a> {
    cuts: Vec<ProfileCut<'a>>,
}

impl<'a> Profile<'a> {
    /// Cuts carrying data, ascending elevation.
    pub fn cut_count(&self) -> usize {
        self.cuts.len()
    }

    pub fn cut_elevation_deg(&self, cut: usize) -> Option<f32> {
        self.cuts.get(cut).map(|c| c.elevation_deg)
    }

    /// Longest slant range covered by any cut (km).
    pub fn max_range_km(&self) -> f32 {
        self.cuts
            .iter()
            .map(|c| {
                (c.data.range_start_m + c.data.gate_count as f32 * c.data.gate_size_m) / 1000.0
            })
            .fold(0.0, f32::max)
    }

    /// Value at one cut for a slant range (linear between gates; `None`
    /// outside the data or in a no-echo gap).
    pub fn value_at_slant(&self, cut: usize, slant_km: f32) -> Option<f32> {
        let c = self.cuts.get(cut)?;
        let m = c.data;
        if m.gate_count == 0 {
            return None;
        }
        let slant_m = slant_km * 1000.0;
        let gf = (slant_m - m.range_start_m) / m.gate_size_m;
        if gf < -0.5 || gf > m.gate_count as f32 - 0.5 {
            return None;
        }
        let g0 = gf
            .floor()
            .clamp(0.0, (m.gate_count.saturating_sub(2)) as f32) as usize;
        let t = (gf - g0 as f32).clamp(0.0, 1.0);
        let v0 = m.value(c.ray, g0)?;
        let v1 = m.value(c.ray, g0 + 1).unwrap_or(v0);
        Some(v0 + (v1 - v0) * t)
    }

    /// The value at a point on this column's vertical plane: `ground_km` is
    /// the horizontal distance from the radar, `z_km` the height. Same
    /// semantics as [`VolumeSampler::sample`].
    pub fn sample(&self, ground_km: f32, z_km: f32) -> Option<f32> {
        if self.cuts.is_empty() {
            return None;
        }
        let slant = ground_km.hypot(z_km);
        let elev = z_km.atan2(ground_km).to_degrees();
        let lo = self.cuts.first().map(|c| c.elevation_deg).unwrap_or(0.0);
        let hi = self.cuts.last().map(|c| c.elevation_deg).unwrap_or(0.0);
        if elev < lo - 0.05 || elev > hi + 0.05 {
            return None;
        }
        let mut above = None;
        let mut below = None;
        for (i, c) in self.cuts.iter().enumerate() {
            if c.elevation_deg <= elev {
                below = Some(i);
            } else {
                above = Some(i);
                break;
            }
        }
        match (below, above) {
            (Some(b), Some(a)) => {
                let cb = self.value_at_slant(b, slant)?;
                let ca = self.value_at_slant(a, slant)?;
                let t = (elev - self.cuts[b].elevation_deg)
                    / (self.cuts[a].elevation_deg - self.cuts[b].elevation_deg);
                Some(cb + (ca - cb) * t)
            }
            (Some(b), None) => self.value_at_slant(b, slant),
            (None, Some(a)) => self.value_at_slant(a, slant),
            (None, None) => None,
        }
    }
}

/// Distance-sorted gates: how far a point at (x, y, z) is along a ray.
/// Re-exported convenience for the probe's ground-distance bookkeeping.
pub fn ground_range_km(x_km: f32, y_km: f32) -> f32 {
    x_km.hypot(y_km)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MomentCalibration, Sweep, VolumeMeta};
    use chrono::TimeZone;

    fn cal() -> MomentCalibration {
        MomentCalibration {
            scale: 1.0,
            offset: 2.0,
            threshold: 0.0,
        }
    }

    /// Two cuts at 0.5 and 1.5 deg, constant 20 and 40 dBZ, every ray.
    fn test_volume() -> Volume {
        let mk = |elev: f32, dbz: f32| {
            let rays: Vec<f32> = (0..360).map(|i| i as f32).collect();
            let mut data = vec![0i16; 360 * 200];
            for r in 0..360 {
                for g in 0..200 {
                    data[r * 200 + g] = (dbz + 2.0) as i16;
                }
            }
            Sweep {
                elevation_deg: elev,
                elevation_measured_deg: elev,
                azimuth_width_deg: 1.0,
                azimuths_deg: rays,
                moments: vec![MomentData {
                    kind: Moment::Reflectivity,
                    calibration: cal(),
                    range_start_m: 0.0,
                    gate_size_m: 250.0,
                    gate_count: 200,
                    ray_count: 360,
                    data,
                }],
            }
        };
        Volume {
            meta: VolumeMeta {
                site_id: "KTWX".into(),
                volume_start: chrono::Utc.with_ymd_and_hms(2026, 10, 18, 0, 0, 0).unwrap(),
                vcp: None,
                sequence_number: None,
                sweep_count: 2,
                moments: vec![Moment::Reflectivity],
            },
            sweeps: vec![mk(0.5, 20.0), mk(1.5, 40.0)],
        }
    }

    #[test]
    fn sampler_interpolates_between_cuts_and_honors_gaps() {
        let vol = test_volume();
        let s = VolumeSampler::new(&vol, Moment::Reflectivity);
        assert!((s.max_range_km() - 50.0).abs() < 0.01);

        // 30 km north (inside the 50 km sweep), elevation 1.0 deg (halfway
        // between the cuts) -> 30.
        let h = 30.0 * 1.0f32.to_radians().tan();
        let v = s.sample(0.0, 30.0, h).expect("inside the data");
        assert!((v - 30.0).abs() < 0.5, "expected ~30, got {v}");

        // Exactly on a cut -> that cut's value.
        let h0 = 30.0 * 0.5f32.to_radians().tan();
        let v = s.sample(0.0, 30.0, h0).expect("on the low cut");
        assert!((v - 20.0).abs() < 0.2, "expected ~20, got {v}");

        // Above the highest cut / beyond range / in a dropped ray: None.
        assert!(
            s.sample(0.0, 30.0, 30.0 * 3.0f32.to_radians().tan())
                .is_none()
        );
        assert!(s.sample(0.0, 400.0, 2.0).is_none());
    }

    /// The prepared profile must agree with the point sampler wherever both
    /// cover the same point — the cross-section and the probe share one
    /// physical answer.
    #[test]
    fn profile_matches_the_point_sampler() {
        let vol = test_volume();
        let s = VolumeSampler::new(&vol, Moment::Reflectivity);
        let p = s.profile(33.0);
        assert_eq!(p.cut_count(), 2);
        for (ground, z) in [(10.0f32, 0.5f32), (33.0, 1.0), (49.0, 0.8), (25.0, 0.2)] {
            let x = ground * 33.0f32.to_radians().sin();
            let y = ground * 33.0f32.to_radians().cos();
            let a = s.sample(x, y, z);
            let b = p.sample(ground, z);
            match (a, b) {
                (None, None) => {}
                (Some(a), Some(b)) => {
                    assert!((a - b).abs() < 1.0e-3, "({ground}, {z}): {a} vs {b}")
                }
                (a, b) => panic!("({ground}, {z}): sampler {a:?} vs profile {b:?}"),
            }
        }
        // Above the top cut: both refuse.
        assert!(p.sample(30.0, 4.0).is_none());
    }

    /// A ray list that is not sorted falls back to the scan and still finds
    /// the nearest ray.
    #[test]
    fn nearest_ray_handles_unsorted_and_wrap_around() {
        let sorted = [0.0f32, 90.0, 180.0, 270.0];
        let (i, _) = nearest_ray(&sorted, true, 359.0).unwrap();
        assert_eq!(i, 0);
        let (i, _) = nearest_ray(&sorted, true, 91.0).unwrap();
        assert_eq!(i, 1);
        let unsorted = [270.0f32, 0.0, 180.0, 90.0];
        let (i, _) = nearest_ray(&unsorted, false, 89.0).unwrap();
        assert_eq!(i, 3);
    }
}
