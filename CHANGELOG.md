# Changelog

Notable changes per release. The project's early history lives in the commit
log; entries start at the first public release.

## Unreleased

**Lightning (GOES GLM)**
- Optional **Lightning (GLM)** overlay: official NOAA GOES-East (GOES-19) /
  West (GOES-18) flashes, decoded locally from the 20-second NetCDF-4 files on
  the public AWS buckets (pure-Rust HDF5 reader, no C libraries). The app
  fetches one file per minute and keeps 20 minutes of flashes; fresh strikes
  are white/yellow, older ones amber, fading with age. Off by default; about
  0.4 GB/day while enabled. No accounts, keys or commercial restrictions.
  The Map controls window shows the satellite, total flashes and how many are
  within 186 mi, so an active layer with no nearby strikes is distinguishable
  from a broken one. Strikes are drawn as small lightning-bolt glyphs; fresh
  ones pulse with a soft glow, older ones settle to amber and fade.

**Loading feedback**
- The bulk-load pill now says **Fetching cache · …** when the current scan is
  read from disk and **Downloading …** only when it is actually fetched from
  S3. Live backfill is mostly cached, so it no longer claims to be downloading.

**Radar rendering**
- Gate splats now overlap by 40 % and the high-zoom radius cap is 256 px
  instead of 48 px. Extreme zoom no longer turns the radar into separate
  polka dots with gaps between rays; anti-aliased seams are closed too.

**Timeline and display defaults**
- The bottom bar now shows **every cached scan for the selected station**, not
  only the 24-frame ring: a time-proportional strip with the date range, scan
  count and size. Hover a tick for its time/size; click to load that day around
  the scan **directly from the cached `.gz` files** — no S3 listing and no
  downloads for scans that are not on disk. Ring frames are highlighted in blue.
- **Downloaded…** in the timeline opens the full on-disk history for the station:
  every cached day with scan count, size and time range, Start/Mid/End loads, and
  earlier/later 24-scan windows, all decoded from disk. Archive (S3) is unchanged.
- Hovering the timeline strip shows the exact Central date and time (and size
  on the disk strip). The separate disk strip was removed — the **Downloaded…**
  button in the same top bar opens the full browser instead.
- Each cached day in the Downloaded window has a **Delete** button (with an
  inline confirmation) that removes that whole day's scans from disk.
- 2D radar opacity and 3D volume opacity now default to **100 %**.

## v1.4.7 — 2026-10-20

**AI meteorologist (optional)**
- A **Meteorologist** window: one short now / next-90-minutes brief for every
  marker on the map, built from a compact snapshot of what the app already
  sees — the sampled radar column at each pin (surface gates, column max,
  VIL/echo tops/MEHS, HCA, snow rate, melting level), warnings containing or
  near it, tracked cells with arrival estimates, spotter reports, SPC
  outlook/MCD context, station totals and the NWS forecast.
- **Ask questions** in a chat-style transcript: every message you send and
  every answer stays in the window (scroll, copy, clear), follow-ups keep the
  whole conversation in context, and nothing is replaced when you ask the next
  question.
- Transient gateway failures are handled: the client retries 429/5xx and
  connection errors, and if the endpoint is still down the question stays in
  the chat with a **Retry** button instead of vanishing. Empty replies are
  retried too, with a larger output budget (2,000 → 4,096 tokens): a thinking
  model can spend the whole budget in hidden reasoning before writing the
  answer, and that no longer becomes a dead end.
- **It can look before it speaks**: a question about a place with no marker
  ("is it raining in Wichita?") gets a `LOOK:` round-trip. Each look is an
  **area** (default 20 mi radius, up to 3 areas): the app returns the centre
  column plus the strongest echo inside the disk with its distance/direction,
  the percentage of the area with ≥20 dBZ, the nearest echo and any warnings —
  never a single gate. The map fits the whole searched area while the report
  window shows "Looking at …". If another NEXRAD is materially closer to that
  area (30% and 40 mi closer, within range), the app switches to it, loads its
  volume and samples from there — no more "Wichita" answered by a radar 140
  miles away. It also waits for that site's **second scan** before asking the
  model, so storm cells with heading/speed/arrival exist; tracks are derived
  on demand from the two newest frames even if the map has not drawn yet. If
  only one scan arrives, explicit motion in warning/MCD text is used, and a
  missing vector is stated rather than guessed. The site switch now fetches
  that previous scan on purpose (about 5 MB) instead of relying on a cached
  one, so a fresh station has two frames immediately. Chat history is sent as
  actual user/assistant role messages.
