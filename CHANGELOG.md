# Changelog

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

## v1.8.8 — 2026-10-30

**Gemini voice for storm alerts**
- Linux and Windows now use the configured Gemini Live voice for spoken storm
  alerts. If no Gemini key is set, or Gemini fails, Stormscope falls back to
  the existing Piper voice (and then the system voice).
- Android alert speech now uses Gemini when a key is configured, including
  foreground NWS alerts and Storm Watch background alerts. With no key — or
  if Gemini is unavailable — the existing Android system TTS remains the
  fallback.
- Gemini key, model, voice and test controls are now available on desktop as
  well as Android.
  arm64; cache/config live in the app's private storage and the animation
  ring is capped at 8 frames / 512 MB. Touch UI polish is still in progress.
