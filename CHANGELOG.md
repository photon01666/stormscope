# Changelog

Notable changes per release. The project's early history lives in the commit
log; entries start at the first public release.

## v1.2.0 — 2026-10-16

**3D radar pane (new)**
- The map gained an optional 3D companion: every elevation cut of the
  current scan as one rotatable volume — beside the 2D map (resizable split,
  draggable divider) or maximized to fill the window. "Max"/"Split" chips
  live on the panes, "Fit" re-frames the camera; drag orbits, wheel zooms,
  double-click re-fits.
- The geometry is honest: each cut is a draped surface over its azimuth ×
  range grid at true 4/3-earth beam heights (a 0.5° beam is ~1.5 km up at
  100 km), and no-echo gaps are skipped rather than smeared across. Height
  scale is the readability knob (default ×6, range ×1–×20) because real
  storms are ~15 km tall over a 300 km view — 1:20 on paper.
- Units are US radar-display units: range rings every 50 mi, the altitude
  scale and the altitude slab in feet, echo floors in the moment's unit.
- The pane's bottom center carries the three quick sliders (echo floor,
  opacity, height scale — the wheel adjusts the one under the pointer); the
  sidebar carries tilt range (e.g. only 0.5–1.8°, or one cut alone),
  altitude slab (e.g. only echo above 10,000 ft), mesh detail, and it
  follows the map's moment and timeline playhead.
- The ground is the same basemap the 2D map draws — the same tiles out of
  the same disk cache and fetch queue — so the volume reads against real
  geography. Custom markers rise from it as labeled pins.
- Rendered by a wgpu offscreen pass on the UI's own device (depth buffer,
  premultiplied blending, colors straight from the 2D colormap — the two
  views never disagree about what a color means). The mesh builder is a
  pure module in radar-render under unit test, and a headless smoke test
  renders a synthetic supercell to a PNG.

## v1.1.7 — 2026-10-16

**Storm reports (new layer)**
- Preliminary Local Storm Reports from the NWS, plotted where each product
  places them: event-family colors, a white ring for trained-spotter
  reports, a 12-hour window polled every 5 minutes, hover for the brief and
  click for the full product text. The parser reads the product's own
  fixed-width columns (measured against real reports, two-word counties
  included) and the verbatim latitude/longitude it carries.

**Mesoscale discussions (new layer)**
- SPC MDs as amber areas with "MD n" tags: hover for area, validity and
  watch chance; click for the full discussion. RSS-driven (none shown when
  none are active), polygon decoded from the packed `LAT...LON` line —
  verified against the product's own WFO list.

**SPC outlook types**
- The outlook layer gained the probabilistic products: Categorical /
  Tornado / Wind / Hail, days 1–2 (day 3 has none upstream and disables
  them; picking day 3 falls back to categorical). SPC's own colors and
  wording ("chance of a tornado within 25 miles of a point"), a separate
  probability legend, and the 10-minute refresh follows the product on
  screen.

**Rain & snow totals (new)**
- A totals window and a map overlay: the stations inside the selected
  site's viewing area (400 km, geometry-checked), their rolling buckets
  (rain last hour / 3 / 6 h, snow depth, …) and an hourly-summed
  accumulation — sampled ≥50 minutes apart so a 5-minute station's
  overlapping windows do not double-count. On the map: diamonds tagged
  with each station's strongest number (❄ when it is frozen), hover for the
  table, click for the window.

