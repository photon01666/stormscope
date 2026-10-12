//! Projection: WGS84 <-> Web Mercator (EPSG:3857) tile pixels, and radar
//! gate geolocation with the 4/3-earth refraction model. (W3)

/// Current viewport in screen pixels at a given integer-ish zoom.
#[derive(Copy, Clone, Debug)]
pub struct MapView {
    pub center_lat: f64,
    pub center_lon: f64,
    pub zoom: f64,
    pub width_px: f32,
    pub height_px: f32,
}

impl Default for MapView {
    fn default() -> Self {
        Self { center_lat: 35.3331, center_lon: -97.2778, zoom: 8.0, width_px: 1280.0, height_px: 800.0 }
    }
}

impl MapView {
    /// Viewport width in whole pixels (rounded).
    pub fn width(&self) -> usize {
        (self.width_px.round() as i64).max(0) as usize
    }

    /// Viewport height in whole pixels (rounded).
    pub fn height(&self) -> usize {
        (self.height_px.round() as i64).max(0) as usize
    }

    /// Web Mercator pixel coordinates (at this view's zoom) of the viewport's
    /// top-left corner. Every layer (tiles, radar, warnings) must derive its
    /// screen position from this origin so the layers line up exactly.
    pub fn origin_px(&self) -> (f64, f64) {
        let (cx, cy) = lonlat_to_merc_px(self.center_lon, self.center_lat, self.zoom);
        (cx - self.width() as f64 / 2.0, cy - self.height() as f64 / 2.0)
    }

    /// Project WGS84 to viewport pixel coordinates; (0,0) is the top-left
    /// corner of the viewport, x grows east, y grows south.
    pub fn lonlat_to_view_px(&self, lon: f64, lat: f64) -> (f64, f64) {
        let (mx, my) = lonlat_to_merc_px(lon, lat, self.zoom);
        let (ox, oy) = self.origin_px();
        (mx - ox, my - oy)
    }

    /// Inverse of [`Self::lonlat_to_view_px`].
    pub fn view_px_to_lonlat(&self, px: f64, py: f64) -> (f64, f64) {
        let (ox, oy) = self.origin_px();
        merc_px_to_lonlat(px + ox, py + oy, self.zoom)
    }
}

const EARTH_CIRCUMFERENCE: f64 = 40_075_016.686; // meters, Web Mercator

/// Mean earth radius used for surface (great-circle) distances, meters.
pub const EARTH_RADIUS_M: f64 = 6_371_000.0;

/// Effective earth radius for the 4/3-earth refraction model, meters.
pub const REFRACTED_EARTH_RADIUS_M: f64 = 4.0 / 3.0 * EARTH_RADIUS_M;

/// Zoom 0 => 256 px global tile.
pub fn lonlat_to_merc_px(lon: f64, lat: f64, zoom: f64) -> (f64, f64) {
    let scale = 256.0 * 2f64.powf(zoom);
    let x = (lon + 180.0) / 360.0 * scale;
    let y = merc_y_px(lat, scale);
    (x, y)
}

/// Standard Web Mercator y (pixels) for a latitude at a given world scale.
#[inline]
fn merc_y_px(lat: f64, scale: f64) -> f64 {
    // Web Mercator is only defined to +/-85.05112878 degrees.
    let lat_rad = lat.clamp(-85.05112878, 85.05112878).to_radians();
    (1.0 - (std::f64::consts::FRAC_PI_4 + lat_rad / 2.0).tan().ln() / std::f64::consts::PI) / 2.0
        * scale
}

pub fn merc_px_to_lonlat(x: f64, y: f64, zoom: f64) -> (f64, f64) {
    let scale = 256.0 * 2f64.powf(zoom);
    let lon = x / scale * 360.0 - 180.0;
    let n = std::f64::consts::PI * (1.0 - 2.0 * y / scale);
    let lat = n.sinh().atan().to_degrees();
    (lon, lat)
}

/// Meters per screen pixel at a latitude/zoom (for sizing splats/range rings).
/// This is *ground* distance per pixel; the Mercator x-scale is uniform.
pub fn meters_per_pixel(lat: f64, zoom: f64) -> f64 {
    EARTH_CIRCUMFERENCE * lat.to_radians().cos() / (256.0 * 2f64.powf(zoom))
}

/// Precomputed projection for one viewport: lets a caller project thousands of
/// lon/lats into the same view without re-deriving the zoom scale each call.
/// This is the single source of truth every layer uses for pixel placement.
#[derive(Copy, Clone, Debug)]
pub struct MercProjector {
    scale: f64,
    origin_x: f64,
    origin_y: f64,
}

