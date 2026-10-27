# Changelog

Notable changes per release. The project's early history lives in the commit
log; entries start at the first public release.

## v1.6.0 — 2026-10-27

**Radar site**
- Desktop starts on **Norman (KTLX)** when there is no GPS; Android still
  picks the radar nearest to the first GPS fix.
- **Set default** in Controls stores the selected site and uses it on every
  launch. A saved default also stops the GPS-nearest choice from taking over.
- Controls show which default is in effect: the saved site, GPS-nearest, or
  Norman.
- Changing radar sites refreshes the weather-station layer immediately
  instead of waiting for the 20-minute Synoptic timer. **Apply site** and
  **Set default** also move the map to the chosen radar; map, GPS, alert and
  AI site changes keep the station layer in step.

## v1.5.19 — 2026-10-27

**Weather stations on the map**
- Synoptic (Mesowest) network stations: NWS ASOS/AWOS, state mesonets and
  personal weather stations, fetched automatically for the current map view.
- Ambient Weather: your own personal station via the Ambient API (API key +
  application key), always shown as a green thermometer.
- Thermometer badges are tinted by temperature; the selected station's label
  avoids user pins and their labels.
- Hover a station for a second for a readable observation card; click for the
  full fixed-width popup with °F, mph, inHg and inches.
- Station settings have dedicated popups; Synoptic refreshes every 20
  minutes, your Ambient station every 5 minutes. "Show stations on the map"
  is in Map controls; your own station is always on.

**AI meteorologist**
- FutureCast can be requested on demand, including the 48-hour run, and is
  included in the live voice summary. Download notes mention the 30–60 s wait.
- Fixed a stuck spinner after a Gemini Live tool answered.
- Instruct the live model to keep one consistent voice for the session.

**Map**
- Android pinch-to-zoom is damped and no longer pans the map mid-pinch;
  wheel-style zoom is ignored on touch.
- Disk cache moved to the bottom of Settings.

**Desktop**
- The AI settings window hugs its form instead of leaving a blank band.

## v1.5.18 — 2026-10-26

**FutureCast and live radar**
- FutureCast now covers the whole CONUS (no more local square), with a
  512 MB cache cap.
- Precipitation-type colouring: HRRR `CSNOW/CICEP/CFRZR/CRAIN` recolour
  snow blue, ice purple and freezing rain pink; rain keeps the reflectivity
  ramp. Toggle in the FutureCast bar and Map controls, with a winter legend.
- Live radar has an Android-only dBZ crosshair: tap **dBZ** on the timeline,
  drag the map, and read the value in a card above the crosshair. The finger
  tooltip is disabled on touch.
- FutureCast playback keeps running in fullscreen.
- Left/right arrows navigate the scrub bar for FutureCast on Windows/Linux.
- 18/48-hour switch is smaller on desktop and only appears once loaded.
- Forecast and FutureCast share one row of larger action buttons.
- Map tile attribution moved into Settings.
- Map controls window now fits all controls on a phone.
- Settings/GPS chips stacked so the date/time card is not obscured.
- Removed the map's 3D opener chip (3D opens from the sidebar only).

## v1.5.17 — 2026-10-25


**Linux**
- **AppImage release with self-update.** The Linux artifact is now a
  `stormscope-1.5.17-linux-x86_64.AppImage` that bundles the Piper runtime and
  voice. An installed AppImage detects a newer release from GitHub, downloads
  it, replaces itself atomically and relaunches.

## v1.5.16 — 2026-10-25

**Windows**
- End-to-end update test release for the Windows installer/updater. An
  installed 1.5.15 build should detect 1.5.16, download the setup executable
  and relaunch into the new version.
- The setup and app executables now carry the Stormscope icon.

## v1.5.15 — 2026-10-25

**Windows**
- **Per-user installer.** `stormscope-1.5.15-windows-x86_64-setup.exe`
  installs to `%LOCALAPPDATA%\Programs\Stormscope`, creates Desktop and Start
  Menu shortcuts, registers an uninstall entry, and keeps `Uninstall.exe`.
- **In-app Windows updater.** The same centred update modal now checks GitHub
  for a newer Windows installer, downloads it, runs it silently, and relaunches
  Stormscope.

