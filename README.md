# stormscope

A fast, native NEXRAD weather-radar viewer for Linux, built in Rust with [egui/eframe](https://github.com/emilk/egui) on
Vulkan (wgpu), native Wayland and X11.

**Free public data only**: NOAA/NWS NEXRAD Level II (via Unidata's and NOAA's
AWS Open Data buckets), `api.weather.gov` alerts/forecasts/observations/storm
reports, SPC convective outlooks and mesoscale discussions, IEM's warning
archive, NOAA's MRMS national mosaic, NASA GIBS satellite imagery, and
OpenStreetMap-derived basemap tiles. No accounts, no API keys, no telemetry.

[![CI](https://github.com/photon01666/stormscope/actions/workflows/ci.yml/badge.svg)](https://github.com/photon01666/stormscope/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Latest release](https://img.shields.io/github/v/release/photon01666/stormscope)](https://github.com/photon01666/stormscope/releases)


## Features

**Radar data**
- Live decoding of NEXRAD Level II message 31 volumes (REF, VEL, SW, ZDR,
  CC, PHI, KDP — KDP derived from the differential-phase field) — wrap
  around `nexrad-data`/`nexrad-decode`, no reimplementation of the binary
  format.
- Live mode (Unidata chunk stream, poller + freshness watchdog) and archive
  mode (calendar day picker, per-day volume windows, paging through the
  whole day), with a WSR-88D site selector.
- Animation ring with a timeline scrubber: play/pause, frame stepping,
  adjustable loop speed, exact per-frame timestamps, and a resident-memory
  budget so an overnight session does not creep.
- Reflectivity filter (min dBZ) in the map's Controls window (scroll over
  the slider to step 5 dB), applied per product.
- A small **(i)** beside the product selector explains every product — what
  REF/VEL/SW/ZDR/PHI/KDP/CC are, how to read VIL, Echo Tops, MEHS and
  storm-relative velocity, and a short reading-tips list.
- Hover **data probe** on the map: the value under the cursor in the gate
  the pixel is in, its beam height (k ft) and distance/bearing from the
  radar (mi), plus the strongest cut in the column for reflectivity. It
  appears only where an echo is actually drawn.
- Optional **3D radar pane**: every elevation cut of the current scan as one
  rotatable volume, beside the map or maximized. Drag orbits, right-drag
  (or W/A/S/D over the pane) pans freely, wheel zooms; the pane's Controls
  window sets the echo floor, opacity, height scale, tilt range, altitude
  slab (feet), mesh detail, and the analysis tools:
  - **Cross-section** — cut a vertical slice through the volume along any
    azimuth (data drawn, no-measurement areas transparent).
  - **Echo shells** — isosurfaces at a *Convective* preset (30 / 50 / 65 dBZ)
    or a *Winter* one (10 / 20 / 30, where snow lives), so a hail core or a
    snowband's structure reads at a glance.
  - **Data probe** — hover the pane for the first and strongest echo under
    the cursor: value, height in k ft, distance in miles, bearing.
  - **Storm motion** sliders for the storm-relative velocity product.
  - **Warning walls** — NWS warning polygons draped on the ground and
    extruded into translucent prisms around the storm (height and fill
    adjustable); they follow the radar on screen: live warnings with live
    radar, and the archive day's polygons only when they were in effect at
    the scan you are looking at.
- **Derived products**, computed at decode so they loop like anything else:
  **VIL**, **Echo Tops** (kft) and **MEHS** (inches) from the reflectivity
  volume, plus **hydrometeor classification** (rain / wet snow / dry snow /
  graupel / hail … from the dual-pol fields), **snowfall rate** (the Z–S
  relation, inches per hour), the **melting level** (the 0 °C level, from the
  correlation-coefficient bright band), and **storm-relative velocity** (SRM)
  from velocity and the storm motion sliders.
- The ground is the same basemap the 2D map draws, and custom
  markers rise from it as pins. Range rings are 50-mile landmarks, like a
  sectional chart.
- Scan-age badge next to the precip/clear-air mode badge: exact age of the
  newest scan with cadence-aware staleness colours.

**Overlays**
- **MRMS national composite**: NOAA's multi-radar, multi-sensor reflectivity
  mosaic (`noaa-mrms-pds`), decoded locally from PNG-packed GRIB2 and drawn
  as a CONUS-wide layer with a native 1 km/px window around the radar on top
  of it. Refreshes every 10 minutes while enabled; its own opacity slider is
  in the map's Controls window. Off by default.
- **GOES satellite** from NASA GIBS: **infrared** (the warm surface is made
  transparent from brightness, so only the cloud field overlays the map) or
  **GeoColor**, from GOES-East or West by site longitude — again CONUS-wide
  plus a sharp radar window, refreshed every 10 minutes. Off by default.
- The **0 °C level** as contour lines every 2 kft over the map (on by
  default; toggle in the map's Controls), and as a hover readout even at a
  gate with no echo.
- SPC Day 1–3 categorical outlooks (fills under the radar, outlines on top)
  with the NWS definition of each risk level — legend in the sidebar, hover
  an outline for the brief explanation, and click it for the full SPC
  forecaster discussion.
- NWS warnings (tornado / severe thunderstorm / flash flood / winter storm,
  ice storm, blizzard, snow squall) drawn as colored polygons — ice blue for
  the winter storm families — plus an alerts browser: every active alert
  nationwide on one tab,
  just the selected station's area on the other, with a search filter. Each
  list groups by event category and reads positions in miles/bearings from
  the station ("42.1 mi SSW of KTWX").
  Hovering a list row — or the polygon on the map itself — glows and labels
  that warning, and a second's rest on a polygon pops a brief description
  under the pointer. Overlapping warnings all light up and are named, reachable
  by their outlines even where one is drawn under the other, and clicking the
  same spot again steps through the stack;
  clicking jumps to it — loading the station that covers it when the alert is
  outside the selected one's range; click a polygon (or the row's Details
  button) for the full NOAA description and safety instruction.
- **Warnings follow the radar**: live polygons only while live radar is on
  screen; in archive mode the selected day's historical polygons are limited
  to the station's coverage and to warnings in effect at the displayed
  scan's time, so scrubbing the loop moves them with the echoes. The alerts
  archive tabs (whole day / station-owned) load that day's warnings, and
  clicking a historical polygon opens its archived NWS text with a
  "load radar at this time" button.
- The top-of-map alerts pill flashes in the issued color (tornado / severe
  thunderstorm / flash flood / winter storm, watches dimmed) when one of those
  watches or warnings is in force near the selected station — checked every 60 seconds
  with a tiny point query, so a new warning lights up within about a minute.
- NWS preliminary **local storm reports** (spotter/public reports) plotted at
  the reported location, and **SPC mesoscale discussions** as areas with the
  full forecaster text on click.
- NWS gridpoint **forecast** from the *Forecast* pill next to the alerts
  badge: the selected station's area by default, or any point you click on
  the map ("Pick on map…"), with day/night periods, the full NOAA narrative,
  current station conditions, and a 15-minute auto-refresh while the window
  is open.
- **Station totals**: rain/snow accumulation from the reporting stations
  around the radar (or any point), labelled on the map with the observation
  window it covers.
- Storm tracks: cell-motion arrows, dashed 30-minute nowcasts, mph labels.
- Per-marker arrival ETAs ("storm reaches this pin in ~14 min") derived from
  smoothed cell motion; velocity estimates blend scan-to-scan so they
  converge instead of jumping.
- Range rings, radar-site dots + city labels, user markers with persistence.
- Measure tool: drag an A–B line (endpoints snap to your markers, then radar
  sites), draggable blue bubble showing statute miles, tap to expand km +
  bearing.

**UI / platform**
- Auto-hiding controls sidebar that slides in on left-edge hover, with a
  pin; the sidebar, its windows and their tooltips scale on high-resolution
  monitors without fighting the OS scale factor.
- Each pane has its own **Controls** window (bottom-right chip) for its
  display settings, and the map has a **GO LIVE** pill (red and pulsing
  while browsing history, green on the newest scan) plus a top-center
  **download pill** with real counts while a bulk load streams.
- Archive day picker as a clickable calendar; the timeline carries exact
  per-frame timestamps and adjustable loop speed.
- Keyboard: **Space** play/pause (pausing jumps to the newest scan),
  **L** go live, **←/→** step frames, **Esc** cancels the measure tool.
  Map: drag pans, wheel zooms; 3D pane: drag orbits, right-drag pans,
  wheel zooms, double-click re-fits.
- Dark basemap synthesized from the light OSM/Wikimedia tiles (hue-preserving
  inversion), on-disk tile cache, dBZ-filtered look tuned for echoes.
- Native Wayland (no XWayland round-trip) with X11 fallback; Vulkan via wgpu.
- Units: the 3D pane and the alerts UI speak **miles and feet**; products use
  their native units (dBZ, m/s, deg/km, kg/m², kft, inches).

**AI meteorologist (optional)**
- A **Meteorologist** window writes a short *now / next 90 minutes* brief for
  every marker on the map, from one compact snapshot of what stormscope
  already knows: the sampled radar column at each pin (surface gates, column
  maximum, VIL / echo tops / MEHS, HCA, snow rate, melting level, SRM,
  velocity extremes), a coarse **echo-region overview** (connected ≥20 dBZ
  areas with centroid, strongest cell, size and distance) so it can see
  features away from the pins, MRMS and satellite pixels, warnings containing
  or near it, tracked cells and their arrival estimates, spotter reports, SPC
  outlook/MCD context, station totals and the NWS forecast. It is decoded
  data — **no screenshot is taken or sent**.
- If a question is about a place with no marker ("is it raining in Wichita?"),
  it can ask the app to **look** there: each look is an **area** (default 20 mi
  radius) returning the strongest echo, its distance/direction, the percentage
  of the area with ≥20 dBZ, the nearest echo and any warnings. The map fits the
  whole searched area, and the model answers from that area summary instead of
  a single gate. If another NEXRAD is materially closer to the area than the
  selected one, the app **switches to it automatically** (a "Switching to
  KICT…" note), loads its volume, and samples from there — a station 140 mi
  away sees the area through a wide, high beam. After the switch it waits for
  a **second scan**, because motion needs two volumes: storm cells then carry
  heading, speed and arrival estimates. If only one scan is available it uses
  explicit motion in warning/MCD text and says when neither exists. The chat
  history is sent to the model as real user/assistant turns, not pasted text.
  Every tracked cell on the scope is included (speed, heading, peak dBZ,
  position) with a vector mean, not only cells within 60 mi of a marker — so
  "which way is the line moving" uses the same storm-track layer you see.
- It can **request missing data**: MRMS, GOES, the NWS forecast, station totals or a
  different SPC day/type. A `DATA:` request makes the app load them (sidebar layers
  included), then the model answers from a second snapshot. Every tilt's gates at each
  sampled column are in the snapshot too, so tilt-specific questions get the real gate.
- The window is edge-resizable and has a **chat height** slider, so the transcript can
  be made as tall or short as you like.
- **Ask it questions** in a chat transcript: every question and answer stays
  in the window (you can scroll back, copy, clear), follow-ups keep the whole
  conversation in context, and a failed call is retried automatically and kept
  as a retryable error instead of losing what you asked.
- It can **show you** rather than just tell you: the reply may carry a
  whitelisted map directive that pans/zooms the 2D map, switches product and
  tilt, opens the 3D pane, sets the camera, cuts a cross-section or shows the
  0 °C level — with one click to restore the previous view. It can also drop
  **labelled circles** on the map (a suspected tornado under a warning, a hail
  core), and marks with an altitude get a pole to that height in the 3D pane —
  for the things only 3D can show, like an updraft top.
- The opener sits at the **bottom-left of the 2D map**; the endpoint is set in
  Settings → AI meteorologist.
- Works with **OpenCode Go** (the popup is pre-filled with
  `https://opencode.ai/inference/go/v1` and `deepseek-v4.1-flash`; paste your
  key), OpenAI, or a local Ollama/LM Studio server. Off until configured, and
  a request is sent only when you press *Write brief* / *Ask*. The snapshot is
  plain JSON — no images, no session history.

## Install

**Prebuilt binary** (Linux x86_64, glibc): grab the asset from the
[latest release](https://github.com/photon01666/stormscope/releases):

```sh
chmod +x stormscope-*-linux-x86_64
./stormscope-*-linux-x86_64
```

**From source** (Rust stable ≥ 1.85 for edition 2024):

```sh
cargo install --path crates/stormscope-app
```

Runtime requirements: Linux with working Vulkan drivers (Mesa/Intel/AMD/NVIDIA)
and a Wayland or X11 session. No other system packages beyond a normal desktop
stack.

## Performance notes

The app is written so a full radar frame never lands on the UI thread and the
expensive raster work happens only when something actually changes:

- Gate geolocation is cached as *zoom-independent* world coordinates and
  reused across pans, zooms, moment switches and playback frames; rotated
  scan layouts, measured-elevation jitter and a dropped axis ray are handled
  by remapping rows instead of rebuilding (the rebuild is ~90 ms, so a cache
  miss per playback frame used to be the stutter).
- Drags, playback and wheel-zoom bursts rasterize at half resolution with a
  full-resolution pass when the gesture stops.
- The 3D pane supersamples 1.6x when settled (egui downsamples linearly —
  anti-aliased edges and no tile shimmer) and draws at exact display
  resolution while the camera moves: never below it, because sub-pixel grid
  lines flicker as their coverage flips frame to frame.
- The 3D pane renders on demand: only when its camera, settings, volume,
  ground tiles, cross-section or size changed. One sharp (supersampled) frame
  lands 160 ms after the motion stops — idle wakes cost a texture sample
  instead of a volume pass (the policy is a unit-tested pure function).
- The 3D scene follows playback: meshes build on a worker thread and upload
  when newer than what is displayed (a frame one behind is the intended
  behaviour), so playing the loop can never starve map panning or 3D
  orbiting; scrub-dragging rebuilds at most every 250 ms.
- Echo shells (isosurfaces) build on that same worker; the cross-section
  curtain is a prepared-profile sweep rebuilt in ~2 ms on the UI thread, and
  both ride a revision in the render key so toggling them redraws at once.
- Derived products (VIL / Echo Tops / MEHS, hydrometeor classification, snow
  rate, melting level) are computed once at decode on the decode thread,
  threaded over ray chunks: ~45 ms on a real volume and ~4 % memory, after
  which switching products and looping cost nothing.
- MRMS and GOES work runs on the fetch/decode workers (GRIB2 parse, PNG
  decode, crop, WMS fetch); the UI thread only builds a texture when a new
  frame arrives, and pans/zooms just redraw the cached one.
- Splats are premultiplied, division-free, into a persistent buffer uploaded
  through one reused GPU texture.
- Measured on the development machine (Intel Arc A310, 2540×1329 viewport): heavy
  echo-dense motion frames ~10 ms, geometry rebuilds once per scan shape.
- Memory: the animation ring has a resident-memory budget (oldest-first
  eviction, ≥ 8 frames retained), the tile texture cache is bounded, and the
  allocator is tuned to return decode-sized buffers to the OS — an overnight
  session sits near 2 GB instead of creeping.
- Basemap tiles: per-provider circuit breaker (a 403/429 server is skipped
  for a cooldown and the last working provider is preferred) so one blocked
  tile server cannot stall the map.

Run with `RUST_LOG=debug` to see per-raster timings and the 10-second
memory/texture accounting.

## Data sources & attribution

| Source | Used for | Terms |
| --- | --- | --- |
| [Unidata NEXRAD Level II](https://registry.opendata.aws/noaa-nexrad/) (`unidata-nexrad-level2-chunks`, `noaa-nexrad-level2`) | Volume scans, live chunks | NOAA public-domain data |
| [MRMS](https://www.nssl.noaa.gov/projects/mrms/) (`noaa-mrms-pds`) | National multi-radar reflectivity composite | NOAA public-domain data |
| [NASA GIBS](https://www.earthdata.nasa.gov/engage/open-data-services-software/earthdata-developer-portal/gibs) | GOES-East/West imagery (infrared, GeoColor) | NASA/NOAA; public data |
| [`api.weather.gov`](https://www.weather.gov/documentation/services-web-api) | Active alerts, forecast gridpoints, station observations, local storm reports, radar station list | US Government work; a descriptive `User-Agent` is required and sent |
| [IEM](https://mesonet.agron.iastate.edu/) (Iowa Environmental Mesonet) | Archived warning polygons (VTEC day files) and archived NWS product text | Iowa State University; public data, requested politely (rate-limited, cached) |
| [SPC](https://www.spc.noaa.gov/) | Day 1–3 categorical outlooks, mesoscale discussions | US Government work |
| [Wikimedia Maps](https://maps.wikimedia.org/) (`osm-intl`), with [OpenStreetMap](https://www.openstreetmap.org/) fallback | Basemap tiles | © OpenStreetMap contributors, ODbL — attribution is drawn on the map |
| [OpenCode Go](https://opencode.ai/v2/docs/console/go/) (optional) | AI meteorologist brief and follow-up answers | Your own subscription/key; the JSON snapshot goes only to the endpoint you configure |

Requests are anonymous, cached on disk, and rate-limited; there is no
telemetry. Tiles are fetched at low volume and the dark theme is synthesized
locally from the light tiles. The optional AI brief is the one request that
carries a credential; it is sent only when you press the button, and the key
stays on your machine (environment variable or `~/.config/stormscope/ai.json`,
mode 0600).

## Architecture

```
crates/
  radar-core/       data model: volumes, sweeps, moments, VCP, sites, warnings,
                    outlooks, forecasts; volume sampling; derived products
  radar-decode/     NEXRAD Level II bytes -> radar_core::Volume (archive + realtime);
                    KDP derived from differential phase
  radar-data/       S3 fetch/cache/live poll; NWS alerts, forecasts, observations,
                    LSRs; SPC outlooks and MCDs; IEM archived warnings; MRMS
                    mosaic and GOES imagery
  radar-render/     projection, color tables, rasterization, tiles, storm tracks,
                    3D volume geometry, cross-sections, isosurfaces
  stormscope-app/   eframe app: state, timeline, panes, wiring
```

Heavy work (fetch, decode, rasterize, 3D meshes, derived products) runs on
background threads and communicates over channels; the UI polls state each
frame. All times are UTC internally, all coordinates WGS84 degrees.

## Status & roadmap

Single-site radar is solid; the interesting gaps are:

- Configurable color tables / palettes.
- Packaging (AUR, Flatpak) and a proper `.desktop` entry.
- Windows/macOS are untested (the crate is wgpu-based, so it should port).

Release history lives in [CHANGELOG.md](CHANGELOG.md).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Issues and PRs welcome — bug reports
with the app's `RUST_LOG=debug` output are especially useful.

## License

[MIT](LICENSE). Third-party license inventory (all permissive) in
[docs/THIRD-PARTY.md](docs/THIRD-PARTY.md).
