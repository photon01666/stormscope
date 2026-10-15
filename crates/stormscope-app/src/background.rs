//! Background data coordination: owns the fetch/decode/alert threads and
//! shuttles results to the UI. Runs on its own thread (spawned by StormApp).
//!
//! This is the real data coordinator — no more heartbeat stubs. A site
//! change restarts the [`S3Source`] real-time poller; an archive request
//! lists + fetches that date's volumes off-thread. Everything heavy happens
//! here, never on the UI thread.

use crate::app::{DataCmd, DataMsg};
use chrono::{NaiveDate, Utc};
use radar_data::{AlertSource, DiskCache, OutlookSource, S3Source};
use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

/// User-Agent sent to every NOAA/SPC host (they require a descriptive UA:
/// app name + a way to reach an operator).
const USER_AGENT: &str = "stormscope/0.1 (Linux NEXRAD radar viewer)";

/// How far back an archive request reaches when listing a day's volumes.
const ARCHIVE_MAX_VOLUMES: usize = 24;

/// How often the SPC outlook is re-fetched. SPC issues outlooks only a few
/// times per day, so 10 minutes is plenty and costs ~6 requests/hour.
const OUTLOOK_REFRESH: Duration = Duration::from_secs(600);
/// NWS alerts re-fetch cadence (api.weather.gov suggests <=1 req/s; this
/// uses one every 5 minutes).
const ALERTS_REFRESH: Duration = Duration::from_secs(300);

/// The fast lane: alerts that contain the selected station, fetched with the
/// API's server-side point filter — a few hundred bytes per request, so a
/// minute's cadence costs ~10 KB/hour while cutting warning latency from five
/// minutes to one.
const ALERTS_FAST_REFRESH: Duration = Duration::from_secs(60);

/// An `Instant` that has *already* elapsed `d` ago. The monotonic clock can't
/// go before boot, so fall back to `now()` (fires the tick immediately anyway).
fn already_elapsed(d: Duration) -> std::time::Instant {
    std::time::Instant::now()
        .checked_sub(d)
        .unwrap_or_else(std::time::Instant::now)
}

/// Human-readable byte size (B/KiB/MiB/GiB) for status text.
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Inclusive-exclusive UTC instant window of a Central calendar day.
fn central_day_window(date: NaiveDate) -> (chrono::DateTime<Utc>, chrono::DateTime<Utc>) {
    use chrono::TimeZone;
    let start = crate::timefmt::central()
        .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .expect("central midnight")
        .with_timezone(&Utc);
    (start, start + chrono::Duration::days(1))
}

/// List a site's volumes for a *Central* calendar day, newest-first.
///
/// NOAA's S3 archive is keyed by UTC dates, but the user's "day" is Central:
/// the Central day D spans UTC [D 05:00, D+1 05:00) in summer (CDT), so the
/// day can touch two UTC date prefixes. Querying only the same-numbered UTC
/// day is why "the newest of Oct 11" ended at 6:55 PM CDT.
fn list_central_day(
    source: &S3Source,
    site: &str,
    date: NaiveDate,
) -> anyhow::Result<Vec<radar_data::ArchiveVolumeRef>> {
    let (start, end) = central_day_window(date);
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<radar_data::ArchiveVolumeRef> = Vec::new();
    for utc_date in [
        start.date_naive(),
        (end - chrono::Duration::seconds(1)).date_naive(),
    ] {
        for r in source.list_archive(site, utc_date)? {
            if r.volume_start >= start && r.volume_start < end && seen.insert(r.key.clone()) {
                out.push(r);
            }
        }
    }
    // Newest-first, matching the previous contract.
    out.sort_by(|a, b| b.volume_start.cmp(&a.volume_start));
    Ok(out)
}

/// One listed archive day plus where the loaded window starts in it.
struct ArchiveDay {
    vols: Vec<radar_data::ArchiveVolumeRef>,
    window_start: usize,
}

