# Changelog

## v1.9.0 — 2026-11-01

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
  the selected scan *time* (not a frame index), so all maps show the same
  moment.
- The primary map now paints the newest cached scan immediately while the S3
  listing runs, so a slow or throttled listing no longer leaves it stuck
  behind the loading card.
- Android full-pane analysis windows (model comparison, storm trends, forecast
  videos) now fill the pane instead of being offset off-screen.
- Live radar polls the S3 chunk feed every 30 seconds instead of every 5,
  reducing throttling while still catching new scans promptly.

## v1.8.9 — 2026-10-31

**Phone-local times, timeline hold, and UI polish**
- Displayed scan times and archive day/time-of-day now use the device/OS
  timezone with short names such as CDT/CST/PDT, instead of hardcoded Central
  time.
- In Live mode, scrubbing to an older radar scan holds that frame while new
  scans arrive. After 30 seconds without touching the scrub bar it returns to
  the newest scan; closing/reopening the app also returns to live.
- The top status bar is removed on Linux, Windows, and Android. Site, product,
  tilt and version now appear at the top of the Controls/Settings sidebar;
  scan age, latency and status text were not moved there.
- Radar keeps refreshing while the map is dragged instead of only after the
  finger lifts.
- All buttons use slightly rounded corners on every platform.
- Android Settings/Controls/GPS actions use matching large touch targets, with
  AI above Settings and hidden until an API key is configured.
- Map Controls fill the map area, support swiping between Display and the SPC
  outlook, and Back returns to Display before closing.
- Forecast gains Forecast/Videos tabs with swipe navigation, compact current
  conditions, Details collapsed by default, and no manual refresh button.
- Archive moved to the bottom of Settings.
- Fixed the Android sidebar header/Close button clipping after the status bar
  removal.