## v1.5.14 — 2026-10-25

**Android**
- The update modal now stays open while the user grants "install unknown
  apps", downloads, and launches the system installer. An **Install** button
  retries the installer after the permission is granted.

## v1.5.13 — 2026-10-25

**Android**
- Remote-update release for end-to-end testing of the in-app updater: an
  installed 1.5.12 build should detect this release, download the APK and hand
  it to the system installer.

## v1.5.12 — 2026-10-25

**Android**
- **In-app updater.** Stormscope checks GitHub Releases on launch and shows a
  centred modal when a newer release is available. One tap on **Update**
  downloads the APK and hands it to the Android installer.
- Added the current version number to the top status bar on all platforms.

## v1.5.11 — 2026-10-25

**Android**
- The Settings drawer now fills the map width, with larger text and buttons
  and wrapped labels so nothing runs off the right edge.
- Archive and AI meteorologist buttons close Settings and open their window.
- AI settings fills the screen, cannot be moved or resized, and its form spacing
  fills the panel instead of leaving a large blank band at the bottom.
- The soft keyboard no longer flashes while editing AI settings; the form
  scrolls clear of the keyboard.

**Linux / Windows**
- AI settings and Archive remain fixed-size, non-resizable windows.

## v1.5.10 — 2026-10-25

**AI**
- **Fewer 409 Conflict errors from OpenCode Go.** Test connection now uses a
  fresh `x-opencode-session` for every run, and retries use their own session
  id instead of reusing one that may still have an in-flight request. This
  removes the repeated-session collisions behind the 409 spike.

## v1.5.9 — 2026-10-25

**Android**
- **Storm Watch notification redesign.** Large, transparent current-conditions
  text in two clean lines (`51°F · Overcast` / `Wind NNE 3 mph`), an animated
  weather glyph that matches the condition (sun, moon, cloud, rain, snow,
  storm or fog), no redundant Stormscope title and no “watching for…” text.
- **Tapping the persistent notification opens the NWS forecast** for the
  spotter's GPS point (or current pin) instead of only recentring the radar.

**Forecast window**
- Current conditions now lead with a large temperature and condition headline,
  followed by a scannable label/value list for dew point, humidity, wind,
  pressure, visibility, precipitation, sky, UV and related values instead of
  one long dot-separated line.

## v1.5.8 — 2026-10-25

**Desktop (Linux / Windows)**
- Removed the AI window's **“Show me:”** quick prompts and the privacy line
  beneath them.
- Removed the desktop AI header (endpoint/model and the Settings + Minimize
  buttons). AI settings remain available from the sidebar.
- Removed the **Write brief** button.
- Removed the **Restore previous view** button.

**All platforms**
- After the AI moves the map or 3D pane, the previous view is now restored
  automatically **60 minutes** later. The manual Restore button is gone.
  Android keeps the rest of its AI UI unchanged.

## v1.5.7 — 2026-10-25

**Android**
- **The meteorologist can now control the radar during a live voice
  conversation.** Gemini Live function calls drive the same map actions as the
  typed AI: move/zoom the map, switch product, open or close the 3D pane,
  change tilt/view angles, mark features, inspect other areas (`look`) and
  fetch MRMS/GOES/forecast/totals/SPC data. The data tools answer with a fresh
  snapshot, and site switches wait for the new radar before replying.
- **Gemini Live defaults to `gemini-3.8-live-extended-thinking`.** The setup
  now sends `thinkingLevel: high`, which that model requires; the API closed
  every session without it. The model can still be changed in Settings.
- **New round AI button, always visible at the bottom-left.** It replaces the
  text “AI Meteorologist” chip, opens/expands the AI window, and glows and
  pulses while either side is talking (microphone level, model speech, or an
  in-flight request). The AI window header gained a minimise button.
- **Notification tap now just restores the radar.** Tapping the persistent
  Storm Watch notification closes the AI window, requests a fresh GPS fix and
  recentres the map; it no longer opens the AI or asks for a brief.