/// Entry point for the background "data" thread.
pub fn run(cmds: Receiver<DataCmd>, out: Sender<DataMsg>) {
    let cache = DiskCache::with_default_max(DiskCache::default_root());
    let source = match S3Source::new(cache) {
        Ok(s) => s,
        Err(e) => {
            let _ = out.send(DataMsg::Error(format!("data source init failed: {e:#}")));
            return;
        }
    };
    let alerts = AlertSource::new(USER_AGENT).ok();
    let outlooks = OutlookSource::new(USER_AGENT).ok();
    let forecasts = radar_data::ForecastSource::new(USER_AGENT).ok();
    // Report the current on-disk cache sizes so Settings has a number from
    // the first frame; the loop below keeps re-sending as they change.
    let mut cached_sizes = cache_sizes(&source);
    let _ = out.send(DataMsg::CacheInfo {
        radar: cached_sizes.0,
        tiles: cached_sizes.1,
    });
    // Which SPC outlook day the UI is asking for + when it was last refreshed
    // (outlooks update only a few times per day).
    let mut outlook_day: u8 = 1;
    // Start both timers pre-expired so the first loop iteration fetches
    // alerts + outlook immediately — startup never blocks on the network.
    let mut last_outlook = already_elapsed(OUTLOOK_REFRESH);
    let mut last_alerts = already_elapsed(ALERTS_REFRESH);
    let mut last_alerts_fast = already_elapsed(ALERTS_FAST_REFRESH);
    // Cache sizes for the Settings menu: recomputed every couple of seconds
    // (a full directory walk is far too expensive to run per 250 ms tick).
    let mut last_cache_report = already_elapsed(CACHE_INFO_REFRESH);
    const CACHE_INFO_REFRESH: Duration = Duration::from_secs(2);

    // Live poller for the currently selected site. Replaced on SetSite; the
    // old one exits when its receiver is dropped.
    let mut live: Option<(Receiver<radar_core::Volume>, radar_data::RealtimePoller)> = None;
    let mut current_site = radar_core::DEFAULT_SITE.to_string();
    // Listed archive day + loaded window, so ◀/▶ can page through the day
    // without re-listing S3.
    let mut archive: Option<ArchiveDay> = None;
    // Freshness tracking + archive catch-up watchdog: the app should show
    // whichever upstream (live chunks vs archive bucket) is AHEAD, not
    // whichever it happens to be listening to.
    let mut watchdog_enabled = true; // off while browsing archive/last-hour
    let mut last_watchdog = std::time::Instant::now();
    const ARCHIVE_CATCHUP_INTERVAL: Duration = Duration::from_secs(60);

    let _ = out.send(DataMsg::Status("starting live feed…".into()));
    start_live(&source, radar_core::DEFAULT_SITE, &mut live, &out);
    // Restore the recent scans already on disk (instant) and top up with the
    // newest, while the poller warms up in parallel. The watchdog below
    // compares against whatever this delivers first.
    let (mut last_delivered, queued) = prime_site(&source, radar_core::DEFAULT_SITE, &out);
    // Volumes waiting to replay into the ring; drained a few per loop tick so
    // commands stay responsive during the refill.
    let mut backfill: VecDeque<radar_data::ArchiveVolumeRef> = queued;
    // Alerts + SPC outlook are fetched by the first loop iteration (timers
    // start pre-expired), so startup never blocks on the network.

    // Dev harness: STORMSCOPE_BOOTSTRAP_ARCHIVE=YYYY-MM-DD[=SITE] pre-loads
    // an archive day at startup so the archive path can be exercised (and
    // demoed) without driving the GUI.
    if let Ok(spec) = std::env::var("STORMSCOPE_BOOTSTRAP_ARCHIVE") {
        let mut parts = spec.split('=');
        let date = parts
            .next()
            .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
        let site = parts.next().unwrap_or(radar_core::DEFAULT_SITE).to_string();
        if let Some(date) = date {
            let _ = out.send(DataMsg::Status(format!(
                "bootstrap: loading {site} {date}…"
            )));
            watchdog_enabled = false; // browsing a day, not live
            backfill.clear(); // the archive day replaces the queued refill
            archive = open_archive_day(&source, &site, date, &mut current_site, &out);
        }
    }

    // Drain on a timer, not just on commands: `recv()` would block forever
    // waiting for a UI command that never comes, leaving the poller's
    // channel full and the UI ring empty (that bug showed up as "no radar
    // data at all").
    loop {
        match cmds.recv_timeout(Duration::from_millis(250)) {
            Ok(cmd) => match cmd {
                DataCmd::SetSite(site) => {
                    let _ = out.send(DataMsg::Status(format!("switching to {site}…")));
                    // Drop the old station's frames FIRST — otherwise its
                    // sweeps render (georeferenced to the new site!) until
                    // the new station's data arrives.
                    let _ = out.send(DataMsg::ClearFrames);
                    current_site = site.clone();
                    start_live(&source, &site, &mut live, &out);
                    // Refill from what is already on disk so the bottom
                    // timeline can play back this station's data instead of
                    // showing a single frame; `prime_site` also refreshes the
                    // newest scan if the bucket has moved on.
                    backfill.clear();
                    let (now, queued) = prime_site(&source, &site, &out);
                    backfill = queued;
                    if let Some(ts) = now {
                        last_delivered = Some(ts);
                    }
                    // Alerts are point-filtered, so they move with the site
                    // (previously evaluated around a hardcoded Oklahoma point).
                    refresh_alerts(&alerts, &site, &out);
                }
                DataCmd::LoadArchiveDate(site, date) => {
                    let _ = out.send(DataMsg::Status(format!("loading {site} archive {date}…")));
                    watchdog_enabled = false; // browsing a day, not live
                    backfill.clear(); // the day replaces whatever was queued
                    archive = open_archive_day(&source, &site, date, &mut current_site, &out);
                }
                DataCmd::LoadLastHour(site) => {
                    current_site = site.clone();
                    archive = None;
                    watchdog_enabled = false; // browsing a fixed window
                    backfill.clear();
                    let _ = out.send(DataMsg::ArchiveInfo {
                        total: 0,
                        start: 0,
                        len: 0,
                    });
                    load_last_hour(&source, &site, &out);
                }
                DataCmd::ArchiveWindowShift(delta) => {
                    backfill.clear();
                    if let Some(day) = archive.as_mut() {
                        shift_archive_window(&source, day, delta, &out);
                    }
                }
                DataCmd::EnterLive => {
                    // Back to live: drop the archive listing, clear the ring,
                    // and show the newest scan immediately. The live poller
                    // keeps future scans coming.
                    archive = None;
                    watchdog_enabled = true;
                    let _ = out.send(DataMsg::ClearFrames);
                    let _ = out.send(DataMsg::Status("returning to live…".into()));
                    if live.is_none() {
                        start_live(&source, &current_site, &mut live, &out);
                    }
                    // Refill from the disk cache + the newest scan, so the
                    // timeline is playable again rather than one frame wide.
                    backfill.clear();
                    let (now, queued) = prime_site(&source, &current_site, &out);
                    backfill = queued;
                    if let Some(ts) = now {
                        last_delivered = Some(ts);
                    }
                }
                DataCmd::RefreshAlerts => {
                    refresh_alerts(&alerts, &current_site, &out);
                }
                DataCmd::LoadOutlookDiscussion(day) => {
                    match outlooks.as_ref().map(|o| o.discussion(day)) {
                        Some(Ok(text)) => {
                            let _ = out.send(DataMsg::OutlookDiscussion(day, text));
                        }
                        Some(Err(e)) => {
                            log::warn!("outlook discussion day {day} failed: {e:#}");
                            let _ = out.send(DataMsg::Error(format!("outlook text: {e:#}")));
                        }
                        None => {}
                    }
                }
                DataCmd::LoadOutlook(day) => {
                    outlook_day = day;
                    refresh_outlook(&outlooks, day, &out);
                }
                DataCmd::LoadForecast(lat, lon) => {
                    let _ = out.send(DataMsg::Status(format!(
                        "loading forecast for {lat:.3}, {lon:.3}…"
                    )));
                    refresh_forecast(&forecasts, lat, lon, &out);
                }
                DataCmd::ClearRadar => {
                    let _ = out.send(DataMsg::Status("clearing radar data…".into()));
                    // Only `l2/` — the basemap tiles and the in-memory ring of
                    // decoded frames are untouched, so the map keeps rendering;
                    // future scans re-download what they need.
                    let freed = match source.cache().clear_section("l2") {
                        Ok(n) => n,
                        Err(e) => {
                            let _ = out.send(DataMsg::Error(format!("clear radar data: {e}")));
                            0
                        }
                    };
                    log::info!("radar cache cleared: {} freed", human_bytes(freed));
                    let _ = out.send(DataMsg::Status(format!(
                        "radar data cleared ({})",
                        human_bytes(freed)
                    )));
                    cached_sizes = cache_sizes(&source);
                    let _ = out.send(DataMsg::CacheInfo {
                        radar: cached_sizes.0,
                        tiles: cached_sizes.1,
                    });
                }
                DataCmd::ClearTiles => {
                    let _ = out.send(DataMsg::Status("clearing map tiles…".into()));
                    // Only `tiles/`. The UI drops its decoded-texture LRU at
                    // the same moment, so tiles aren't served from memory
                    // after the files are gone.
                    let freed = match source.cache().clear_section("tiles") {
                        Ok(n) => n,
                        Err(e) => {
                            let _ = out.send(DataMsg::Error(format!("clear map tiles: {e}")));
                            0
                        }
                    };
                    log::info!("tile cache cleared: {} freed", human_bytes(freed));
                    let _ = out.send(DataMsg::Status(format!(
                        "map tiles cleared ({})",
                        human_bytes(freed)
                    )));
                    cached_sizes = cache_sizes(&source);
                    let _ = out.send(DataMsg::CacheInfo {
                        radar: cached_sizes.0,
                        tiles: cached_sizes.1,
                    });
                }
                DataCmd::Shutdown => break,
            },
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }

        // Replay the queued refill one slice at a time. Decoding costs ~1 s
        // per volume, so draining it inside the command handler would stall
        // the next command — and the next site switch — for tens of seconds.
        if !backfill.is_empty() {
            const BACKFILL_SLICE: Duration = Duration::from_millis(150);
            let deadline = std::time::Instant::now() + BACKFILL_SLICE;
            while let Some(r) = backfill.pop_front() {
                let _ = emit_volume(&source, &r, &out);
                if std::time::Instant::now() >= deadline {
                    break;
                }
            }
            if backfill.is_empty() {
                if let Some(ts) = last_delivered {
                    let _ = out.send(DataMsg::Status(format!(
                        "live · {}",
                        crate::timefmt::fmt_central(&ts)
                    )));
                }
            }
        }

        // Always drain whatever the live poller has produced this tick, so
        // volumes flow to the UI even when no command ever arrives.
        if let Some(ts) = drain_live(&mut live, &out) {
            last_delivered = Some(ts);
        }

        // Archive catch-up watchdog: if the archive bucket already has a
        // newer scan than the newest shown, take it — the app is then only
        // as slow as the fastest upstream. ~2 S3 list req/min, no rate risk.
        if watchdog_enabled && last_watchdog.elapsed() >= ARCHIVE_CATCHUP_INTERVAL {
            last_watchdog = std::time::Instant::now();
            if let Some(ts) = archive_catchup(&source, &current_site, last_delivered, &out) {
                last_delivered = Some(ts);
            }
        }

        // Timed refreshes (not user-driven): keep alerts + the SPC outlook
        // current without hammering anything.
        if last_alerts_fast.elapsed() >= ALERTS_FAST_REFRESH {
            last_alerts_fast = std::time::Instant::now();
            refresh_alerts_at_station(&alerts, &current_site, &out);
        }
        if last_alerts.elapsed() >= ALERTS_REFRESH {
            last_alerts = std::time::Instant::now();
            refresh_alerts(&alerts, &current_site, &out);
        }
        if last_outlook.elapsed() >= OUTLOOK_REFRESH {
            last_outlook = std::time::Instant::now();
            refresh_outlook(&outlooks, outlook_day, &out);
        }
        // Keep the Settings cache readout live. Only send on an actual
        // change — a directory walk every 2s that says the same thing would
        // just churn the message channel.
        if last_cache_report.elapsed() >= CACHE_INFO_REFRESH {
            last_cache_report = std::time::Instant::now();
            let sizes = cache_sizes(&source);
            if sizes != cached_sizes {
                cached_sizes = sizes;
                let _ = out.send(DataMsg::CacheInfo {
                    radar: sizes.0,
                    tiles: sizes.1,
                });
            }
        }
    }
}

