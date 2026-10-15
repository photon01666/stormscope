# Changelog

Notable changes per release. The project's early history lives in the commit
log; entries start at the first public release.

## v1.1.0 — 2026-10-15

**Alerts browser**
- New popup listing active NWS alerts, opened from the sidebar or by clicking
  the alert count in the status bar.
- Two tabs: every active alert nationwide, or only those whose polygon lies
  within 400 km of the selected radar site (the same radius the map's alert
  fetch uses).
- Search box matching event, affected area, issuing office and headline;
  rows show severity color (matching the map polygons), WFO, expiry in local
  time, distance from the site, and the affected-area text. Zone-only alerts
  are labeled because they have no polygon to center on.
- Clicking a row centers the map on that alert (zoom unchanged).

## v1.0.10 — 2026-10-15

First release.

**Radar**
- NEXRAD Level II decoding for REF, VEL, SW, ZDR, CC, PHI, KDP (message 31
  data blocks, per-moment calibration).
- Live mode via Unidata's real-time chunk stream, with a freshness watchdog;
  archive mode with a date picker and per-day volume windows.
- Animation ring + timeline scrubber: play/pause, frame stepping, fps control.
- Min dBZ (REF) filter in the sidebar and as a floating quick-adjust bar on
  the map (5 dB per wheel notch).

**Overlays**
- SPC Day 1–3 outlooks (fills under radar, outlines on top), NWS warning
  polygons with details, storm tracks with 30-minute nowcasts, per-marker
  storm ETAs, range rings, site/city labels, persistent user markers,
  RadarScope-style measure tool with a draggable blue bubble.

**UI**
- Auto-hiding controls sidebar (edge hover, pin, monitor-aware HiDPI scaling).
- Scan-age badge with cadence-aware staleness colours.
- Dark basemap synthesized from OSM/Wikimedia light tiles, disk-cached.

**Performance & stability**
- Zoom-independent gate-geometry cache (rotated scan layouts remap rows),
  half-resolution motion rasters with a full-resolution settle pass,
  premultiplied division-free splats, reused GPU texture and scratch buffer.
  Heavy echo-dense motion frames went from ~163 ms to ~10 ms on the
  development box.
- Animation ring bounded by a resident-memory budget; tile texture cache
  bounded; allocator tuned to return decode buffers — overnight footprint
  ~2 GB instead of ~5 GB.
- Tile providers get a 403/429 circuit breaker (skip + last-good preference).

**Fixes**
- Radar georeferencing during pan/zoom (motion-raster scaling and stale-texture
  translation).
- Archive day window now spans midnight Central, not midnight UTC.
- Workspace moved to Rust edition 2024; warning-free build.