- **On-demand data**: a `DATA:` directive loads MRMS, GOES, the NWS forecast,
  station totals or a different SPC day/type, then re-asks the model with the
  new snapshot. The sidebar layer switches on automatically while it fetches.
  The snapshot also carries every tilt's gates at each column.
- The AI window is edge-resizable: drag any edge or corner and the transcript
  fills the window (the fixed chat-height slider is gone).
- SPC outlooks now carry their **features** (label, name, risk level, approximate
  area) and, for every marker/LOOK area, either `spc_risk` (inside that area) or
  `spc_nearest` (distance to the closest edge). "General Thunderstorms Risk" and
  the severe categories are answerable without the raw polygons.
- The snapshot now includes **every tracked cell** on the scope, not just those
  within 60 mi of a marker: position, peak dBZ, speed, heading and the nearest
  marker, plus a vector mean (`mean_motion`) and each echo region's nearest-cell
  motion. The app tracks cores at or above 40 dBZ, which the snapshot states.
- The snapshot now also carries **SRM** when built, the **GOES** pixel
  brightness, spectrum width/differential phase, and an **echo-region
  overview**: connected areas of ≥20 dBZ on the lowest cut, each with its
  centroid, strongest cell, size, distance/bearing from the radar and nearest
  marker. That is what lets it answer "what about the south?" from the actual
  line rather than from blind compass samples. It is decoded data throughout:
  no screenshot is taken or sent.
- **It shows, not just tells**: a reply can carry a whitelisted `MAP:`
  directive that pans/zooms the 2D map, switches product and tilt, opens the
  3D pane, sets the camera, cuts a cross-section, or shows the 0 °C level.
  It can also **mark the maps**: labelled circles for a suspected tornado
  under a warning, a hail core or a wind threat, and altitude-tagged marks
  drawn as a pole with a ring at that height in the 3D pane (updraft tops and
  other 3D-only features). The window reports what changed and offers
  one-click *Restore previous view* and *Clear marks*.
- The opener is a chip at the **bottom-left of the 2D map**.
- Settings → **AI meteorologist…** is pre-filled for **OpenCode Go**
  (`https://opencode.ai/inference/go/v1`, `deepseek-v4.1-flash`); paste the API
  key there (or use `OPENCODE_API_KEY`). OpenAI and local Ollama/LM Studio
  endpoints work too, with a connection test.
- Off until configured; requests are sent only when you press *Write brief* /
  *Ask*, and contain the JSON snapshot only (no images or history). The key is
  stored only if you tick "remember", in `ai.json` (0600), and never leaves the
  machine except to the endpoint you set.

**Docs & repository**
- README condensed to the essentials (features, install, data sources,
  architecture, performance) instead of a long narrative.
- Release policy added to CONTRIBUTING: binaries live on GitHub Releases and
  are never committed; old release artifacts are purged from the git repo.


## v1.4.6 — 2026-10-20

**Winter analysis: hydrometeor classification, snow rate, the 0 °C level**
- Three new products, computed once at decode like VIL/TOPS/MEHS and
  available on the timeline and in 3D: **HCA** (hydrometeor classification —
  rain, big drops, wet snow, dry snow, ice crystals, graupel, hail, mixed —
  from a Z/ZDR/CC/KDP decision tree), **SNOW** (snowfall rate from the Z–S
  relation, inches per hour) and **MELT** (the melting level, the
  correlation-coefficient bright-band minimum in the 0.4–7 km layer). Color
  tables, the product explainer and the probe labels ("HCA wet snow") know
  them.