/// On-disk usage per cache section: `(radar volumes, basemap tiles)`.
/// Counts only files, so a failed walk reads as 0 rather than stalling
/// the readout.
fn cache_sizes(source: &S3Source) -> (u64, u64) {
    let cache = source.cache();
    (
        cache.section_bytes("l2").unwrap_or(0),
        cache.section_bytes("tiles").unwrap_or(0),
    )
}

/// List a Central day, remember it for window paging, and stream the newest
/// window into the UI ring.
fn open_archive_day(
    source: &S3Source,
    site: &str,
    date: NaiveDate,
    current_site: &mut String,
    out: &Sender<DataMsg>,
) -> Option<ArchiveDay> {
    *current_site = site.to_string();
    let vols = match list_central_day(source, site, date) {
        Ok(v) => v,
        Err(e) => {
            let _ = out.send(DataMsg::Error(format!("archive {site} {date}: {e:#}")));
            return None;
        }
    };
    if vols.is_empty() {
        let _ = out.send(DataMsg::Status(format!("no scans for {site} on {date}")));
        return None;
    }
    let total = vols.len();
    let window_len = ARCHIVE_MAX_VOLUMES.min(total);
    // vols is newest-first; the newest window is the head.
    let day = ArchiveDay {
        vols,
        window_start: 0,
    };
    // The day *replaces* whatever was on the ring — without this the live
    // frame still sitting there made every older archive scan look
    // out-of-order and get dropped.
    let _ = out.send(DataMsg::ClearFrames);
    stream_window(source, site, date, &day, window_len, out);
    let _ = out.send(DataMsg::ArchiveInfo {
        total,
        start: 0,
        len: window_len,
    });
    Some(day)
}

