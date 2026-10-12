//! stormscope — app shell (W4, owned by lead).
//!
//! Layout: left control panel (site, product, tilt, opacity, loop controls),
//! central map canvas (basemap tiles + radar overlay + warning polygons),
//! bottom timeline scrubber, top status bar.
//!
//! This module owns UI + state wiring only; heavy work (fetch, decode,
//! rasterize) happens on background threads whose results we poll each frame.

mod app;
mod background;
mod state;
mod timeline;
mod ui;

pub use app::StormApp;
