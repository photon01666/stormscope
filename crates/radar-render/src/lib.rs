//! radar-render — projection, color tables, sweep rasterization, tile map.
//!
//! Workstream W3. See PLAN.md. Public API:
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

pub use colormap::{ColorTable, Stop};
pub use projection::{
    gate_latlon, lonlat_to_merc_px, meters_per_pixel, merc_px_to_lonlat, MercProjector, MapView,
};
pub use raster::{rasterize_sweep, rasterize_sweep_into, RadarImage};
pub use tiles::{tile_range, tile_scale, tile_view_rect, TileKey, TileLayer, TileRange};
pub use tracks::{compute_tracks, extrapolate, StormTrack};