/// Page the archive window earlier (delta<0) or later (delta>0) by one
/// window, clear the ring, and stream the new window.
fn shift_archive_window(
    source: &S3Source,
    day: &mut ArchiveDay,
    delta: i32,
    out: &Sender<DataMsg>,
) {
    let total = day.vols.len();
    if total == 0 {
        return;
    }
    let window_len = ARCHIVE_MAX_VOLUMES.min(total);
    let max_start = total - window_len;
    let new_start =
        (day.window_start as i32 + delta * window_len as i32).clamp(0, max_start as i32) as usize;
    if new_start == day.window_start {
        let _ = out.send(DataMsg::Status("already at the end of that day".into()));
        return;
    }
    day.window_start = new_start;
    let site = day.vols[0].site.clone();
    let date = day.vols[0].date;
    let _ = out.send(DataMsg::ClearFrames);
    stream_window(source, &site, date, day, window_len, out);
    let _ = out.send(DataMsg::ArchiveInfo {
        total,
        start: new_start,
        len: window_len,
    });
}

/// Fetch+emit one window's volumes oldest→newest into the UI ring.
fn stream_window(
    source: &S3Source,
    site: &str,
    date: NaiveDate,
    day: &ArchiveDay,
    window_len: usize,
    out: &Sender<DataMsg>,
) {
    // vols newest-first: the window slice [start..start+len] played in
    // reverse gives oldest→newest forward playback.
    let slice = &day.vols[day.window_start..day.window_start + window_len];
    let _ = out.send(DataMsg::Status(format!(
        "loading {site} {} scans {}–{}…",
        date.format("%b %-d"),
        day.window_start + 1,
        day.window_start + window_len
    )));
    for r in slice.iter().rev() {
        match source.fetch_archive(r) {
            Ok(volume) => {
                let _ = out.send(DataMsg::Volume(volume));
            }
            Err(e) => {
                log::warn!("archive fetch {} failed: {e:#}", r.file_name());
                let _ = out.send(DataMsg::Error(format!("{}: {e:#}", r.file_name())));
            }
        }
        std::thread::sleep(Duration::from_millis(120));
    }
}