- The **0 °C level** is drawn as isohypses every 2 kft (default on, toggle
  in the map's Controls window), and the probe answers with it on hover even
  at a gate that has no echo ("0 °C level 8.4k ft").
- Derived products sample gate **centres** now instead of half a gate back —
  that half-gate offset went into every derived column.

**Winter defaults and alerts**
- Snow-friendly defaults: minimum dBZ 10, and a *Winter* echo-shell preset
  (10 / 20 / 30 dBZ, where snow lives) beside the convective one.
- Winter storm warnings and advisories use a dimmer ice-blue family, and
  advisories no longer pulse the alert badge.

**MRMS national composite** (off by default)
- NOAA's multi-radar, multi-sensor reflectivity mosaic (`noaa-mrms-pds`),
  decoded locally from PNG-packed GRIB2. One download yields two windows: a
  CONUS-wide image for context and a native 1 km/px window around the radar
  for detail, drawn in that order. Refreshes every 10 minutes while enabled,
  caches to disk, and has its own opacity slider in the map's Controls.

**GOES satellite** (off by default)
- NASA GIBS imagery in the map's own projection: **infrared** or
  **GeoColor**, GOES-East or West chosen by the site's longitude. Infrared
  arrives as an opaque gray disk covering the whole CONUS; its alpha is now
  derived from brightness so the warm surface dissolves and only the cloud
  field overlays the map. CONUS-wide plus a sharp radar window, 10-minute
  refresh, opacity and channel in the map's Controls.

**3D ground scale**
- The 3D pane's ground matches the 2D map's metres-per-pixel again (the
  auto-fit was using a fraction of the column), with a coarse surround past
  the radar and a fade at its edge instead of a hard cut.

**Internals**
- README rewritten against what the app actually does; third-party inventory
  regenerated; dependency debug info dropped from dev builds (`target/` had
  grown to 71 GB); CI's rustfmt and clippy gates fixed.

## v1.4.5 — 2026-10-19

**The 2D map gets the hover probe**
- Hover the map for the value under the cursor — the same readout the 3D
  pane has: the product's value in the gate the pixel actually falls in (no
  interpolation, so it matches what is drawn), the beam height in k ft, and
  the distance and bearing from the radar in miles and compass terms. On
  reflectivity it also reads the strongest cut in the column ("column max
  62 dBZ @ 21.0k ft") — the hail-core / storm-top number. It sits on a black
  plate so it stays readable over bright cores, has its own switch in the
  map's Controls window (on by default), and stays quiet while dragging or
  measuring.
- Both probes now appear **only over an echo**: on the map, exactly where
  the rasterizer draws a pixel (gate has data, clears the color table's
  floor, clears Min dBZ, not transparent) — point at basemap and nothing
  appears, so the height/distance lines always describe a real echo. The 3D
  probe no longer says "no echo under the cursor" either.

**Sampler fix: half a beam off**
- The shared volume sampler's nearest-ray search compared a magnitude
  against the *signed* delta of the first candidate, so whenever the ray
  after the insertion point was the closer one (half of all queries) it
  kept the ray before it. Half a beam of error went into the 3D probe, the
  cross-section, the isosurface lattice and the derived columns. Fixed,
  with a regression test pinning both sides of a ray boundary and the
  wrap-around cases.

## v1.4.0 — 2026-10-19

**Warning polygons wrap the storm in the 3D pane**
- NWS warning polygons are now **prisms** there: the polygon the 2D map
  draws is extruded from the ground into translucent walls with a rim and
  vertical corners, so a warning encloses the storm instead of lying flat on
  the ground. The layer has its own switch, a **fill** slider (0 = outlines
  only) and a **Walls ≤ … k ft** height (default 30, **0 = flat**).
- The whole prism is a target: hovering names the warning, clicking opens
  the same NOAA detail popup the map opens. The archive day's historical
  polygons draw too (calm styling — live warnings keep the loud look), and
  clicking one opens its archived NWS text.
- When the layer is on and nothing is drawn, the pane says so ("no warning
  polygons in range") instead of leaving you wondering.

**Warning polygons are synced to the radar on screen**
- No more "why is this polygon here?": live warnings are drawn only while
  the radar on screen is live, and in archive mode the archive day's
  polygons are limited to the station's coverage radius **and to warnings in
  effect at the displayed scan's time**. Scrub or play the archive loop and
  the polygons come and go exactly as the warnings did. Browsing a past day
  from the live map (archive tab open) still shows that day's set — there
  the day itself is the subject. The archive tab's list still shows the
  whole day; only the display is synced.
- Both panes share one filter, so the 2D outlines, the hover/click stacks
  and the 3D walls can never disagree.

**A product explainer**
- A small **(i)** beside the Product selector opens a window describing
  every product — REF, VEL, SW, ZDR, PHI, KDP, CC, VIL, Echo Tops, MEHS,
  SRM — with what it is, how to read it, and a short reading-tips list.

**Cross-section redraw fix**
- Toggling the cross-section (or moving its azimuth slider) appeared to do
  nothing: the curtain uploads straight into its GPU buffers, but the
  pane's render-on-demand key did not include it, so the previous frame
  stayed on screen until the camera happened to move. The key now carries a
  curtain revision, with a GPU regression test that toggles it with a fixed
  camera and asserts the frame is redrawn.

## v1.3.0 — 2026-10-18

**Derived products: VIL, Echo Tops, MEHS**
- Three new products in the selector, computed from the reflectivity volume
  the way analysts expect: **VIL** (Amburn & Wolf `3.44·10⁻⁶·Z^(4/7)`
  integrated over the beam heights at each column's slant range), **Echo
  Tops** in kft (the interpolated 18.5 dBZ crossing, the operational unit)
  and **MEHS** in inches (`0.1·√VIL`, Witt et al.), each with its own color
  ramp. The map, the 3D pane and the probe read them like any other moment.
- They are derived **at decode** on the data thread (threaded over ray
  chunks: ~45 ms on a real volume, ~4 % memory), so switching products and
  looping playback cost nothing.

**Storm-relative velocity**
- New **SRM** product: radial velocity minus the storm motion projected onto
  each radial, with *Storm motion* toward/speed sliders in the 3D Controls
  window. It follows the sliders, so it is derived on demand on the data
  thread and cached per scan + motion; the panes keep the last derivation on
  screen while the next one builds instead of blanking.

**3D analysis tools**
- **Cross-section**: a vertical curtain (RHI) through the volume along any
  azimuth, drawn with the same colors and echo floor as everything else and
  transparent where the radar did not measure, so the silhouette is honest.
  Rebuilt in a couple of milliseconds while the azimuth slider moves.
- **Echo shells (isosurfaces)**: 30 / 50 / 65 dBZ shells extracted by
  marching tetrahedra over the polar grid on the 3D mesh worker — crack-free,
  open where there is no data, so a hail core's shape reads at a glance.
- **Data probe**: hover the 3D pane for the first and strongest echo under
  the cursor — value, height in k ft, distance in miles and bearing — with
  markers in the scene, a drop line to the ground, and the readout on a
  black plate (clamped inside the pane) so it stays legible over bright
  cores.
- All three sit on one shared, unit-tested sampler (nearest ray by binary
  search, linear in gate and between cuts, plus a prepared per-azimuth
  profile) — which is what makes them cheap enough to run live.

**KDP fixed**
- KDP never appeared because the upstream decoder has no KDP block accessor:
  it is now derived from the differential-phase field (least-squares over a
  ±4-gate window, ICD-style scale/offset) when sweeps are built, verified
  against a real volume (~293k gates with data, physical −0.5…124 °/km).

**Fixes**
- Dual-pol moments (KDP, PHI) drew **blank on the 2D map**: gate geometry
  was built from a sweep's *first* moment, but PHI/KDP blocks carry fewer
  gates than REF in the same sweep (720×1192 vs 720×1832), so the rasterizer
  refused the reused table every frame. Geometry is now per moment
  (regression-tested), and the playback-tolerant reuse (elevation jitter, a
  dropped start ray) still absorbs scan-to-scan differences instead of
  rebuilding ~90 ms per tick.

## v1.2.9 — 2026-10-17

**The 3D loop stops skipping frames**
- Two bugs stacked: a finished mesh was discarded whenever the playhead had
  moved past it (so a pane whose builds are slower than the tick showed only
  the frames that happened to win the race — "plays a few, then pauses"),
  and completions were only collected on UI frames, which during playback
  arrive once per tick. Builds now upload whenever they are newer than what
  is displayed (strictly monotonic; a frame one behind is the intended
  playback behavior, unit-tested), and while a build is pending the pane
  asks for a repaint 30 ms out so meshes land the moment they are ready.

**The alerts archive grows up**
- New **`Archive: <SITE>`** tab: the archive day restricted to warnings the
  station *owns* — it is the radar nearest the warning's polygon. Neighbors'
  warnings (Tulsa's, Omaha's) stay in their own tabs; county-based events
  carry no coordinates and never appear in a station-restricted view.
- Clicking a historical warning's polygon on the map opens its **archived
  NWS text** — the day-files carry metadata and polygons only, so the full
  bulletin is fetched once from IEM's product archive (on demand, cached
  forever, deduped); the popup also carries "load radar at this time" and
  "center map".
- Row-index fix: rows in the station tab carried *filtered* positions into
  the jump action (which addresses the full list), so a click could load a
  different warning entirely — a Topeka row opened a Midland polygon, in
  the field. Rows now carry their true indices.

**Alerts UI speaks miles and reads clearly**
- Rows read "42.1 mi SSW of KTWX" (brief tooltips and archive rows too),
  the station radius shows "~250 mi", the station tab label carries the
  city ("KTWX Topeka"), and its header names the station, city and area.
- Every list groups by category: per-event sections with counts, ordered
  convective-first (Tornado → Severe Thunderstorm → Flash Flood → Flood →
  Marine → winter / wind / heat / fog / fire → everything else).

**Pane controls get their own windows**
- Each pane has a bottom-right **"Controls"** chip opening that pane's
  display settings: 3D (echo floor, opacity, height scale, tilt range,
  altitude slab in feet, mesh detail, fit view) and map (Min dBZ, opacity).
  Scrolling over a slider adjusts it one step. The old floating bars are
  gone; everything else stays in the side panel.
- The archive day is a **clickable calendar** in both the alerts Archive
  tab and the Settings archive popup (month navigation, clamped to today).

**Going live is obvious — and reliable**
- **"GO LIVE" / "● LIVE"** sits in the badge row right of Forecast: red and
  pulsing while browsing history, dim green on the newest scan. GO LIVE now
  carries the UI's selected site with it, and a mismatched volume stream
  logs a loud warning naming the site to pick — a silent site desync used
  to drop every volume (an empty map while the decoder hummed along).
- A top-center **download progress pill** shows bulk loads with a spinner
  and real counts ("Downloading KTWX live radar · 13/24", archive windows
  included), flipping to "rendering" at 100% and fading out — a
  multi-second fill no longer reads as a stall.

## v1.2.8 — 2026-10-17

**The warning backfill was fetching tornado warnings only**
- The day-file request used IEM's `limitps` flag, which silently filters
  the archive to tornado warnings: a day measured 42 records where the
  real convective set has 328 (146 severe thunderstorm, 93 flash flood,
  42 tornado, 47 special marine). The request now uses `limit0` — the
  convective warning set, which is what carries polygons and what the
  radar-at-warning jump needs (~130 KB per day-file; the unfiltered day
  is 1.26 MB, far too much for a years-long cache).
- Cache scope bumped: day-files from the wrong filter are removed on the
  first run (they are re-fetched as you browse or backfill).

## v1.2.7 — 2026-10-17

*(Rolls up the 1.2.5/1.2.6 fixes with the new alerts archive — those two
patch releases were withdrawn unpublished.)*

**Alerts archive: browse 20+ years of warnings**
- The alerts browser has an **Archive tab**: historical NWS warnings from
  the IEM VTEC archive (api.weather.gov only serves *active* alerts).
  Coverage: storm-based polygons from 1 Jan 2002, all other VTEC products
  from 12 Nov 2005, county-based listings back to 1986 (shown as
  "county-based", nothing to draw).
- One small day-file (~100–300 KB) per day viewed, cached forever and
  loaded only when you open the tab or change its day — never polled.
- **Click a historical warning** (list row or its polygon on the map) and
  stormscope snaps to the radar that covered it, loads the archive radar
  for that date/time, zooms to the polygon, and parks the playhead at the
  warning's issuance once the data lands. The day's polygons also draw on
  the map in a calmer style than live warnings, with a dwell tooltip that
  labels them "historical" and says what a click will do.
- **Opt-in backfill**: a crawler walks the archive newest-first (today →
  2002) at ≤1 day-file per 850 ms, 800/day, skipping anything cached,
  backing off on failures and pausing itself if they persist. Progress
  lives in `warnings/crawl.json` and shows in the tab.
- The parsers (zip → DBF + shapefile) are in `radar-data::warnings_archive`
  and verified against real archive days (59 warnings, polygons intact).

**The 3D pane runs the timeline with you — at full detail**
- Volume meshes now build on a worker thread: playback animates the 3D
  view at the user's detail (Fine) instead of holding still, and the UI
  thread never waits on a ~50 ms mesh build. Scene buffers are reused
  across frames, so timeline playback costs no per-tick allocation.

**Fixes (the withdrawn 1.2.5/1.2.6 content)**
- Playback stutter: radar gate-geometry tables are reused across scans
  again — measured-elevation jitter and a dropped axis ray no longer
  force a ~90 ms rebuild on every playback frame.
- Ring/line flicker: the 3D ground grid's rings and spokes hold a
  minimum on-screen width (and the pane never renders below display
  resolution while moving); the 2D map's range rings tessellate to their
  on-screen radius instead of a fixed 96 points.
- Ground-map shimmer at steep angles: mip emulation in the 3D ground
  shader (adaptive multi-tap as the texel footprint exceeds a pixel).
- Pausing (button or Space) jumps the playhead to the newest scan:
  stopping means "show me now".

## v1.2.6 — 2026-10-17

**Range rings and spokes hold a screen-space width — no more flicker**
- The grid was baked as fixed ~1 km-wide ribbons, so at distance or steep
  pitch it compressed to sub-pixel width and the rasterizer's coverage
  flipped frame to frame while the camera moved: flickering, jagged rings.
- The rings and spokes are now emitted as *centerlines* (their
  perpendicular rides in the vertex normal) and the vertex shader expands
  them to a minimum on-screen width using the per-vertex distance to the
  eye — the standard screen-space-line technique. Lines are pixel-stable
  at any pitch, at any zoom, in any pane size; the grid also shades unlit
  now (its normals are line perpendiculars, not surface normals).
- Volume pass cost measured unchanged (2.0 ms settled / 0.96 ms motion).

## v1.2.5 — 2026-10-17

**Ground-map flicker at steep camera angles, fixed with mip emulation**
- At steep or grazing pitches the basemap tiles compressed so one screen
  pixel covered many texels; with single-mip textures the one-tap LINEAR
  sample picked effectively random texels and roads, labels and boundaries
  crawled and flickered while the camera moved. Neither MSAA nor plain
  supersampling addresses this class of aliasing.
- The ground fragment shader now measures the texel footprint from
  screen-space derivatives and blends a four-tap diagonal cross in as the
  footprint grows past one texel (a box-ish filter — sharp at 1:1, soft
  exactly when undersampling starts). Volume pass cost measured unchanged
  (2.1 ms settled / 1.05 ms motion on the A310).

## v1.2.4 — 2026-10-17

**The playback stutter was ~90 ms gate-table rebuilds — now reused across scans**
- The app's log showed 68 "radar geometry rebuilt in ~90 ms" events clustered
  in playback windows: the geometry cache keyed on the *measured* elevation
  (which jitters by scan) and demanded exact ray counts (L2 volumes drop or
  gain a ray at the scan start), so consecutive playback frames missed it and
  stalled the UI thread ~90 ms per miss. That was the "weird jumpy lag" while
  playing frames, panning the map, or flying the 3D view.
- The cache now keys only on what actually changes the table layout (site,
  nominal tilt, azimuth width, gate layout) and `reuse_for` walks both rings
  with a tolerant nearest-azimuth mapping: scan-to-scan jitter, a rotated
  scan start, and a dropped/extra start ray all reuse the table. A genuinely
  different layout still rebuilds. Covered by tests (jitter, missing start
  ray, rotated remap).

**3D pane anti-aliasing — by supersampling, not MSAA**
- 4× MSAA was measured on the development A310 at 17.8 ms/pass — more than
  the whole frame budget — and wouldn't fix what actually shimmers: the
  ground tiles have no mipmaps, so their aliasing is texture minification,
  which MSAA ignores.
- Instead the pane renders at 1.6× its display size and egui's linear
  filtering downsamples it (2.2 ms/pass measured), while camera motion draws
  at 0.8× (1.1 ms/pass). Edges smooth, tile shimmer smooth, budget intact.

## v1.2.3 — 2026-10-17

**Panning the 3D pane no longer competes with the playback raster**
- With the timeline playing, the 2D pane re-rasterizes the radar on every
  tick (~125 ms). The raster gate only treats *its own* drags as motion, so
  flying the 3D camera counted as idle: every playback tick fired a full
  raster spike on the UI thread right under the user's fingers.
- The gate now also counts 3D-camera motion (drag, wheel, WASD) as motion —
  while the 3D view is being flown the radar raster holds, then lands as
  soon as the camera settles (150 ms).
- (Separately: the v1.2.2 git tag had been created at the wrong commit by
  the release tooling after the repo went public mid-push; it now points at
  the v1.2.2 release commit.)

## v1.2.2 — 2026-10-17

**Playing the timeline no longer drags the 3D pane down with it**
- The 3D scene followed every playhead tick, so an 8 fps loop triggered a
  full ~1M-vertex mesh rebuild and ~50 MB of buffer uploads eight times a
  second — map panning and 3D orbiting in that state were a slideshow.
- The scene now holds still while the timeline plays and re-syncs the moment
  playback pauses (the pane says so in its stats line). While paused,
  scrub-dragging rebuilds at most every 250 ms: smooth to drag, and the final
  position still lands because the stale request is re-asked until built.
  The policy is a unit-tested pure function.
- Panning the 3D pane during playback now costs one half-res volume pass per
  moved frame and nothing else.

## v1.2.1 — 2026-10-16

**Performance: the 3D pane renders on demand (and at half res while moving)**
- The app's own telemetry showed frames taking ~41 ms with under 1 ms of CPU
  work while the 3D pane was interacted with — a GPU-bound frame, with the
  pane's offscreen pass paid on every repaint including idle wakes.
- The pane now renders only when something changed (camera, display
  settings, volume, ground tiles, size). A motion frame draws at half
  resolution — the same pattern as the map's rasters — and one
  full-resolution frame lands 160 ms after the motion stops; idle repaints
  cost a texture sample. The policy is a pure function under unit test, and
  the stats line reports `3d passes` so regressions are visible.
- Measured for scale on the development machine (Arc A310): one volume pass
  at full pane size is ~1 ms alone; the cost was the *every-frame* habit,
  not the pass.

## v1.2.0 — 2026-10-16

**3D radar pane (new)**
- The map gained an optional 3D companion: every elevation cut of the
  current scan as one rotatable volume — beside the 2D map (resizable split,
  draggable divider) or maximized to fill the window. "Max"/"Split" chips
  live on the panes, "Fit" re-frames the camera; drag orbits, right/middle
  drag — or W/A/S/D with the pointer over the pane, Shift to sprint — pans
  freely, wheel zooms, double-click re-fits.
- The geometry is honest: each cut is a draped surface over its azimuth ×
  range grid at true 4/3-earth beam heights (a 0.5° beam is ~1.5 km up at
  100 km), and no-echo gaps are skipped rather than smeared across. Height
  scale is the readability knob (default ×20 — full —, range ×1–×20)
  because real storms are ~15 km tall over a 300 km view — 1:20 on paper.
- Units are US radar-display units: range rings every 50 mi, the altitude
  scale and the altitude slab in feet, echo floors in the moment's unit.
- The pane's bottom center carries the three quick sliders (echo floor,
  opacity, height scale — the wheel adjusts the one under the pointer); the
  sidebar carries tilt range (e.g. only 0.5–1.8°, or one cut alone),
  altitude slab (e.g. only echo above 10,000 ft), mesh detail, and it
  follows the map's moment and timeline playhead. Mesh detail defaults to
  Fine (1° columns).
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
