# Changelog

## v1.9.5 — 2026-11-10

**PWPF labels, station switches, and signed Android updates**
- Fixed WPC PWPF snow amount labels: the 1" polygon has the 2"+ area cut out as a KML hole, so nested bands are now parsed with their holes and each number is placed in its own band instead of inside the next threshold's area.
- Switching radar sites clears the old station's frames and radar texture immediately, so the map cannot show the previous radar georeferenced to the new site while the clear command is in flight.
- Android release builds are signed with the installed key again, so in-app updates replace the app in place; a release build now fails instead of producing an unsigned APK nobody can install.

## v1.9.4 — 2026-11-03

**WPC winter products, smoother dragging, and platform defaults**
- Added WPC Winter Storm Severity Index (WSSI) with Overall, Snow Amount, Snow Load, Ice Accumulation, and Blowing Snow components across Day 1/2/3 and Days 1-3, with official impact colors, hover tooltips, and a click detail card (same layout as SPC).
- Added WPC Probabilistic Winter Precipitation Forecast (PWPF) with snow/ice amounts (50th percentile, inches) and probabilities (≥4"/8"/12"/≥0.25", %), Day 1-3, including vector bands and raster ice overlays. Amount/probability labels stay visible and anchored to full-resolution geometry while dragging.
- Winter weather geometry now draws at full resolution with no shape simplification during drag or zoom. County borders only draw at zoom ≥ 7.5 to keep wide views smooth; state basemap boundaries remain.
- Radar rendering now keeps the layer visible throughout pan/zoom by rasterizing at low resolution during gestures (with full resolution ~100 ms after), eliminating blank areas and sudden pop-ins.
- Platform defaults: on Android, SPC defaults to Tornado only, WSSI defaults to Overall only, PWPF defaults off; desktop keeps all on by default.
- QC defaults: Hail cores (MEHS), Clutter filter (CC), and Attenuation correction (KDP) on by default; beam blockage remains off.

## v1.9.3 — 2026-11-02

**WPC winter outlook and phone-first defaults**
- Added WPC's Winter Storm Severity Index (WSSI) beside the SPC outlook:
  Overall Impact, snow amount, snow load, ice accumulation, and blowing snow,
  with Day 1/2/3/Days 1-3 selection and WPC's official impact colors. The
  WSSI fill defaults to 5%, matching the watch layer.
- Linux and Windows start with every WSSI component; Android starts with the
  Overall Impact summary (the maximum across components) to keep a phone map
  uncluttered. The other components and ALL remain one tap away in Map
  Controls.
- Android SPC outlook now starts with Tornado only; Categorical, Wind, Hail
  and ALL remain selectable. Desktop SPC defaults are unchanged.
- WSSI fills render under the radar and their impact outlines above it,
  following the same two-pass treatment as the SPC outlook. Hover a WSSI
  outline for a one-second tooltip and click it for the same kind of detail
  card the SPC outlooks use, including the WPC impact definition and a note
  that WSSI is impact-based rather than an inches forecast.

## v1.9.2 — 2026-11-01

**Watches are back on the map**
- Nationwide watches now take their county geometry from the bundled county
  set, so the map draws the county-grid watch areas even when a watch is
  outside the station-radius alert list. Previously only station alerts were
  enriched, leaving the browser's nationwide watches with no polygon and no
  map rendering.
- A zone URL such as `OKC003` resolves directly to its county rings, so
  drawing a multi-county watch costs no API request per county.

## v1.9.1 — 2026-10-31

**County context, county-grid watches, and multi-pane fixes**
- Added a county-border overlay drawn under the radar return, with a "County
  borders" toggle in Map Controls. County and city names stay with the
  basemap, whose labels are now brighter and easier to read on the dark map.
- Zone-based watches render as the NWS county grid again: every affected
  county is filled and outlined individually instead of being dissolved into
  one outer boundary.
- Fixed concave warning and watch fills rendering as gray/white triangular
  fans; the fills are now triangulated correctly.
- Clicking any station marker (synoptic, Ambient Weather, rain/snow totals)
  now switches to the radar that covers it, matching radar-site clicks.
- Each pane shows its radar site beside the Max/Restore chip, Map Controls
  titles include the site, and site changes persist across restarts.
- Fixed secondary panes drifting out of sync on alerts, watches and SPC
  outlooks; passive panes no longer fetch nationwide alerts or outlooks.
- Alert-detail windows use the full available height and get a per-pane window
  id, so they no longer leave a dead band or clip text mid-line.
- Added the per-pane Max/Restore button, watch-fill slider, and the remaining
  multi-pane polish from the v1.9.0 follow-up work.

## v1.9.0 — 2026-10-30

**Multi-pane radar, stable SPC outlook labels, and tablet polish**
- Radar now supports up to two independent panes on Android and up to four on
  Linux/Windows. Each pane has its own site, product, tilt, layers, map view,
  and full Map Controls; the layout and per-pane defaults are persisted.
- Every pane runs the complete radar app — warnings, storm tracks, markers,
  storm reports, SPC outlooks, archive timeline, and FutureCast — instead of a
  simplified second view.
- SPC outlook areas now label their own borders in SPC's colors: categorical
  bands (`MRGL`, `SLGT`, `ENH`, …) and probability/CIG bands (`10% TOR`,
  `15% WIND`, `CIG1 HAIL`). Labels sit on a dark plate that breaks the outline,
  are clickable and hoverable like the border, and are anchored to fixed
  points on the polygon so they no longer jump while panning or zooming.
- Fixed outlook hover and click using the wrong product when all four SPC
  products are loaded, and fixed conditional-intensity (CIG) labels rendering
  as black boxes.
- Android: returning after a long spell in the background now shows a
  prominent "Downloading the latest radar frame…" strip at the top of the map
  while the newest scan is fetched. The strip is reserved for long resumes;
  normal in-app downloads stay quiet, and it never covers the middle of the
  radar.
- Tablet and phone UI: Map Controls and Settings use a tighter content scale
  on tablets, Map Controls open centered and size to their content, and the
  Android controls card is fixed while the desktop window stays draggable.
- Moved QC/calibration, rotation/TVS, hail cores/swath, and local weather
  stations from Map Controls into Settings. Flash density, QC, rotation, and
  hail are off by default.
- Station totals (rain/snow) is now an app-wide layer: turning off "Station
  totals on map" in Settings hides the diamonds in every pane, not only pane 1.
- The bottom scrub bar and Play/Pause now drive every pane: each pane follows
  the selected scan *time* (not a frame index), and the scrub/play state is
  shared so secondary maps repaint at the same cadence instead of lagging
  behind. "Last 2hr" loads its window for every pane, anchored to the same
  newest scan, and a pane created while pane 1 already has a long history
  seeds the same window automatically.
- The primary map now paints the newest cached scan immediately while the S3
  listing runs, so a slow or throttled listing no longer leaves it stuck
  behind the loading card.
- Android full-pane analysis windows (model comparison, storm trends, forecast
  videos) now fill the pane instead of being offset off-screen.
- Live radar polls the S3 chunk feed every 30 seconds instead of every 5,
  reducing throttling while still catching new scans promptly.