/// Is this volume already in the on-disk cache? Purely a filesystem stat —
/// no S3 request, which is what makes switching back to an already-seen
/// station cost nothing.
fn is_cached(source: &S3Source, r: &radar_data::ArchiveVolumeRef) -> bool {
    source
        .cache()
        .path_for(&r.cache_rel())
        .map(|p| p.is_file())
        .unwrap_or(false)
}

/// Fetch + emit one archive volume; returns its scan time (or `None`, having
/// reported the failure).
fn emit_volume(
    source: &S3Source,
    r: &radar_data::ArchiveVolumeRef,
    out: &Sender<DataMsg>,
) -> Option<chrono::DateTime<Utc>> {
    match source.fetch_archive(r) {
        Ok(v) => {
            let _ = out.send(DataMsg::Volume(v));
            Some(r.volume_start)
        }
        Err(e) => {
            log::warn!("fetch {} failed: {e:#}", r.file_name());
            let _ = out.send(DataMsg::Error(format!("{}: {e:#}", r.file_name())));
            None
        }
    }
}

/// Plan the ring refill for a freshly selected station (or a return to live),
/// from a single archive listing.
///
/// Switching sites clears the ring — the frames are georeferenced to the old
/// station — but the *volumes* behind them are still in the disk cache. This
/// puts them back instead of leaving the bottom timeline with one frame:
///
///  1. the newest scan in the bucket is emitted right away, fetched only when
///     not already on disk, so the map is current while the rest fills in;
///  2. every recent scan already on disk is queued for replay with **no**
///     downloads, so the timeline can play back the data on hand.
///
/// The queue is drained a few volumes per loop tick by the caller: decoding
/// costs ~1 s each, so replaying it inline would stall the next site switch.
///
/// Returns `(scan time delivered now, volumes still queued)`.
fn prime_site(
    source: &S3Source,
    site: &str,
    out: &Sender<DataMsg>,
) -> (
    Option<chrono::DateTime<Utc>>,
    VecDeque<radar_data::ArchiveVolumeRef>,
) {
    const PRIME_SCANS: usize = crate::state::RING_CAPACITY;
    let today = chrono::Utc::now()
        .with_timezone(crate::timefmt::central())
        .date_naive();
    // Newest-first for today, topped up from yesterday while the Central day
    // has barely started (same fill rule as "load last hour").
    let mut vols = list_central_day(source, site, today).unwrap_or_default();
    if vols.len() < PRIME_SCANS {
        if let Some(yday) = today.pred_opt() {
            for r in list_central_day(source, site, yday).unwrap_or_default() {
                if vols.len() >= PRIME_SCANS {
                    break;
                }
                vols.push(r);
            }
        }
    }
    vols.truncate(PRIME_SCANS);
    if vols.is_empty() {
        log::warn!("prime {site}: no scans listed");
        return (None, VecDeque::new());
    }

    // What is already on disk, newest-first (the listing's own order).
    let cached: Vec<&radar_data::ArchiveVolumeRef> =
        vols.iter().filter(|r| is_cached(source, r)).collect();
    let newest = vols[0].volume_start;
    let newest_is_cached = cached.first().map(|r| r.volume_start) == Some(newest);

    // Drop whatever is on the ring before pushing, or a frame from the
    // previous station lingers under the new one.
    let _ = out.send(DataMsg::ClearFrames);

    let mut delivered = None;
    if !newest_is_cached {
        // The first frame the user sees after a switch: get it moving before
        // the backfill starts.
        delivered = emit_volume(source, &vols[0], out);
        if delivered.is_some() {
            log::info!(
                "live refresh: scan {}",
                crate::timefmt::fmt_central(&newest)
            );
        }
    }

    let queue: VecDeque<radar_data::ArchiveVolumeRef> = cached.into_iter().cloned().collect();
    if !queue.is_empty() {
        log::info!(
            "prime {site}: {} scans already on disk, newest {}",
            queue.len(),
            crate::timefmt::fmt_central(&newest)
        );
        let _ = out.send(DataMsg::Status(format!(
            "restoring {} cached scans…",
            queue.len()
        )));
    }
    // When the newest is already cached it heads the queue, so report it as
    // delivered for freshness tracking even though it hasn't replayed yet.
    let newest_cached = queue.front().map(|r| r.volume_start);
    (delivered.or(newest_cached), queue)
}

