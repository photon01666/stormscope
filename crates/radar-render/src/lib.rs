//! radar-render — projection, color tables, sweep rasterization, tile map.
//!
//! Public API:
//!
//! ```ignore
//! pub struct MapView { center_lat, center_lon, zoom, width_px, height_px }
//! pub fn lonlat_to_merc_px(...); pub fn merc_px_to_lonlat(...);
//! pub fn gate_latlon(...) -> (lat, lon);          // 4/3-earth model
//! pub struct ColorTable; impl ColorTable { pub fn for_moment(m: Moment) -> Self; pub fn map(&self, v) -> [u8;4]; }
//! pub fn rasterize_sweep(...) -> RadarImage;      // owns a fresh buffer
//! pub fn rasterize_sweep_into(..., buf: &mut [u8]); // composite in place
//! pub struct TileLayer; impl TileLayer { pub fn paint(&mut self, ui, view) -> egui::Response; }
//! ```
//!
//! Layer alignment contract: every layer derives screen positions from
//! `MapView::origin_px` / `MapView::lonlat_to_view_px`. `TileLayer::paint`
//! returns a `Response` whose `rect.min` is that origin in screen space, so
//! the app paints overlays at `response.rect.min + view_px`.

pub mod colormap;
pub mod projection;
pub mod raster;
pub mod tiles;
pub mod tracks;
pub mod volume3d;

pub use colormap::{ColorTable, Stop};
pub use projection::{
    MapView, MercProjector, gate_latlon, initial_bearing_deg, lonlat_to_merc_px, merc_px_to_lonlat,
    meters_per_pixel, surface_distance_m,
};
pub use raster::{
    GateGeometry, RadarImage, RowMap, rasterize_sweep, rasterize_sweep_from_geometry_pm,
    rasterize_sweep_into, rasterize_sweep_premultiplied, rasterize_sweep_premultiplied_into,
    scaled_view,
};
pub use tiles::{TileKey, TileLayer, TileRange, tile_range, tile_scale, tile_view_rect};
pub use tracks::{CellEta, StormTrack, compute_tracks, eta_to_point, extrapolate};
pub use volume3d::{
    Camera, FT_PER_KM, KM_PER_MILE, MeshDetail, MeshFilter, MeshVertex, NO_THRESHOLD_VALUE,
    TiltDraw, VolumeMesh, beam_height_km, build_ground_grid, build_volume_mesh,
    ground_tile_bounds_km, ground_tile_keys, ring_radii_mi,
};
