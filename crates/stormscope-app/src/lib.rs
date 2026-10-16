//! stormscope — app shell.
//!
//! Layout: auto-hiding controls sidebar (site, product, tilt, opacity, loop
//! controls) sliding in over the central map on left-edge hover,
//! central map canvas (basemap tiles + radar overlay + warning polygons),
//! bottom timeline scrubber, top status bar.
//!
//! This module owns UI + state wiring only; heavy work (fetch, decode,
//! rasterize) happens on background threads whose results are polled each frame.

mod app;
mod background;
pub mod render3d;
mod state;
mod timefmt;
mod timeline;
mod ui;

pub use app::StormApp;