**Fixes**
- Zooming no longer blanks the radar. Under the gesture throttle a stale
  raster used to be skipped when the zoom had moved ("translation would be
  scale-wrong"), which flashed the layer off 10–46 times a second while
  zooming; it is now drawn georeferenced — a pan is a translation, a zoom
  is the mercator scale — and the fresh raster lands within 100 ms.
- The gesture throttle now covers every view gesture (100 ms; drags keep
  150 ms; playback's 8 fps is untouched): measured wheel-zoom churn of
  16–46 rasters/s is capped at ~10/s, and the churn warning only trips
  above that cap.

**CI**
- GitHub Actions now gate every push: rustfmt, workspace check, release
  build and the test suite; clippy runs informational with the style-lint
  backlog documented. The live S3 tests (archive download, decode
  fixtures, realtime poller) are ignored by default so the gating job is
  hermetic — run them locally with `--ignored`.

## v1.1.6 — 2026-10-16

**Presentation and diagnostics**
- Presentation is pinned to vsync (`Fifo`) instead of eframe's `AutoVsync`.
  The auto mode renegotiates on every display reconfigure, and a monitor that
  has been asleep can come back without the same capability list: the
  fullscreen map then presents uncapped, the shared GPU is pinned, and the
  whole session feels like it dropped to half the refresh rate — dragging,
  zooming, and the DisplayPort audio that rides the same GPU. That is what
  returning to a machine whose monitors had been off ended in, until the app
  was restarted. The mode is no longer left to negotiation (and a test keeps
  it that way).
- Frame-cadence telemetry: a debug line every 15 s with the frame rate,
  average/max frame time, radar raster count and viewport size, plus an INFO
  line when the monitor scale changes — so "the machine feels half-rate" is
  measurable from the log next time it happens.
- A rate-limited warning if idle radar rasters exceed 15/s: each raster is
  also a texture upload, so an oscillating signature would otherwise become a
  per-frame cost with nothing naming it.

## v1.1.5 — 2026-10-15

**SPC outlooks**
- Risk categories now carry their NWS wording: the sidebar legend lists each
  category present (most severe first, in SPC's own colors) with what it
  means — "Enhanced Risk (3/5) — Numerous severe storms possible." — plus the
  valid-until window. Hovering an outline draws that category bolder, and
  clicking an outline opens the full SPC forecaster discussion for the day
  (plain-text product from spc.noaa.gov, WMO header trimmed), headed by the
  clicked risk level's NWS wording, forecaster name and validity window, with
  a reload button. The brief hover tooltip keys off the *border* only — the
  emphasis, the tooltip and the click share one 8 px reach — so the risk areas
  covering most of the map never pop text while the pointer is merely inside
  one.

**Warning latency**
- Watches and warnings now reach the screen within about a minute of
  issuance. The five-minute sweep around the station (a 3.1 MB nationwide
  listing, filtered locally) stays as the broad set, and a new *fast lane*
  asks the API for alerts that apply to the station itself (`?point=`, a few
  hundred bytes) every 60 seconds — roughly 10 KB/hour. New warnings appear
  in the map polygons, the browser list and the badge flash as soon as that
  poll sees them, and a zone-based warning (no polygon, so no centroid) can
  now flash the badge too, which the proximity filter could never do.

**Overlapping warnings**
- Two warnings on the same ground no longer hide each other. The map hit test
  returns an ordered stack instead of a single polygon: a polygon outline
  within 8 px of the pointer comes first (nearest edge wins), then the
  containing polygons smallest first. The warning drawn *underneath* is
  therefore reachable wherever its border runs under the other one — the case
  that used to be untouchable.
- Every warning in a stack glows and is named while the pointer is on it, and
  the leading one glows at full hovering strength, because that is the one a
  click opens. Clicking the same spot again steps to the next warning in the
  stack (wrapping around) and the tooltip names what else is stacked there;
  clicking anywhere else starts over.
- Polygon fills are much lighter (5–22% toward the outline's color, was
  12–42%), so overlapping warnings no longer multiply into an opaque wash over
  the radar — the border carries the color. Event labels now appear only while
  the pointer is on a polygon: a clicked alert keeps its afterglow without
  leaving a name floating on the map.
- That tint is adjustable now: a **fill slider** sits in Settings → Map
  appearance and in the alerts browser header (0% = outlines only, 100% = the
  shipped wash).

**Popup resizing**
- Grabbing a popup's bottom border no longer yanks it upward. egui derives the
  resize drag's base rectangle from the *window* while treating it as a
  *content* size, so any vertical space a popup was offered and did not use
  (a scroll area capped below its space, a footer row using less than the
  rest) made the first drag frame jump by exactly that gap — a 50 px gap, a
  50 px jump — leaving the pointer grabbing thin air. Popups now pad the
  leftover so they are exactly as tall as they look, and a headless test
  drives synthetic pointer input at the border to keep it that way.

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
- Clicking a row centers the map on that alert (zoom unchanged) and flashes
  the polygon briefly, then leaves it highlighted. Hovering a row glows its
  polygon live and labels it with the event name on the map; pointing at a
  polygon on the map itself gives the same glow + label (slightly gentler)
  with a pointer cursor, so a warning can be identified either way round.
  Resting on a polygon for a second pops a brief tooltip under the pointer —
  event, issuing office, expiry, distance, affected area and the opening
  sentence of the NOAA text — and the list rows show that same brief card on
  hover.
- Full NOAA/CAP text: every alert carries `description` (and `instruction`
  where NOAA provides it — safety guidance such as "TAKE COVER NOW"). The
  browser's **Details** button, and clicking a warning polygon on the map,
  open a popup with the full text, severity/urgency, affected areas and
  expiry.
- Popups are positioned by their top-left corner instead of an egui
  *pivot*: with a center pivot, every size change also shifts the stored
  position by half the delta, which fights the resize drag (a downward drag
  could snap the window back). They still open centered, and dragging them
  works like any normal window.
- Popups resize smoothly: the SPC discussion is no longer re-wrapped while
  the window edge is dragged (the text product is pre-formatted, so it is
  laid out once and scrolls in both directions), scrollbars are always
  reserved so growing/shrinking cannot make a bar appear and disappear, the
  windows carry a minimum size so they cannot be dragged into an overflow
  state, and the SPC window no longer uses an explicit constrain rect (which
  could make it jump when the clamp engaged mid-resize).
- Popups (alerts, NOAA details, forecast, SPC discussion) open at a size
  scaled for the monitor and clamped to the display, so a long discussion or
  alert list cannot push a window off the bottom edge — and they stay freely
  resizable: drag a popup taller and its list/text area grows with it (the
  height cap is the screen, not the opening size). The SPC discussion is laid
  out as a summary card (level, wording, forecaster, validity) over a framed,
  scrollable text block.
- High-resolution scaling follows the monitor for the sidebar, the browser /
  forecast / detail windows *and their tooltips* (tooltip layers do not
  inherit per-panel scaling, so the hover card gets the scale explicitly); the
  map's own painted labels keep their radar-appropriate sizes.
- Jumping to an alert far from the selected station (from the All-US tab) also
  loads the station that covers it: radar frames and the near-site alert set
  are per station, so before this the map showed neither the warning polygon
  nor radar after the jump. The switch only happens when another station is
  nearer *and* actually covers the alert (offshore or out-of-network alerts
  just pan); the zoom is raised to a station-scale view if the map was far
  out, so the warning is visible once the data lands.
- The per-row **Details** button works (the row's click region used to sit
  above it in egui's hit order and swallow its clicks).

**NWS forecast and current conditions**
- New *Forecast* pill to the right of the alerts badge in the top-center row.
  It opens the NWS gridpoint forecast (`api.weather.gov` `points` → gridpoint
  forecast) for the selected station's area — one click, no setup.
- The window can instead forecast any point: "Pick on map…" arms a mode where
  the next map click becomes the forecast point (labeled with its
  coordinates), with a "station" tab to go back, and a Refresh button. A
  radar site's own grid point is often rural (KTWX resolves to Alma, KS), so
  the picked point covers wherever you actually care about.
- Periods render as day/night sections (temperature, short forecast, wind)
  with the detailed NOAA narrative inside; the header shows the issuing
  office, grid and update time.
- The forecast and current conditions refresh themselves every 15 minutes
  while the window is open (four small requests per cycle: gridpoint lookup,
  forecast, station list, latest observation) with a "Refresh now" button for
  impatience. The NWS API is open data — its own documentation says "free to
  use for any purpose" with "reasonable rate limits ... a generous amount for
  typical use", and that requests directly from clients are not likely to
  reach them — so this cadence is comfortably within the terms. Upstream,
  observations report about hourly and gridpoint forecasts update several
  times an hour, so 15 minutes keeps up without pointless traffic.
- Current conditions for the same point, from the nearest reporting station
  (`/points` → station list → latest observation): text description,
  temperature, dewpoint, humidity, wind with compass direction and gusts,
  pressure, visibility and last-hour precipitation in US units, plus the
  observation time and its age — ASOS/AWOS stations report roughly hourly, so
  the age matters. Weather Underground's API is paywalled (no free tier since
  the IBM/TWC takeover), so this uses the free NWS observations instead.
  Observation failures are non-fatal: not every point has a close station.

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
