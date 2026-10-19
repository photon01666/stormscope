# stormscope

A fast, native NEXRAD weather-radar viewer for Linux, built in Rust with [egui/eframe](https://github.com/emilk/egui) on
Vulkan (wgpu), native Wayland and X11.

**Free public data only**: NOAA/NWS NEXRAD Level II (via Unidata's and NOAA's
AWS Open Data buckets), `api.weather.gov` alerts/forecasts/observations/storm
reports, SPC convective outlooks and mesoscale discussions, IEM's warning
archive, and OpenStreetMap-derived basemap tiles. No accounts, no API keys, no
telemetry.

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
  - **Echo shells** — 30 / 50 / 65 dBZ isosurfaces, so a hail core's shape
    reads at a glance.
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
  volume, plus **storm-relative velocity** (SRM) from velocity and the storm
  motion sliders.
- The ground is the same basemap the 2D map draws, and custom
  markers rise from it as pins. Range rings are 50-mile landmarks, like a
  sectional chart.
- Scan-age badge next to the precip/clear-air mode badge: exact age of the
  newest scan with cadence-aware staleness colours.

**Overlays**
- SPC Day 1–3 categorical outlooks (fills under the radar, outlines on top)
  with the NWS definition of each risk level — legend in the sidebar, hover
  an outline for the brief explanation, and click it for the full SPC
  forecaster discussion.
- NWS warnings (tornado / severe thunderstorm / flash flood) drawn as colored
  polygons, plus an alerts browser: every active alert nationwide on one tab,
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
  thunderstorm / flash flood, watches dimmed) when one of those watches or
  warnings is in force near the selected station — checked every 60 seconds
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
- Derived products (VIL / Echo Tops / MEHS) are computed once at decode on
  the decode thread, threaded over ray chunks: ~45 ms on a real volume and
  ~4 % memory, after which switching products and looping cost nothing.
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
| [`api.weather.gov`](https://www.weather.gov/documentation/services-web-api) | Active alerts, forecast gridpoints, station observations, local storm reports, radar station list | US Government work; a descriptive `User-Agent` is required and sent |
| [IEM](https://mesonet.agron.iastate.edu/) (Iowa Environmental Mesonet) | Archived warning polygons (VTEC day files) and archived NWS product text | Iowa State University; public data, requested politely (rate-limited, cached) |
| [SPC](https://www.spc.noaa.gov/) | Day 1–3 categorical outlooks, mesoscale discussions | US Government work |
| [Wikimedia Maps](https://maps.wikimedia.org/) (`osm-intl`), with [OpenStreetMap](https://www.openstreetmap.org/) fallback | Basemap tiles | © OpenStreetMap contributors, ODbL — attribution is drawn on the map |

Requests are anonymous, cached on disk, and rate-limited; there is no
telemetry. Tiles are fetched at low volume and the dark theme is synthesized
locally from the light tiles.

## Architecture

```
crates/
  radar-core/       data model: volumes, sweeps, moments, VCP, sites, warnings,
                    outlooks, forecasts; volume sampling; derived products
  radar-decode/     NEXRAD Level II bytes -> radar_core::Volume (archive + realtime);
                    KDP derived from differential phase
  radar-data/       S3 fetch/cache/live poll; NWS alerts, forecasts, observations,
                    LSRs; SPC outlooks and MCDs; IEM archived warnings
  radar-render/     projection, color tables, rasterization, tiles, storm tracks,
                    3D volume geometry, cross-sections, isosurfaces
  stormscope-app/   eframe app: state, timeline, panes, wiring
```

Heavy work (fetch, decode, rasterize, 3D meshes, derived products) runs on
background threads and communicates over channels; the UI polls state each
frame. All times are UTC internally, all coordinates WGS84 degrees.

## Status & roadmap

Single-site radar is solid; the interesting gaps are:

- Multi-site mosaics / composites.
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