- **Fixed the black screen after swiping Stormscope from Recents.** The old
  activity blocked in `GameActivity.terminateNativeCode` waiting for the Rust
  looper thread, ANR'd, and the next launch hung. The process now exits before
  that call on a real destroy; configuration changes still recreate normally.
- **Live radar catches up after backgrounding.** Returning to the foreground
  with a scan older than six minutes re-enters live mode and fetches the newest
  scan instead of waiting for the frozen live poller.

**Linux / Windows**
- The round AI button replaces the text chip on desktop as well; the minimised
  bar remains above the timeline.

## v1.5.4 — 2026-10-24

**Linux**
- The Linux release is now a tarball that bundles the Piper runtime and the
  **`en_US-lessac-high`** neural voice, so spoken alerts work without a
  separate Piper install. Extract the archive and run `./stormscope`; the app
  finds the adjacent `piper/` tree. The installed app can also use
  `~/.local/share/stormscope/piper`.
- The bundled C++ Piper now receives its text on stdin (the input mode it
  supports); the Python `piper-tts` fallback still takes the text as an
  argument.

**Android**
- Includes the v1.5.3 60-second silence timeout and clean session-ending
  message. versionCode 5.

**Windows**
- Unchanged from v1.5.2, which still ships the single-file Piper embed.

## v1.5.3 — 2026-10-24

**Android**
- The Gemini Live voice session now ends after **60 seconds of silence**,
  closing the microphone and the WebSocket. Speech from either side restarts
  the timer, so an active conversation is never cut off.
- Mid-session connection drops now show **“Live AI session ended. Tap Talk to
  start a new one.”** instead of the raw WebSocket/OS error. The technical
  detail stays in logcat.

Linux and Windows are unchanged from v1.5.2.

## v1.5.2 — 2026-10-24

**Android**
- **Natural voice conversation with the AI meteorologist, powered by Gemini
  Live.** Tap **Talk** in the AI window for a native speech-to-speech
  conversation: ask about the radar, warnings, current conditions or the full
  NWS forecast and hear the answer in a human voice. The session carries the
  same live weather snapshot as a typed question, including a freshly fetched
  forecast, observation and station rain/snow totals before it connects.
  Input and output are pinned to US English, and the microphone pauses while
  Gemini speaks so the model cannot hear itself and cut an answer off. Add a
  Google AI Studio key under **Settings → AI meteorologist → Gemini Live** —
  separate from the OpenCode key, with editable model (`gemini-3.8-live`) and
  voice (Aoede by default) fields. The microphone is requested on first use;
  voice audio is streamed to Google only while a conversation is active.
  Android only.

**Desktop**
- **The AI meteorologist can answer rain and snow accumulation questions on
  Linux and Windows too.** Its snapshot now carries a top-level
  `rain_snow_totals` array from the same NWS station reports the Totals window
  uses, including latest values and hourly sums in inches.
- The AI settings window now scrolls when the content is taller than the
  window, and the connectivity button is labelled **Test text AI** to distinguish
  it from the new Android voice mode.

## v1.5.1 — 2026-10-24

**Alerts**
- The Alerts browser separates polygon alerts from zone/county alerts: the
  **All US** and station tabs list only warnings with a polygon (the ones you
  can click to center on the map), and a new **Zones** tab holds the
  no-polygon alerts. Clicking a zone row opens its full NOAA text.

**Desktop**
- **New warnings and watches are read aloud on Windows and Linux.** Desktop
  Stormscope uses the neural **Piper** voices when installed (the female
  `en_US-lessac-high` voice by default, with a voice picker in Settings), and
  falls back to SAPI on Windows or speech-dispatcher/espeak-ng on Linux. The
  Windows CI artifact is a single **`Stormscope.exe`** with Piper and the
  lessac high voice embedded; first launch extracts them to
  `%LOCALAPPDATA%\stormscope\piper`. Only alerts whose polygon is within
  **50 miles of the GPS fix or a manual map marker** are read; zone-only
  alerts have no polygon and stay
  silent. The first sweep after launch or a station change is silent, speech
  is serialized so a burst cannot talk over itself, and Settings has a master
  **Voice alerts** switch, separate **Tornado warnings**, **Severe
  thunderstorm warnings**, **Watches** and **Other warnings** switches, and
  a test button.