impl MercProjector {
    pub fn new(view: &MapView) -> Self {
        let scale = 256.0 * 2f64.powf(view.zoom);
        let (cx, cy) = lonlat_to_merc_px(view.center_lon, view.center_lat, view.zoom);
        let (w, h) = (view.width(), view.height());
        Self {
            scale,
            origin_x: cx - w as f64 / 2.0,
            origin_y: cy - h as f64 / 2.0,
        }
    }

    /// Project lon/lat to viewport pixel coordinates (0,0 = top-left).
    #[inline]
    pub fn view_px(&self, lon: f64, lat: f64) -> (f64, f64) {
        let x = (lon + 180.0) / 360.0 * self.scale;
        let y = merc_y_px(lat, self.scale);
        (x - self.origin_x, y - self.origin_y)
    }
}

/// Geolocate one radar gate: slant range (m), azimuth (deg, 0=N clockwise),
/// elevation (deg) -> (lat, lon), using the 4/3-earth refraction model:
///
/// * `h = sqrt(r² + Re² + 2·r·Re·sin(elev)) − Re` (height above the surface)
/// * `d = Re·asin(r·cos(elev)/(Re+h))` (ground distance along the surface)
/// * then a great-circle march of length `d` from the radar site along `azimuth`.
///
/// `d` comes out as a *ground* distance in meters, so the march runs on the
/// mean-radius sphere: a near-horizontal gate at slant range R lands ~R km
/// from the site.
pub fn gate_latlon(
    radar_lat: f64,
    radar_lon: f64,
    azimuth_deg: f32,
    elevation_deg: f32,
    slant_range_m: f32,
) -> (f64, f64) {
    let re = REFRACTED_EARTH_RADIUS_M;
    let r = (slant_range_m as f64).max(0.0);
    let elev = (elevation_deg as f64).to_radians();

    // Height of the gate above the (4/3) earth surface.
    let h = (r * r + re * re + 2.0 * r * re * elev.sin()).sqrt() - re;
    // Ground distance radar -> gate (arc on the effective sphere), clamped for
    // numerical safety at extreme ranges.
    let arg = (r * elev.cos() / (re + h)).clamp(-1.0, 1.0);
    let d = re * arg.asin();

    // Great-circle destination: march `d` meters from the site along azimuth.
    let delta = d / EARTH_RADIUS_M;
    let theta = (azimuth_deg as f64).to_radians();
    let phi1 = (radar_lat).to_radians();
    let lambda1 = radar_lon.to_radians();

    let sin_phi2 = phi1.sin() * delta.cos() + phi1.cos() * delta.sin() * theta.cos();
    let phi2 = sin_phi2.clamp(-1.0, 1.0).asin();
    let lambda2 = lambda1
        + (theta.sin() * delta.sin() * phi1.cos())
            .atan2(delta.cos() - phi1.sin() * phi2.sin());

    let mut lat = phi2.to_degrees();
    let mut lon = lambda2.to_degrees();
    // Normalize longitude into [-180, 180).
    lon = lon.rem_euclid(360.0);
    if lon > 180.0 {
        lon -= 360.0;
    }
    lat = lat.clamp(-90.0, 90.0);
    (lat, lon)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Great-circle distance between two lon/lats on the mean-radius sphere.
    fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
        let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
        let dp = (lat2 - lat1).to_radians();
        let dl = (lon2 - lon1).to_radians();
        let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
        2.0 * EARTH_RADIUS_M * a.sqrt().atan2((1.0 - a).sqrt())
    }

    #[test]
    fn round_trip() {
        for &(lon, lat, zoom) in &[(-97.2778, 35.3331, 8.0), (0.0, 0.0, 3.0), (151.2, -33.87, 11.0)] {
            let (x, y) = lonlat_to_merc_px(lon, lat, zoom);
            let (lon2, lat2) = merc_px_to_lonlat(x, y, zoom);
            assert!((lon - lon2).abs() < 1e-9 && (lat - lat2).abs() < 1e-9);
        }
    }

    #[test]
    fn known_values() {
        // Zoom 0: whole world is 256 px.
        let (x, y) = lonlat_to_merc_px(0.0, 0.0, 0.0);
        assert!((x - 128.0).abs() < 1e-9 && (y - 128.0).abs() < 1e-9);
    }

    #[test]
    fn meters_per_pixel_known_values() {
        let m0 = meters_per_pixel(0.0, 0.0);
        assert!((m0 - EARTH_CIRCUMFERENCE / 256.0).abs() < 1e-9);
        // Halves every zoom step.
        let m1 = meters_per_pixel(0.0, 1.0);
        assert!((m1 - m0 / 2.0).abs() < 1e-9);
        // Shrinks with latitude.
        assert!(meters_per_pixel(60.0, 3.0) < meters_per_pixel(0.0, 3.0));
    }

    #[test]
    fn view_px_round_trip_and_center() {
        let view = MapView {
            center_lat: 35.3331,
            center_lon: -97.2778,
            zoom: 9.0,
            width_px: 1024.0,
            height_px: 768.0,
        };
        // The view center must land exactly on the viewport center.
        let (px, py) = view.lonlat_to_view_px(view.center_lon, view.center_lat);
        assert!((px - 512.0).abs() < 1e-9 && (py - 384.0).abs() < 1e-9);
        // Round trip through view pixel coordinates.
        let (lon, lat) = view.view_px_to_lonlat(px, py);
        assert!((lon - view.center_lon).abs() < 1e-9 && (lat - view.center_lat).abs() < 1e-9);
        // The precomputed projector agrees.
        let proj = MercProjector::new(&view);
        let (qx, qy) = proj.view_px(-97.0, 36.0);
        let (rx, ry) = view.lonlat_to_view_px(-97.0, 36.0);
        assert!((qx - rx).abs() < 1e-9 && (qy - ry).abs() < 1e-9);
        // East is right, north is up.
        assert!(rx > 512.0 && ry < 384.0);
    }

    #[test]
    fn gate_due_east_lands_east_and_deflects() {
        let (lat, lon) = gate_latlon(35.0, -97.0, 90.0, 0.5, 100_000.0);
        // East of the site...
        assert!(lon > -97.0, "lon={lon}");
        // ...and the great circle deflects by ~0.005 deg over 100 km. A
        // *due-east* great circle starts exactly at its vertex (azimuth 90 =
        // max latitude), so it bows equatorward (south in the northern
        // hemisphere); an off-axis (NE) ray bows poleward — see
        // `gate_northeast_is_poleward` below. Either way: definite deflection.
        assert!(lat < 35.0 - 1e-3, "due-east gate should bow equatorward: lat={lat}");
        // Ground distance stays within 0.3% of the slant range.
        let d = haversine_m(35.0, -97.0, lat, lon);
        assert!((d - 100_000.0).abs() < 300.0, "d={d}");
    }

    #[test]
    fn gate_northeast_is_poleward() {
        // Off the vertex, a great circle bows poleward: heading NE from 35N
        // the vertex sits near 54N, so latitude climbs.
        let (lat, lon) = gate_latlon(35.0, -97.0, 45.0, 0.5, 100_000.0);
        assert!(lon > -97.0, "lon={lon}");
        assert!(lat > 35.0 + 1e-3, "NE gate should bow poleward: lat={lat}");
        let d = haversine_m(35.0, -97.0, lat, lon);
        assert!((d - 100_000.0).abs() < 300.0, "d={d}");
    }

    #[test]
    fn gate_near_zero_elevation_range_r_is_r_km_away() {
        for &r in &[10_000.0f64, 50_000.0, 120_000.0] {
            let (lat, lon) = gate_latlon(35.0, -97.0, 45.0, 0.0, r as f32);
            let d = haversine_m(35.0, -97.0, lat, lon);
            assert!((d - r).abs() < r * 0.002, "r={r} d={d}");
        }
    }

    #[test]
    fn gate_cardinal_directions() {
        let (nlat, nlon) = gate_latlon(35.0, -97.0, 0.0, 0.0, 10_000.0);
        assert!(nlat > 35.0 && (nlon + 97.0).abs() < 1e-9);

        let (slat, slon) = gate_latlon(35.0, -97.0, 180.0, 0.0, 10_000.0);
        assert!(slat < 35.0 && (slon + 97.0).abs() < 1e-9);

        let (_, elon) = gate_latlon(35.0, -97.0, 90.0, 0.0, 10_000.0);
        assert!(elon > -97.0);

        let (_, wlon) = gate_latlon(35.0, -97.0, 270.0, 0.0, 10_000.0);
        assert!(wlon < -97.0);

        // Due-east and due-west gates sit at the vertex: they bow slightly
        // equatorward (south in the northern hemisphere), not poleward.
        let (elat, _) = gate_latlon(35.0, -97.0, 90.0, 0.0, 10_000.0);
        let (wlat, _) = gate_latlon(35.0, -97.0, 270.0, 0.0, 10_000.0);
        assert!(elat < 35.0 && wlat < 35.0, "east={elat} west={wlat}");
        // Magnitude is small: ~50 m of bow per 10 km, far less than a gate.
        assert!(35.0 - elat < 5e-4, "east bow too large: {}", 35.0 - elat);
    }

    #[test]
    fn gate_higher_elevation_is_shorter_on_the_ground() {
        let (lat0, lon0) = gate_latlon(35.0, -97.0, 30.0, 0.0, 80_000.0);
        let (lat4, lon4) = gate_latlon(35.0, -97.0, 30.0, 4.0, 80_000.0);
        let d0 = haversine_m(35.0, -97.0, lat0, lon0);
        let d4 = haversine_m(35.0, -97.0, lat4, lon4);
        assert!(d4 < d0, "d0={d0} d4={d4}");
    }
}
