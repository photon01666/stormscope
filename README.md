# Stormscope

A fast native NEXRAD Level II weather-radar for Linux and Windows, built
in Rust with [egui/eframe](https://github.com/emilk/egui) on wgpu (Vulkan,
Wayland/X11 on Linux and Android, DX12 on Windows).

Free public data only: NEXRAD Level II (Unidata and NOAA AWS Open Data),
`api.weather.gov`, SPC, IEM, NOAA MRMS, NASA GIBS, GOES GLM lightning and
OpenStreetMap tiles.
No accounts, API keys or telemetry.

[![CI](https://github.com/photon01666/stormscope/actions/workflows/ci.yml/badge.svg)](https://github.com/photon01666/stormscope/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Latest release](https://img.shields.io/github/v/release/photon01666/stormscope)](https://github.com/photon01666/stormscope/releases)



## Features

**Radar**
- Live chunk stream and archive mode (calendar, paging, timeline scrubber).
- The timeline exposes **every cached scan for the selected site** — hover for
  the exact date/time and size, click to load that day straight from the cached
  files (no downloads). **Downloaded…** opens the full day-by-day history with
  Start/Mid/End and earlier/later 24-scan windows, plus a Delete action per day
  (with confirmation). Archive (S3) is unchanged.
- REF, VEL, SW, ZDR, PHI, KDP, CC; derived VIL, Echo Tops, MEHS, SRM and the
  winter products HCA, snow rate and melting level.
- Hover probe: gate value, beam height (k ft), distance/bearing, column max.
- Product explainer, Min dBZ filter, animation ring with a memory budget.

**3D radar**
- Every elevation cut as one rotatable volume beside or instead of the map;
  orbit, free pan, wheel zoom.
- Cross-sections, echo-shell isosurfaces (convective or winter), warning
  prisms, markers and a data probe.
- The 3D Controls window has its **own product selector**, independent of the
  2D map (REF on the map, VEL/HCA/SRM in the volume, for example).
- Ground scale matched to the 2D map; altitude slab and height scaling.

**Overlays**
- SPC Day 1–3 outlooks (categorical, Tornado/Wind/Hail) with risk-level help.
- NWS warnings (ice-blue winter family), alerts browser, local storm reports,
  mesoscale discussions, NWS forecast, station rain/snow totals.
- The forecast window's current conditions carry the full surface observation:
  air temperature, dew point, relative humidity, wind speed/direction/gusts,
  altimeter and sea-level pressure, 1/3/6-hour precipitation, visibility,
  sky cover and cloud bases, heat index/wind chill, the current UV index
  (Open-Meteo) and an animated moon-phase glyph. Numbers tween when data
  refreshes, and forecast temperatures animate on update.
- Storm tracks with nowcasts and per-marker arrival ETAs; measure tool;
  range rings.
- MRMS national composite (CONUS + native 1 km window) and GOES infrared or
  GeoColor (CONUS + sharp window), on demand.
- **GOES GLM lightning**: official NOAA flashes for the last 20 minutes (one
  20-second file per minute, ~0.4 GB/day while enabled; off by default).
- 0 °C level as isohypses every 2 kft (default on).

**AI meteorologist (optional)**
- OpenCode Go (`deepseek-v4.1-flash` by default) or any OpenAI-compatible
  endpoint; key in Settings → AI meteorologist.
- Short per-marker briefs and a persistent chat. It can look at other areas
  (`LOOK`), fetch MRMS/GOES/forecast/totals/SPC (`DATA`), and move the 2D/3D
  views or mark features — including altitude poles in 3D.
- Off until configured; only the decoded JSON snapshot is sent, never a
  screenshot.

**UI**
- Auto-hiding sidebar, per-pane Controls windows, timeline and GO LIVE pill.
- Dark OSM/Wikimedia basemap with on-disk cache; miles and feet in the 3D pane
  and alerts.
- Keyboard: Space play/pause, L live, ←/→ step, Esc cancels measuring.

**Voice alerts**
- New warnings and watches are read aloud when their polygon is within
  **50 miles of the GPS fix or a manual map marker**.
- Desktop builds use the neural **Piper** voice (female lessac high by
  default, with a voice picker in Settings); Android uses the system
  text-to-speech engine. The Windows CI artifact is a single
  **`Stormscope.exe`** with Piper and the voice embedded; it extracts them to
  `%LOCALAPPDATA%\stormscope\piper` on first launch. Settings has a master
  switch and separate tornado, severe thunderstorm, watch and other-warning
  switches.
- Spoken desktop alerts are triggered from the background data thread, so a
  window on another Wayland workspace (which may stop receiving frame
  callbacks) still announces warnings out loud.
- Android: an opt-in **Storm Watch** foreground service keeps a persistent
  notification and speaks new warnings with its own TTS while Stormscope is
  closed, polling the last GPS fix every 60 seconds. Toggle it from Settings -> Voice alerts.

## Install

All three builds ship with the
[latest release](https://github.com/photon01666/stormscope/releases):

- `stormscope-<version>-linux-x86_64` — x86_64 Linux binary
- `stormscope-<version>-windows-x86_64.exe` — Windows 10/11, a single file
  with the Piper neural voice embedded (first launch extracts it to
  `%LOCALAPPDATA%\stormscope\piper`)
- `stormscope-<version>-android.apk` — Android 8+, arm64-v8a (sideload;
  versionCode 2)

CI also uploads a `stormscope-windows-x86_64` artifact on every green run.

Linux:

```sh
chmod +x stormscope-*-linux-x86_64
./stormscope-*-linux-x86_64
```

From source (Rust stable ≥ 1.85, edition 2024):

```sh
cargo install --path crates/stormscope-app
```

Linux needs working Vulkan drivers and a Wayland or X11 session. For the
neural spoken alerts, install Piper and an English voice (for example
`piper-tts` and `piper-voices`, or `pip install piper-tts` and a voice from
[the Piper voices collection](https://huggingface.co/rhasspy/piper-voices));
without Piper, Stormscope falls back to the system TTS. On Windows the
renderer uses DX12: install the MSVC build tools (Desktop development with
C++) plus CMake and NASM (the TLS crates need them), then
`cargo build --release -p stormscope-app` writes
`target\release\stormscope.exe`. The CI Windows artifact is a single
`Stormscope.exe` with Piper and lessac embedded; a local build uses the
system SAPI voice unless Piper is placed in `%APPDATA%\stormscope\piper`.

## Data sources

| Source | Used for |
| --- | --- |
| [Unidata/NOAA NEXRAD Level II](https://registry.opendata.aws/noaa-nexrad/) | Volume scans, live chunks |
| [`api.weather.gov`](https://www.weather.gov/documentation/services-web-api) | Alerts, forecasts, observations, storm reports |
| [IEM](https://mesonet.agron.iastate.edu/) | Archived warning polygons and NWS text |
| [SPC](https://www.spc.noaa.gov/) | Outlooks and mesoscale discussions |
| [NOAA MRMS](https://www.nssl.noaa.gov/projects/mrms/) | National multi-radar composite |
| [NASA GIBS](https://www.earthdata.nasa.gov/engage/open-data-services-software/earthdata-developer-portal/gibs) | GOES-East/West imagery |
| [NOAA GOES GLM](https://www.goes-r.gov/spacesegment/glm.html) (`noaa-goes18/19`) | Lightning flashes (last 20 min) |
| [Wikimedia/OpenStreetMap](https://www.openstreetmap.org/) | Basemap tiles (© OSM contributors, ODbL) |

Requests are anonymous, cached on disk and rate-limited. The optional AI brief
is the only request carrying a credential, and only when you press the button.

## Architecture

```
crates/
  radar-core/     data model, sampling, derived products
  radar-decode/   NEXRAD Level II -> Volume
  radar-data/     S3/NWS/SPC/IEM/MRMS/GOES fetchers and caches
  radar-render/   projection, color tables, raster, 3D, cross-sections
  stormscope-app/ eframe app, state, panes, AI
```

Fetch, decode, raster and mesh work run on background threads; the UI polls
state each frame. Times are UTC internally, coordinates WGS84.

## Performance notes

- Gate geolocation is cached across pans, zooms, products and playback frames.
- Drags and wheel bursts rasterize at half resolution; the settled frame is full.
- The 3D pane renders on demand and supersamples when settled; meshes build on
  a worker thread.
- Derived products cost ~45 ms at decode and ~4 % memory; the animation ring
  and texture caches are bounded.

Run with `RUST_LOG=debug` for raster timings and memory accounting.

## Status

Single-site radar and the 3D/overlay stack are solid. Roadmap: configurable
color tables, packaging (AUR/Flatpak) and a desktop entry; Windows/macOS are
untested.

Release history lives in [CHANGELOG.md](CHANGELOG.md). See
[CONTRIBUTING.md](CONTRIBUTING.md) for development and release rules.

## License

[MIT](LICENSE). Third-party inventory in
[docs/THIRD-PARTY.md](docs/THIRD-PARTY.md).