- Spoken desktop alerts are now triggered from the background data thread as
  well as the UI, so a hidden Wayland window on another workspace keeps
  announcing warnings — Wayland does not suspend the process, and the trigger
  no longer depends on frame callbacks.
- **Windows no longer crashes when the slide-out menu first draws monospace
  text.** The system font path (Consolas in `C:\Windows\Fonts`) was inserted
  as a font-family entry without its bytes ever being loaded, so epaint
  panicked with "No font data found". Stormscope now loads the font data and
  falls back to the bundled Hack font if the file is unreadable.
- Voice discovery checks explicit Piper paths instead of recursively walking
  the bundle, avoiding UI stalls and reparse-point loops; the Windows bundle
  has one voice, shown as a label instead of a combo box.
- The single-file Windows build passes announcement text to `piper.exe` on
  stdin, hides the piper console with `CREATE_NO_WINDOW`, and no longer falls
  back to SAPI on Test voice.

- **Current conditions are now a full surface report**: air temperature, dew
  point, relative humidity, wind speed/direction/gusts, altimeter and
  sea-level pressure, 1/3/6-hour precipitation, visibility, sky cover with
  cloud-base heights, heat index/wind chill, the current UV index
  (Open-Meteo, since NWS has none) and a moon-phase glyph computed from the
  device clock. The temperature/wind/pressure/UV numbers tween when new data
  arrives, forecast temperatures animate, and the moon disc has a slow glow.
- The Android AI window's header is one clean row: the title with the tiny
  model name to its right (`AI meteorologist  mimo-v2.6-flash  ✕`) and no
  separator rules. The Write brief, Settings and Minimize buttons and the
  endpoint path are gone — Settings lives in the slide-out, the ✕ closes the
  window and a brief arrives from the notification tap. Desktop keeps the full
  toolbar.
- The Android Map controls window now uses the phone header (title left,
  painted ✕ at the right edge of a row that spans the box) instead of the
  stock title bar whose background stopped short of the window edge, and its
  height hugs the content — no empty band under the last row. Content past
  the cap scrolls. The desktop Map controls window had the same empty band
  (its scroll area pinned the box to the default height); it now hugs its
  content on Linux and Windows too.
- The Android AI window drops the "Show me" quick-prompt row and the
  data-sharing note under the composer, and the filled window is inset a few
  points so its title header is never flush with (or cut by) the pane edge.
  Desktop keeps both.
- Every forecast period row now carries an animated weather icon drawn from
  the NWS summary — sun with rotating rays, drifting clouds, falling rain,
  drifting snowflakes, a flickering thunder bolt, sliding fog, and day/night
  variants — sized 34 pt with 1.2x row titles. The window repaints while open
  so the animation plays, and titles measure against the icon plus scrollbar
  with an explicit-wrap policy, so phone rows no longer run under the edge.
- The current-conditions moon glyph is rebuilt from clipped primitives (the
  old filled polygon rendered as an invisible stroke): correct terminator,
  crisp rim, faint craters, correct waxing/waning side. Current conditions
  now read "Temp" instead of "Air".

**AI meteorologist**
- **FIXED — the AI claiming the forecast "only goes to Sunday."** The snapshot
  truncated the list to the first four periods while the forecast window kept
  showing all of them. The snapshot now carries **every** period in the list,
  each with its start time (UTC) and day/night flag. (The NWS gridpoint
  forecast itself only extends 7 days, so "through Friday" is the true end of
  the data.)
- **FIXED — repeat questions reusing stale current weather.** When the cached
  surface observation is older than two minutes, a question now forces a
  forecast/observation refresh first and is parked until the new observation
  lands (6-second deadline), so the answer is always built from the fresh
  snapshot. Verified in logcat: a 223-second-old observation triggered the
  refresh path and the answer used the fresh one half a second later.

**Presentation**
- Every full-pane phone window — AI, Alerts browser, NOAA alert detail, NWS
  forecast, rain/snow totals and the SPC outlook discussion — gets the same
  small inset (4 x 8 pt), so their headers no longer sit flush against the
  pane edge and read as cut off at the top.
