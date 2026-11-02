# Changelog

## v1.9.1 — 2026-11-02

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