/// One polite catch-up check: if the archive bucket has a scan *newer* than
/// the newest shown, fetch and emit it now. Combined with the live
/// chunk poller, the app is then only as slow as the FASTEST upstream instead
/// of whichever one it happened to be listening to. Cost: ~2 S3 list
/// requests per interval — far below any rate limit (and unrelated to the
/// tile server's limits).
fn archive_catchup(
    source: &S3Source,
    site: &str,
    last_delivered: Option<chrono::DateTime<Utc>>,
    out: &Sender<DataMsg>,
) -> Option<chrono::DateTime<Utc>> {
    let today = chrono::Utc::now()
        .with_timezone(crate::timefmt::central())
        .date_naive();
    let vols = match list_central_day(source, site, today) {
        Ok(v) => v,
        Err(e) => {
            log::debug!("catchup list failed: {e:#}");
            return None;
        }
    };
    let newest = vols.first()?.volume_start;
    if Some(newest) <= last_delivered {
        return None; // already current
    }
    match source.fetch_archive(vols.first()?) {
        Ok(v) => {
            log::info!(
                "catchup: archive ahead of live feed — scan {}",
                crate::timefmt::fmt_central(&newest)
            );
            let _ = out.send(DataMsg::Volume(v));
            let _ = out.send(DataMsg::Status(format!(
                "live · {}",
                crate::timefmt::fmt_central(&newest)
            )));
            Some(newest)
        }
        Err(e) => {
            log::warn!("catchup fetch failed: {e:#}");
            None
        }
    }
}