- The slide-out sidebar now reaches the timeline bar on every platform. It
  reserved 16 pt at the bottom (and ignored the panel frame's own 4 pt
  padding), leaving a visible gap above the Play row; the panel height now
  accounts for the frame padding so the panel sits flush on the bar.
- The project is spelled **Stormscope** everywhere it is shown: status bar,
  window titles, the Android app label and the docs (crate/package names,
  paths, URLs and the logcat tag stay lowercase as before).
- The NWS forecast popup clamps its minimum width to the window instead of a
  desktop-fixed 420 pt, and a period only puts its full
  "night — temp · sky · wind" line in the header when that line actually
  fits: on a phone the long lines were running off the right edge of the box
  (egui's collapsing headers never wrap).

**Android**
- **Voice alerts are available on Android too**: new warnings and watches are
  read through the system text-to-speech engine, using the same master and
  per-family switches and the same 50-mile GPS/marker rule as the desktop
  build.
- **Android Storm Watch (opt-in)**: a Settings -> Voice alerts toggle starts a
  foreground service with a persistent notification and its own TTS engine.
  It polls the last GPS fix every 60 seconds and speaks new warnings while
  Stormscope is closed (Android 15 caps `dataSync` foreground services at
  about 6 hours per day). The notification itself carries no buttons — the
  watcher is switched off from Settings -> Voice alerts.
- New warnings/watches **within 50 miles of the GPS fix** (polygon distance —
  zone-only alerts with no geometry never notify) now also **pop up as a text
  notification**: a high-importance channel (sound, vibration, red light)
  heads-ups the event as the title with the area, expiry and full instruction
  underneath, and tapping it opens Stormscope **on that alert's NOAA detail**
  with its polygon focused behind it. Posted whether or not the app is open
  and whether or not read-aloud is on; the per-family switches still filter
  which products notify, capped at six per batch.
  The persistent notification also doubles as a current-conditions readout
  (Open-Meteo: temperature, sky, wind, refreshed each poll), and tapping it
  opens the AI meteorologist and automatically asks it for a brief.
- **The AI meteorologist window fills the app screen** on a phone: it opens
  edge to edge between the app's status bar and timeline (which stay visible
  and usable — this is not the chrome-free fullscreen mode), with the whole
  transcript and composer in view and no resize grips. Desktop keeps the
  resizable floating window.
- **Long-press the map (or the 3D pane) for a true fullscreen view**: all
  chrome disappears — status bar, timeline, chips, badges and windows — and
  only the radar fills the screen. On Android the system status bar is hidden
  as well; long-press again (or press Esc on desktop) to bring it all back.
- **The system keyboard (Gboard) now types into the app.** winit's Android
  backend only shows/hides the IME and never forwards its text, so Stormscope
  polls GameActivity's editor state once per frame and replays the commits
  (insertions, deletions) as egui events at the top of the frame, before the
  focused field reads the event queue. The hand-drawn on-screen keyboard is
  gone. The window is `adjustResize`, and the AI window also lifts its
  bottom edge by the IME inset GameActivity reports, so Gboard never covers
  the question box while typing.
- **SPC outlooks show every product at once**: categorical risk areas keep
  their fills, and the Tornado, Wind and Hail probability bands are drawn
  as outlines together on the same map. The product selector now chooses
  which legend/discussion is emphasized, not which product is visible.
- **Rain & snow totals opens as a full-pane phone window**: same treatment
  as the AI and forecast windows (painted ✕, phone type scale, back gesture
  closes it), with the table scrolling inside it.
- **Alerts and NOAA alert details fill the app screen** like the forecast
  and totals windows: full pane, phone type scale, a painted ✕ and the back
  gesture to close. The five alert-view tabs wrap instead of running off
  the phone's right edge.
- Phone-window ✕ buttons now sit where they are hit: the old header painted
  the close box beside the title but hit-tested it at the far right, so a
  tap on the visible ✕ missed.
- **GPS fixes are now part of the app**: a fix is requested on launch, the
  map centres on it, the nearest radar station is selected and the forecast
  is pointed at the fix. The fix is drawn as a pulsing blue **You** dot on
  top of every map layer, with a manual crosshair button above the Controls
  chip for a fresh fix. The AI brief and chat sample the current GPS
  location even with no markers, so a phone with no pins still gets a
  brief.
- **No phantom snow in AI briefs**: surface temperature now outranks the
  radar's winter classifiers. Above 40 °F the snow-rate and melting-level
  fields are omitted and the prompt forbids snow/ice; weak echoes below
  20 dBZ with no echo regions are described as clutter/drizzle, not wet
  snow.
- **AI briefs speak plain English**: the prompt translates radar shorthand
  ("a few very light returns", "no hail signature") into everyday weather
  language while keeping the detail that matters — timing, amounts, wind
  and hail size — instead of dropping it.
- **The slide-out sidebar is phone-sized**: half the map width, a fixed
  header whose title and ✕ no longer clip at the top, and the app settings
  at the bottom of the scroll instead of behind a Settings menu.
- **SPC outlooks are touchable**: ALL is the default and draws the
  categorical, tornado, wind and hail products together; the risk outlines
  get a 40 px touch reach, and tapping any of them opens the full-pane
  forecaster discussion (back or ✕ closes).
- **The 3D pane shows the GPS fix** with the same pulsing blue **You**
  beacon as the 2D map, drawn on the volume's ground plane.
- **Forecast "Pick on map…" works on a phone**: the window minimises so the
  map is fully tappable, then reopens and loads the picked point. Back
  cancels the pick and restores the window.
- **Phone map chrome**: chips are larger, darker and rounded; the 3D opener
  and Split/Max chips sit at the bottom centre; the LIVE pill centres itself
  below a wrapped badge row; Settings and Controls flank the GPS button.
  Badges on the top row stretch to the full map width (labels stay centred
  in their pills) instead of leaving a wide empty strip at the right, and
  the 2D Map controls window is a compact 270×238 pt that scrolls only when
  its content needs it. Lightning is on by default, the 0 °C level is off,
  and the reflectivity floor starts at 20 dBZ.
- Window close (✕) buttons get a larger Heading font, growing their tap
  square from ~24 dp to ~35 dp — they were very hard to hit with a finger.
- The 3D radar pane stacks **below** the 2D map in portrait and sits beside
  it in landscape (the divider stays draggable, and is wider on touch).
  Touch controls: one finger orbits, two fingers pan, pinch zooms.
- The timeline scrub strip spans the full width of the bottom bar and is
  32 pt tall, so it can be dragged with a finger (after the control row it
  was only whatever space was left over — a few dozen pixels).
- The launcher icon is an adaptive icon on a dark background, so the system
  no longer puts a white ring around it.

## v1.5.0 — 2026-10-22

**Presentation**
- Stormscope now has an icon (window/taskbar, and the `.exe` itself on
  Windows), and the README opens with a full screenshot of the KFSX view.
- The AI meteorologist window keeps its full chat layout before a key is
  configured — composer and actions disabled with the setup hint in the
  transcript — instead of collapsing to just a paste-key message.
- Prose falls back to the bundled Hack font for arrows and triangles, so
  strings like "Settings → AI meteorologist" no longer render as boxes.

**Platform**
- **Windows support**: cache files go to `%LOCALAPPDATA%\stormscope`, config
  (AI settings, markers) to `%APPDATA%\stormscope`, the private AI file uses
  the platform's default ACLs, and the mono-font lookup and RSS debug readout
  fall back cleanly where those Linux paths do not exist. wgpu uses DX12, and
  CI now checks, tests and builds a `stormscope.exe` artifact on
  `windows-latest`. The release binary uses the GUI subsystem (no console
  window), the UI is pinned to the dark theme (a light Windows app theme no
  longer turns the AI/Controls windows white), and DWM is asked for a dark
  titlebar and border.
- **Android (early port)**: a GameActivity shell (`crates/stormscope-android`)
  and an `android/` Gradle project build a `com.stormscope.app` APK for
  arm64; cache/config live in the app's private storage and the animation
  ring is capped at 8 frames / 512 MB. Touch UI polish is still in progress.