/// Load the newest ~1 hour of scans (`LAST_HOUR_SCANS`, ~13 at NEXRAD's
/// ~4.5-min cadence) into the ring, oldest→newest, for immediate looping.
/// Clears the ring first so playback covers exactly the recent hour.
fn load_last_hour(source: &S3Source, site: &str, out: &Sender<DataMsg>) {
    const LAST_HOUR_SCANS: usize = 13;
    let today = chrono::Utc::now()
        .with_timezone(crate::timefmt::central())
        .date_naive();
    let mut vols = list_central_day(source, site, today).unwrap_or_default();
    // Early morning Central: today has few scans, so top up from yesterday's
    // newest so "last hour" always fills.
    if vols.len() < LAST_HOUR_SCANS {
        if let Some(yday) = today.pred_opt() {
            let y = list_central_day(source, site, yday).unwrap_or_default();
            for r in y {
                if vols.len() >= LAST_HOUR_SCANS {
                    break;
                }
                vols.push(r);
            }
        }
    }
    vols.truncate(LAST_HOUR_SCANS);
    if vols.is_empty() {
        let _ = out.send(DataMsg::Status(format!("no recent scans for {site}")));
        return;
    }
    let _ = out.send(DataMsg::ClearFrames);
    let _ = out.send(DataMsg::Status(format!(
        "loading last hour ({})…",
        vols.len()
    )));
    // vols is newest-first; reverse for forward playback.
    for r in vols.iter().rev() {
        match source.fetch_archive(r) {
            Ok(volume) => {
                let _ = out.send(DataMsg::Volume(volume));
            }
            Err(e) => {
                log::warn!("last-hour fetch {} failed: {e:#}", r.file_name());
                let _ = out.send(DataMsg::Error(format!("{}: {e:#}", r.file_name())));
            }
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    log::info!("last-hour loop loaded for {site}");
}

/// Restart the real-time poller for `site`.
fn start_live(
    source: &S3Source,
    site: &str,
    live: &mut Option<(Receiver<radar_core::Volume>, radar_data::RealtimePoller)>,
    out: &Sender<DataMsg>,
) {
    // Replacing the Option drops the previous RealtimePoller handle, which
    // cancels that poller thread — otherwise every site switch would leak a
    // poller that keeps hitting S3 (that leak is what got us rate-limited).
    *live = None;
    match source.start_realtime(site) {
        Ok(pair) => {
            *live = Some(pair);
            let _ = out.send(DataMsg::Status(format!("live {site}: polling…")));
        }
        Err(e) => {
            let _ = out.send(DataMsg::Error(format!("live {site}: {e:#}")));
        }
    }
}

/// Forward any volumes the live poller has produced. Non-blocking.
/// Returns the newest delivered scan time this tick (for freshness tracking).
fn drain_live(
    live: &mut Option<(Receiver<radar_core::Volume>, radar_data::RealtimePoller)>,
    out: &Sender<DataMsg>,
) -> Option<chrono::DateTime<Utc>> {
    let Some((rx, _poller)) = live else {
        return None;
    };
    let mut newest: Option<chrono::DateTime<Utc>> = None;
    loop {
        match rx.try_recv() {
            Ok(volume) => {
                let scan = volume.meta.volume_start;
                log::info!("delivered live scan {}", crate::timefmt::fmt_central(&scan));
                newest = Some(scan);
                let _ = out.send(DataMsg::Volume(std::sync::Arc::new(volume)));
                let _ = out.send(DataMsg::Status(format!(
                    "live scan {}",
                    crate::timefmt::fmt_central(&scan)
                )));
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                let _ = out.send(DataMsg::Status("live feed ended".into()));
                *live = None;
                break;
            }
        }
    }
    newest
}

/// Re-fetch active NWS alerts around the current site and emit them. Zones
/// are looked up from the site catalog instead of a hardcoded point (the old
/// default evaluated alerts around Norman, OK no matter where the user was).
/// The minute-cadence check for alerts that apply to the station itself.
/// Errors are logged at debug: a failed fast poll costs nothing and the slow
/// sweep will still run.
fn refresh_alerts_at_station(alerts: &Option<AlertSource>, site: &str, out: &Sender<DataMsg>) {
    let Some(a) = alerts else { return };
    let Some(site) = radar_core::sites::site(site) else {
        return;
    };
    match a.active_at_point(site.latitude, site.longitude) {
        Ok(w) => {
            log::debug!("{} alerts apply to {}", w.len(), site.id);
            let _ = out.send(DataMsg::AlertsFast(w));
        }
        Err(e) => log::debug!("station alert check failed: {e:#}"),
    }
}

fn refresh_alerts(alerts: &Option<AlertSource>, site: &str, out: &Sender<DataMsg>) {
    let Some(a) = alerts else { return };
    let (lat, lon) = radar_core::sites::site(site)
        .map(|s| (s.latitude, s.longitude))
        .unwrap_or((35.3331, -97.2778));
    match a.active_near(lat, lon, crate::state::STATION_ALERT_RADIUS_KM) {
        Ok(w) => {
            let n = w.len();
            let _ = out.send(DataMsg::Alerts(w));
            let _ = out.send(DataMsg::Status(format!("{n} active alerts")));
        }
        Err(e) => {
            log::warn!("alerts fetch failed: {e:#}");
            let _ = out.send(DataMsg::Error(format!("alerts: {e:#}")));
        }
    }
    // The alerts browser also wants the nationwide picture. Same request
    // cadence, one extra listing per 5 minutes.
    match a.active_all() {
        Ok(w) => {
            log::debug!("{} active alerts nationwide", w.len());
            let _ = out.send(DataMsg::AllAlerts(w));
        }
        Err(e) => log::warn!("nationwide alerts fetch failed: {e:#}"),
    }
}

/// Re-fetch the SPC categorical outlook for `day` and emit it. CONUS-wide by
/// nature, so it doesn't need the site; one GET every 10 min.
fn refresh_forecast(
    forecasts: &Option<radar_data::ForecastSource>,
    lat: f64,
    lon: f64,
    out: &Sender<DataMsg>,
) {
    let Some(f) = forecasts else { return };
    let (forecast, observation) = f.snapshot(lat, lon);
    match forecast {
        Ok(fc) => {
            let _ = out.send(DataMsg::Forecast(fc));
        }
        Err(e) => {
            log::warn!("forecast fetch failed: {e:#}");
            let _ = out.send(DataMsg::Error(format!("forecast: {e:#}")));
        }
    }
    // Current conditions are a bonus: not every point has a close station.
    match observation {
        Some(Ok(o)) => {
            let _ = out.send(DataMsg::Observation(o));
        }
        Some(Err(e)) => log::debug!("observation fetch failed: {e:#}"),
        None => {}
    }
}

fn refresh_outlook(outlooks: &Option<OutlookSource>, day: u8, out: &Sender<DataMsg>) {
    let Some(src) = outlooks else { return };
    match src.fetch_day(day) {
        Ok(o) => {
            let issued = o
                .issue
                .map(|t| crate::timefmt::fmt_central(&t))
                .unwrap_or_else(|| "?".into());
            log::info!(
                "outlook day {day}: {} areas, issued {issued}",
                o.features.len()
            );
            let _ = out.send(DataMsg::Outlook(o));
        }
        Err(e) => {
            log::warn!("outlook day {day} fetch failed: {e:#}");
            let _ = out.send(DataMsg::Error(format!("outlook day {day}: {e:#}")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: a Central calendar day starts at midnight CENTRAL, not at
    /// midnight UTC. `Utc.from_utc_datetime(date.and_hms(0,0,0))` is the
    /// tempting-but-wrong construction — it lands on 6 PM of the *previous*
    /// day in Chicago and made the listing query the previous UTC day, so the
    /// newest scans (after ~7 PM Central, which live under the *next* UTC
    /// date prefix) were invisible and the app sat ~2 h stale.
    #[test]
    fn central_day_window_covers_the_central_day() {
        use chrono::TimeZone;

        // CDT (UTC-5): a Central day spans 05:00Z..05:00Z.
        let date = NaiveDate::from_ymd_opt(2026, 10, 13).unwrap();
        let (start, end) = central_day_window(date);
        assert_eq!(
            start,
            Utc.from_utc_datetime(&date.and_hms_opt(5, 0, 0).unwrap())
        );
        assert_eq!(
            end,
            Utc.from_utc_datetime(&date.succ_opt().unwrap().and_hms_opt(5, 0, 0).unwrap())
        );
        // The two UTC prefixes the window can touch: the date itself and the
        // next day. The old bug returned the *previous* day here.
        let last_instant = end - chrono::Duration::seconds(1);
        assert_eq!(start.date_naive(), date);
        assert_eq!(last_instant.date_naive(), date.succ_opt().unwrap());

        // CST (UTC-6): winter days span 06:00Z..06:00Z.
        let winter = NaiveDate::from_ymd_opt(2026, 12, 10).unwrap();
        let (wstart, wend) = central_day_window(winter);
        assert_eq!(
            wstart,
            Utc.from_utc_datetime(&winter.and_hms_opt(6, 0, 0).unwrap())
        );
        assert_eq!(
            wend,
            Utc.from_utc_datetime(&winter.succ_opt().unwrap().and_hms_opt(6, 0, 0).unwrap())
        );
    }
}
