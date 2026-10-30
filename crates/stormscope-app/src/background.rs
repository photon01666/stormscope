//! Background data coordination: owns the fetch/decode/alert threads and
//! shuttles results to the UI. Runs on its own thread (spawned by StormApp).
//!
//! This is the real data coordinator — no more heartbeat stubs. A site
//! change restarts the [`S3Source`] real-time poller; an archive request
//! lists + fetches that date's volumes off-thread. Everything heavy happens
//! here, never on the UI thread.

use crate::app::{DataCmd, DataMsg};
use chrono::{NaiveDate, Utc};
use radar_data::{AlertSource, DiskCache, OutlookSource, S3Source, ZoneSource};
use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// User-Agent sent to every NOAA/SPC host (they require a descriptive UA:
/// app name + a way to reach an operator).
const USER_AGENT: &str = "stormscope/0.1 (+https://github.com/photon01666/stormscope)";

/// How far back an archive request reaches when listing a day's volumes.
const ARCHIVE_MAX_VOLUMES: usize = 24;

/// How often the SPC outlook is re-fetched. SPC issues outlooks only a few
/// times per day, so 10 minutes is plenty and costs ~6 requests/hour.
const OUTLOOK_REFRESH: Duration = Duration::from_secs(600);
/// NWS alerts re-fetch cadence (api.weather.gov suggests <=1 req/s; this
/// uses one every 5 minutes).
const ALERTS_REFRESH: Duration = Duration::from_secs(300);

/// Storm reports and mesoscale discussions are short-fused (a report lands
/// within minutes of the event, an MD lives about an hour), so both ride the
/// same five-minute cadence as the alert sweep.
const REPORTS_REFRESH: Duration = Duration::from_secs(300);
const MCD_REFRESH: Duration = Duration::from_secs(300);

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

/// True when the Android activity is no longer visible. Desktop builds are
/// always considered foreground. Used to stop UI-originated live/prime work
/// from leaking into the background; the explicit Storm Watch gate below owns
/// background radar.
fn backgrounded() -> bool {
    #[cfg(target_os = "android")]
    {
        !crate::android_lifecycle::is_foreground()
    }
    #[cfg(not(target_os = "android"))]
    {
        false
    }
}

/// Entry point for the background "data" thread.
pub fn run(
    cmds: Receiver<DataCmd>,
    out: Sender<DataMsg>,
    archive_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[allow(unused_variables)] shared: Arc<Mutex<crate::app::SharedData>>,
) {
    let cache = DiskCache::with_default_max(DiskCache::default_root());
    #[cfg(target_os = "android")]
    let _ = cache.sweep_radar_age(chrono::Duration::hours(2));
    // Warning-archive backfill (opt-in, newest-first, rate-limited). It
    // lives beside the data cache, not inside it: day-files are immutable.
    let mut crawl = radar_data::warnings_archive::ArchiveCrawler::new(
        DiskCache::default_root().join("warnings"),
    );
    let _ = out.send(DataMsg::ArchiveCrawl(crawl.status()));
    // Historical warnings share the day-file cache with the crawler.
    let warn_archive =
        std::sync::Arc::new(radar_data::warnings_archive::WarningArchiveSource::new(
            DiskCache::default_root().join("warnings"),
        ));
    let source = match S3Source::new(cache) {
        Ok(s) => s,
        Err(e) => {
            let _ = out.send(DataMsg::Error(format!("data source init failed: {e:#}")));
            return;
        }
    };
    let alerts = AlertSource::new(USER_AGENT).ok();
    let mut zones = ZoneSource::new(USER_AGENT).ok();
    let outlooks = OutlookSource::new(USER_AGENT).ok();
    let mut synoptic_token: Option<String> = None;
    let mut ambient_keys: Option<(String, String)> = None;
    let forecasts = radar_data::ForecastSource::new(USER_AGENT).ok();
    // Both feeds hold caches (report texts, MCD pages) and a cold fill can
    // take tens of seconds (up to 80 product texts), so each poll runs on a
    // worker thread: measured cold-start fill blocked this loop for ~40 s,
    // during which radar and alert commands waited behind it. The mutex
    // serializes overlapping polls of the same feed instead of double-fetching.
    let reports = radar_data::LsrSource::new(USER_AGENT)
        .ok()
        .map(|s| std::sync::Arc::new(std::sync::Mutex::new(s)));
    let mcds = radar_data::McdSource::new(USER_AGENT)
        .ok()
        .map(|s| std::sync::Arc::new(std::sync::Mutex::new(s)));
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
    let mut outlook_type = radar_core::outlooks::OutlookType::Categorical;
    // Start both timers pre-expired so the first loop iteration fetches
    // alerts + outlook immediately — startup never blocks on the network.
    let mut last_outlook = already_elapsed(OUTLOOK_REFRESH);
    let mut last_alerts = already_elapsed(ALERTS_REFRESH);
    let mut last_alerts_fast = already_elapsed(ALERTS_FAST_REFRESH);
    let mut last_reports = already_elapsed(REPORTS_REFRESH);
    let mut last_mcds = already_elapsed(MCD_REFRESH);
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
    // Progress bookkeeping for the pill (counts only; reset when a fill
    // completes so the next one starts clean).
    let mut fill_total: u32 = 0;
    let mut fill_done: u32 = 0;
    let mut fill_label = String::new();
    // Alerts + SPC outlook are fetched by the first loop iteration (timers
    // start pre-expired), so startup never blocks on the network.

    // Android background radar gate. Storm Watch stays enabled; the radar
    // feed only keeps running while a watch or warning covers the GPS fix or
    // a map marker. The check is repeated while backgrounded so the feed
    // stops as soon as the last watch/warning expires.
    #[cfg(target_os = "android")]
    let mut background_covered = false;
    #[cfg(target_os = "android")]
    let mut background_exact_covered = false;
    #[cfg(target_os = "android")]
    let mut background_near_covered = false;
    #[cfg(target_os = "android")]
    let mut background_points: Vec<(f64, f64)> = Vec::new();
    #[cfg(target_os = "android")]
    let mut background_exact_checked_at = already_elapsed(Duration::from_secs(60));
    #[cfg(target_os = "android")]
    let mut background_near_checked_at = already_elapsed(Duration::from_secs(300));
    #[cfg(target_os = "android")]
    let mut background_active = false;

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
            archive = open_archive_day(
                &source,
                &site,
                date,
                None,
                &mut current_site,
                &out,
                &archive_cancel,
            );
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
                    if backgrounded() {
                        // A site selection queued just before the app left the
                        // foreground must not start downloading. The Storm Watch
                        // gate starts the feed only if an alert covers a point.
                        live = None;
                        backfill.clear();
                        let _ = out.send(DataMsg::Status(
                            "site selected; background radar waits for a watch/warning".into(),
                        ));
                    } else {
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
                }
                DataCmd::LoadArchiveDate(site, date, minute) => {
                    let _ = out.send(DataMsg::Status(match minute {
                        Some(m) => format!(
                            "loading {site} {date} around {:02}:{:02} CDT…",
                            m / 60,
                            m % 60
                        ),
                        None => format!("loading {site} archive {date}…"),
                    }));
                    watchdog_enabled = false; // browsing a day, not live
                    backfill.clear(); // the day replaces whatever was queued
                    archive_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                    archive = open_archive_day(
                        &source,
                        &site,
                        date,
                        minute,
                        &mut current_site,
                        &out,
                        &archive_cancel,
                    );
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
                DataCmd::LoadLastTwoHours(site, newest) => {
                    current_site = site.clone();
                    archive = None;
                    watchdog_enabled = false;
                    backfill.clear();
                    let _ = out.send(DataMsg::ArchiveInfo {
                        total: 0,
                        start: 0,
                        len: 0,
                    });
                    load_last_two_hours(&source, &site, newest, &out);
                }
                DataCmd::SetArchiveCrawl(on) => {
                    crawl.set_enabled(on);
                    let _ = out.send(DataMsg::ArchiveCrawl(crawl.status()));
                }
                DataCmd::LoadArchiveText(id) => {
                    let src = std::sync::Arc::clone(&warn_archive);
                    let out2 = out.clone();
                    std::thread::Builder::new()
                        .name("warn-text".into())
                        .spawn(move || match src.product_text(&id) {
                            Ok(text) => {
                                let _ = out2.send(DataMsg::ArchiveText {
                                    id,
                                    text: Some(text),
                                });
                            }
                            Err(e) => {
                                let _ =
                                    out2.send(DataMsg::Error(format!("warning text {id}: {e:#}")));
                                let _ = out2.send(DataMsg::ArchiveText { id, text: None });
                            }
                        })
                        .ok();
                }
                DataCmd::DeleteCachedDay { site, date } => {
                    let out = out.clone();
                    std::thread::spawn(move || {
                        let central = crate::timefmt::central();
                        let scans: Vec<radar_data::cache::CachedVolume> =
                            radar_data::cache::list_cached_volumes(&site)
                                .into_iter()
                                .filter(|v| v.start.with_timezone(central).date_naive() == date)
                                .collect();
                        let mut removed = 0;
                        let mut bytes = 0;
                        for scan in scans {
                            if std::fs::remove_file(&scan.path).is_ok() {
                                removed += 1;
                                bytes += scan.bytes;
                            }
                        }
                        let _ = out.send(DataMsg::CachedDeleted {
                            date,
                            removed,
                            bytes,
                        });
                    });
                }
                DataCmd::LoadCachedDay { site, date, target } => {
                    // Replay cached `.gz` volumes for one Central day directly
                    // from disk. No S3 listing, no downloads, no failed-object
                    // errors from scans that were never cached.
                    let out = out.clone();
                    let cancel = std::sync::Arc::clone(&archive_cancel);
                    cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                    std::thread::spawn(move || {
                        let central = crate::timefmt::central();
                        let day: Vec<radar_data::cache::CachedVolume> =
                            radar_data::cache::list_cached_volumes(&site)
                                .into_iter()
                                .filter(|v| v.start.with_timezone(central).date_naive() == date)
                                .collect();
                        if day.is_empty() {
                            let _ = out
                                .send(DataMsg::Error(format!("no cached {site} scans for {date}")));
                            return;
                        }
                        let len = crate::state::RING_CAPACITY.min(day.len());
                        let center = day
                            .partition_point(|v| v.start < target)
                            .min(day.len().saturating_sub(1));
                        let start = center
                            .saturating_sub(len / 2)
                            .min(day.len().saturating_sub(len));
                        let window = &day[start..start + len];
                        let _ = out.send(DataMsg::ClearFrames);
                        let label = format!("{site} radar · {date}");
                        let _ = out.send(DataMsg::LoadProgress {
                            label: label.clone(),
                            done: 0,
                            total: window.len() as u32,
                            cached: true,
                        });
                        for (i, volume) in window.iter().enumerate() {
                            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                                let _ = out.send(DataMsg::Status(
                                    "cached load cancelled — back to live".into(),
                                ));
                                return;
                            }
                            let decoded = std::fs::read(&volume.path)
                                .map_err(|e| anyhow::anyhow!("{}: {e}", volume.path.display()))
                                .and_then(|bytes| {
                                    radar_decode::decode_volume_file(&bytes).map_err(|e| {
                                        anyhow::anyhow!("{}: {e}", volume.path.display())
                                    })
                                });
                            match decoded {
                                Ok(mut decoded) => {
                                    radar_core::derived::attach_derived(&mut decoded);
                                    let _ = out.send(DataMsg::Volume(std::sync::Arc::new(decoded)));
                                }
                                Err(e) => {
                                    log::warn!("cached scan: {e:#}");
                                    let _ = out.send(DataMsg::Error(format!("{e:#}")));
                                }
                            }
                            let _ = out.send(DataMsg::LoadProgress {
                                label: label.clone(),
                                done: (i + 1) as u32,
                                total: window.len() as u32,
                                cached: true,
                            });
                        }
                        let _ = out.send(DataMsg::ArchiveInfo {
                            total: day.len(),
                            start,
                            len: window.len(),
                        });
                        let _ = out.send(DataMsg::Status(format!(
                            "cached {site} · {} scans for {date}",
                            day.len()
                        )));
                    });
                }
                DataCmd::LoadArchiveWarnings(date) => {
                    // Off-thread: a cold day is one (sometimes two) small
                    // archive requests plus parsing — never on the loop.
                    let src = std::sync::Arc::clone(&warn_archive);
                    let out2 = out.clone();
                    std::thread::Builder::new()
                        .name("warn-archive".into())
                        .spawn(move || {
                            let _ = out2.send(DataMsg::Status(format!(
                                "loading warning archive for {date}…"
                            )));
                            let (start, end) = central_day_window(date);
                            let mut days = vec![start.date_naive()];
                            let last = (end - chrono::Duration::seconds(1)).date_naive();
                            if last != days[0] {
                                days.push(last);
                            }
                            let mut all: Vec<radar_data::warnings_archive::HistoricWarning> =
                                Vec::new();
                            for d in days {
                                match src.load_day(d) {
                                    Ok(mut w) => all.append(&mut w),
                                    Err(e) => {
                                        let _ = out2.send(DataMsg::Error(format!(
                                            "warning archive {d}: {e:#}"
                                        )));
                                    }
                                }
                            }
                            // The CGI day files are UTC; the UI day is
                            // Central, so trim to the window and dedupe
                            // (events spanning midnight appear in both).
                            all.retain(|w| w.issued >= start && w.issued < end);
                            all.sort_by(|a, b| b.issued.cmp(&a.issued));
                            all.dedup_by(|a, b| a.id == b.id);
                            let _ = out2.send(DataMsg::ArchiveWarnings {
                                date,
                                warnings: all,
                            });
                        })
                        .ok();
                }
                DataCmd::ArchiveWindowShift(delta) => {
                    backfill.clear();
                    archive_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                    if let Some(day) = archive.as_mut() {
                        shift_archive_window(&source, day, delta, &out, &archive_cancel);
                    }
                }
                DataCmd::DeriveSrm {
                    key,
                    volume,
                    toward_deg,
                    speed_kt,
                } => {
                    // Motion-dependent, and cheap enough to run inline on this
                    // thread (~10 ms over a full VEL volume): the UI keeps
                    // drawing while it computes.
                    let t0 = std::time::Instant::now();
                    match radar_core::derived::storm_relative_volume(&volume, toward_deg, speed_kt)
                    {
                        Some(v) => {
                            log::debug!(
                                "SRM {key:x}: {} sweeps toward {toward_deg:.0}° at {speed_kt:.0} kt in {} ms",
                                v.sweeps.len(),
                                t0.elapsed().as_millis()
                            );
                            let _ = out.send(DataMsg::Srm {
                                key,
                                volume: std::sync::Arc::new(v),
                            });
                        }
                        None => log::debug!("SRM {key:x}: no velocity data in this volume"),
                    }
                }
                DataCmd::EnterLive(site) => {
                    archive_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                    // Back to live: drop the archive listing, clear the ring,
                    // and show the newest scan immediately. The live poller
                    // keeps future scans coming. The UI's site travels with
                    // the command — a desync here used to drop every volume
                    // silently.
                    current_site = site;
                    archive = None;
                    watchdog_enabled = true;
                    let _ = out.send(DataMsg::ClearFrames);
                    let _ = out.send(DataMsg::Status("returning to live…".into()));
                    if backgrounded() {
                        live = None;
                        backfill.clear();
                    } else {
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
                }
                DataCmd::RefreshLive(site) => {
                    // Returning from Android background: keep the frames
                    // already in memory and just restart/catch up the live
                    // feed. Do not clear or replay the cached window.
                    current_site = site;
                    archive = None;
                    watchdog_enabled = true;
                    backfill.clear();
                    if backgrounded() {
                        live = None;
                    } else {
                        start_live(&source, &current_site, &mut live, &out);
                        if let Some(ts) =
                            archive_catchup(&source, &current_site, last_delivered, &out)
                        {
                            last_delivered = Some(ts);
                        }
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
                DataCmd::LoadOutlook(day, ty) => {
                    outlook_day = day;
                    outlook_type = ty;
                    // Fetch all four products for the day on a worker: four
                    // sequential SPC downloads must not hold up radar
                    // commands (the layer shows them together).
                    if let Some(src) = outlooks.clone() {
                        let out = out.clone();
                        std::thread::spawn(move || refresh_outlook(&src, day, ty, &out));
                    }
                }
                DataCmd::LoadMrms => {
                    // Its own worker: the newest file is a several-MB download
                    // plus a PNG-packed GRIB2 decode, which must not sit in
                    // front of radar commands.
                    let out = out.clone();
                    let site = current_site.clone();
                    std::thread::spawn(move || {
                        let source = radar_data::mrms::MrmsSource::new(
                            radar_data::DiskCache::default_root(),
                        );
                        // Only the window around the radar: native 1 km
                        // resolution instead of a blocky continental image.
                        let crop = radar_core::sites::site(&site)
                            .map(|s| radar_data::local_overlay_box(s.latitude, s.longitude));
                        // Two windows from the one downloaded file: the
                        // CONUS-wide image (so the layer covers the map when
                        // zoomed out) and the native-resolution window around
                        // the radar on top of it.
                        let product = radar_data::mrms::MrmsProduct::Reflectivity;
                        let wide = source.latest(product, 3, None);
                        let near = source.latest(product, 1, crop);
                        match (wide, near) {
                            (Ok(wide), Ok(near)) => {
                                let _ = out.send(DataMsg::Mrms {
                                    mosaics: vec![
                                        std::sync::Arc::new(wide),
                                        std::sync::Arc::new(near),
                                    ],
                                });
                            }
                            (Err(e), _) | (_, Err(e)) => {
                                log::warn!("MRMS mosaic: {e:#}");
                                let _ = out.send(DataMsg::Error(format!("MRMS mosaic: {e:#}")));
                            }
                        }
                    });
                }
                DataCmd::LoadQpe(product) => {
                    let out = out.clone();
                    let site = current_site.clone();
                    std::thread::spawn(move || {
                        let source = radar_data::mrms::MrmsSource::new(
                            radar_data::DiskCache::default_root(),
                        );
                        let crop = radar_core::sites::site(&site)
                            .map(|s| radar_data::local_overlay_box(s.latitude, s.longitude));
                        let wide = source.latest(product, 3, None);
                        let near = source.latest(product, 1, crop);
                        match (wide, near) {
                            (Ok(wide), Ok(near)) => {
                                let _ = out.send(DataMsg::Qpe {
                                    product,
                                    mosaics: vec![
                                        std::sync::Arc::new(wide),
                                        std::sync::Arc::new(near),
                                    ],
                                });
                            }
                            (Err(e), _) | (_, Err(e)) => {
                                log::warn!("MRMS QPE: {e:#}");
                                let _ = out.send(DataMsg::Error(format!("MRMS QPE: {e:#}")));
                            }
                        }
                    });
                }
                DataCmd::LoadModelField { model, kind, hour } => {
                    let out = out.clone();
                    let site = current_site.clone();
                    std::thread::spawn(move || {
                        let source = match radar_data::HrrrFieldSource::new() {
                            Ok(s) => s,
                            Err(e) => {
                                let _ = out.send(DataMsg::Error(format!("model field: {e:#}")));
                                return;
                            }
                        };
                        let bbox = radar_core::sites::site(&site)
                            .map(|s| radar_data::local_overlay_box(s.latitude, s.longitude))
                            .unwrap_or([35.0, -100.0, 43.0, -92.0]);
                        let result = match model {
                            radar_data::FieldModel::Hrrr => source.fetch_latest(kind, hour, bbox),
                            radar_data::FieldModel::Rap => {
                                source.fetch_latest_rap(kind, hour, bbox)
                            }
                        };
                        match result {
                            Ok(field) => {
                                let _ = out.send(DataMsg::ModelField(std::sync::Arc::new(field)));
                            }
                            Err(e) => {
                                log::warn!("model field: {e:#}");
                                let _ = out.send(DataMsg::Error(format!("model field: {e:#}")));
                            }
                        }
                    });
                }
                DataCmd::SetSynopticToken(token) => {
                    let t = token.trim().to_string();
                    if t.is_empty() {
                        synoptic_token = None;
                    } else {
                        synoptic_token = Some(t);
                    }
                }
                DataCmd::LoadSynoptic {
                    bbox,
                    width,
                    height,
                } => {
                    if let Some(tok) = synoptic_token.clone() {
                        let out = out.clone();
                        std::thread::spawn(move || {
                            let src = radar_data::synoptic::SynopticSource::new(USER_AGENT, &tok);
                            match src {
                                Ok(s) => match s.latest_in_bbox(bbox, width, height) {
                                    Ok(stations) => {
                                        let _ = out.send(DataMsg::SynopticStations(stations));
                                    }
                                    Err(e) => {
                                        let _ = out.send(DataMsg::SynopticError(format!("{e:#}")));
                                    }
                                },
                                Err(e) => {
                                    let _ = out.send(DataMsg::SynopticError(format!("{e:#}")));
                                }
                            }
                        });
                    } else {
                        let _ =
                            out.send(DataMsg::SynopticError("Synoptic API token not set".into()));
                    }
                }
                DataCmd::LoadMetar { bbox } => {
                    log::debug!("background: loading METAR {bbox:?}");
                    let out = out.clone();
                    std::thread::spawn(move || {
                        match radar_data::MetarSource::new(USER_AGENT)
                            .and_then(|src| src.latest_in_bbox(bbox))
                        {
                            Ok(stations) => {
                                let _ = out.send(DataMsg::MetarStations(stations));
                            }
                            Err(e) => {
                                log::warn!("METAR: {e:#}");
                                let _ = out.send(DataMsg::MetarError(format!("{e:#}")));
                            }
                        }
                    });
                }
                DataCmd::LoadMetarHistory(stid) => {
                    let out = out.clone();
                    std::thread::spawn(move || {
                        match radar_data::MetarSource::new(USER_AGENT)
                            .and_then(|src| src.history(&stid, 6))
                        {
                            Ok(history) => {
                                let _ =
                                    out.send(DataMsg::MetarHistory(std::sync::Arc::new(history)));
                            }
                            Err(e) => {
                                let _ = out.send(DataMsg::MetarHistoryError(format!(
                                    "METAR history: {e:#}"
                                )));
                            }
                        }
                    });
                }
                DataCmd::SetAmbientKeys { api_key, app_key } => {
                    let (a, b) = (api_key.trim().to_string(), app_key.trim().to_string());
                    ambient_keys = if a.is_empty() || b.is_empty() {
                        None
                    } else {
                        Some((a, b))
                    };
                }
                DataCmd::LoadAmbient => {
                    if let Some((ak, apk)) = ambient_keys.clone() {
                        let out = out.clone();
                        std::thread::spawn(move || {
                            let src =
                                radar_data::ambient::AmbientSource::new(USER_AGENT, &ak, &apk);
                            match src {
                                Ok(s) => match s.stations() {
                                    Ok(stations) => {
                                        let _ = out.send(DataMsg::AmbientStations(stations));
                                    }
                                    Err(e) => {
                                        let _ = out.send(DataMsg::AmbientError(format!("{e:#}")));
                                    }
                                },
                                Err(e) => {
                                    let _ = out.send(DataMsg::AmbientError(format!("{e:#}")));
                                }
                            }
                        });
                    } else {
                        let _ = out.send(DataMsg::AmbientError(
                            "Ambient Weather API key / application key not set".into(),
                        ));
                    }
                }
                DataCmd::LoadYouTubeFeed => {
                    let out = out.clone();
                    std::thread::spawn(move || {
                        let result = radar_data::YouTubeSource::new(USER_AGENT)
                            .and_then(|source| source.latest())
                            .map_err(|error| format!("{error:#}"));
                        let _ = out.send(DataMsg::YouTubeFeed(result));
                    });
                }
                DataCmd::LoadGoes {
                    layer,
                    west_satellite,
                } => {
                    // One WMS image per refresh (~1.5 MB); its own worker.
                    let out = out.clone();
                    let site = current_site.clone();
                    std::thread::spawn(move || {
                        let near_box = radar_core::sites::site(&site)
                            .map(|s| radar_data::local_overlay_box(s.latitude, s.longitude))
                            .unwrap_or([34.0, -104.2, 44.0, -88.2]);
                        // CONUS-wide for coverage, then the sharp local
                        // window drawn on top.
                        let fetch = |bbox: [f64; 4]| {
                            radar_data::goes::GoesSource::new()
                                .and_then(|src| src.latest(layer, west_satellite, bbox))
                        };
                        let wide = fetch([20.0, -130.0, 55.0, -60.0]);
                        let near = fetch(near_box);
                        match (wide, near) {
                            (Ok(wide), Ok(near)) => {
                                let _ = out.send(DataMsg::Goes {
                                    images: vec![
                                        std::sync::Arc::new(wide),
                                        std::sync::Arc::new(near),
                                    ],
                                });
                            }
                            (Err(e), _) | (_, Err(e)) => {
                                log::warn!("GOES overlay: {e:#}");
                                let _ = out.send(DataMsg::Error(format!("GOES overlay: {e:#}")));
                            }
                        }
                    });
                }
                DataCmd::LoadFuturecast { bbox, extended } => {
                    // HRRR frame fetch + GRIB decode is network + CPU work;
                    // never let it sit in front of radar commands. Frames are
                    // sent as they decode, so the UI can show f01 immediately
                    // while the rest of the run streams in.
                    let out = out.clone();
                    std::thread::Builder::new()
                        .name("futurecast".into())
                        .spawn(move || {
                            let source = match radar_data::futurecast::FuturecastSource::new() {
                                Ok(s) => s,
                                Err(e) => {
                                    let _ = out.send(DataMsg::FuturecastDone {
                                        received: 0,
                                        total: 0,
                                        error: Some(format!("{e:#}")),
                                        extended,
                                    });
                                    return;
                                }
                            };
                            let plan = match source.plan(Utc::now(), bbox, extended) {
                                Ok(p) => p,
                                Err(e) => {
                                    let _ = out.send(DataMsg::FuturecastDone {
                                        received: 0,
                                        total: 0,
                                        error: Some(format!("{e:#}")),
                                        extended,
                                    });
                                    return;
                                }
                            };
                            let total = plan.frames.len();
                            let _ = out.send(DataMsg::FuturecastRun {
                                init: plan.init,
                                bbox,
                                frames: plan.frames.clone(),
                                extended,
                            });
                            // Fetching a whole run sequentially took ~15 s
                            // for 18 frames (and much longer for 48). A small
                            // worker pool keeps NOMADS happy (four at a
                            // time) while cutting the wait to roughly one
                            // round-trip per wave. Start at the first frame
                            // that is still in the future, then wrap to the
                            // already-valid head, so the map gets useful data
                            // on the first wave instead of waiting for f01.
                            let now = Utc::now();
                            let first_future = plan
                                .frames
                                .iter()
                                .position(|info| info.valid >= now)
                                .unwrap_or(0);
                            let order: Vec<usize> = (first_future..plan.frames.len())
                                .chain(0..first_future)
                                .collect();
                            let next = std::sync::atomic::AtomicUsize::new(0);
                            let received = std::sync::atomic::AtomicUsize::new(0);
                            let first_error: std::sync::Mutex<Option<String>> =
                                std::sync::Mutex::new(None);
                            let workers = total.min(4).max(1);
                            std::thread::scope(|scope| {
                                for _ in 0..workers {
                                    let out = out.clone();
                                    let source = &source;
                                    let plan = &plan;
                                    let order = &order;
                                    let next = &next;
                                    let received = &received;
                                    let first_error = &first_error;
                                    scope.spawn(move || {
                                        loop {
                                            let index = next
                                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                            let Some(&frame_index) = order.get(index) else {
                                                break;
                                            };
                                            let info = &plan.frames[frame_index];
                                            let _ = out.send(DataMsg::Status(format!(
                                                "FutureCast · loading f{:02}",
                                                info.hour
                                            )));
                                            match source.fetch(plan, info, bbox) {
                                                Ok(frame) => {
                                                    log::debug!(
                                                        "FutureCast f{:02}: {}x{} max {:?} dBZ",
                                                        frame.hour,
                                                        frame.width,
                                                        frame.height,
                                                        frame.max_dbz()
                                                    );
                                                    received.fetch_add(
                                                        1,
                                                        std::sync::atomic::Ordering::Relaxed,
                                                    );
                                                    if out
                                                        .send(DataMsg::FuturecastFrame(
                                                            std::sync::Arc::new(frame),
                                                        ))
                                                        .is_err()
                                                    {
                                                        break;
                                                    }
                                                }
                                                Err(e) => {
                                                    log::warn!(
                                                        "FutureCast f{:02} failed: {e:#}",
                                                        info.hour
                                                    );
                                                    let mut slot = first_error
                                                        .lock()
                                                        .unwrap_or_else(|e| e.into_inner());
                                                    slot.get_or_insert_with(|| format!("{e:#}"));
                                                }
                                            }
                                        }
                                    });
                                }
                            });
                            source.sweep_cache();
                            let received = received.load(std::sync::atomic::Ordering::Relaxed);
                            let error = first_error.into_inner().unwrap_or_else(|e| e.into_inner());
                            let _ = out.send(DataMsg::FuturecastDone {
                                received,
                                total,
                                error,
                                extended,
                            });
                        })
                        .ok();
                }
                DataCmd::LoadTotals(lat, lon) => {
                    // Its own worker: up to eight observation series is
                    // seconds of network work and must not sit in front of
                    // radar and alert commands.
                    let out = out.clone();
                    // "Viewing area" = the selected station's area, the same
                    // radius the alerts layer uses for near-station data.
                    let radius = crate::state::STATION_ALERT_RADIUS_KM as f64;
                    std::thread::spawn(move || {
                        let src = match radar_data::ForecastSource::new(USER_AGENT) {
                            Ok(s) => s,
                            Err(e) => {
                                let _ = out.send(DataMsg::Error(format!("totals: {e:#}")));
                                return;
                            }
                        };
                        match src.station_totals(lat, lon, 8, radius) {
                            Ok(v) => {
                                log::debug!("{} stations report totals", v.len());
                                let _ = out.send(DataMsg::Totals(v));
                            }
                            Err(e) => {
                                log::warn!("totals fetch failed: {e:#}");
                                let _ = out.send(DataMsg::Error(format!("totals: {e:#}")));
                            }
                        }
                    });
                }
                DataCmd::LoadForecast(lat, lon) => {
                    let _ = out.send(DataMsg::Status(format!(
                        "loading forecast for {lat:.3}, {lon:.3}…"
                    )));
                    refresh_forecast(&forecasts, lat, lon, &out);
                }
                DataCmd::LoadSounding(lat, lon) => {
                    let out = out.clone();
                    std::thread::spawn(move || {
                        let src = match radar_data::SoundingSource::new(USER_AGENT) {
                            Ok(s) => s,
                            Err(e) => {
                                let _ =
                                    out.send(DataMsg::SoundingError(format!("sounding: {e:#}")));
                                return;
                            }
                        };
                        match src.model_sounding(lat, lon) {
                            Ok(s) => {
                                let _ = out.send(DataMsg::Sounding(s));
                            }
                            Err(e) => {
                                let _ =
                                    out.send(DataMsg::SoundingError(format!("sounding: {e:#}")));
                            }
                        }
                    });
                }
                DataCmd::LoadObservedSounding(lat, lon) => {
                    let out = out.clone();
                    std::thread::spawn(move || {
                        match radar_data::RaobSource::new(USER_AGENT)
                            .and_then(|src| src.latest_nearest(lat, lon))
                        {
                            Ok(s) => {
                                let _ = out.send(DataMsg::ObservedSounding(s));
                            }
                            Err(e) => {
                                let _ = out
                                    .send(DataMsg::ObservedSoundingError(format!("RAOB: {e:#}")));
                            }
                        }
                    });
                }
                DataCmd::LoadRapSounding(lat, lon) => {
                    let out = out.clone();
                    std::thread::spawn(move || {
                        match radar_data::RapSource::new().and_then(|src| src.latest(lat, lon)) {
                            Ok(s) => {
                                let _ = out.send(DataMsg::RapSounding(s));
                            }
                            Err(e) => {
                                let _ = out.send(DataMsg::RapSoundingError(format!("RAP: {e:#}")));
                            }
                        }
                    });
                }
                DataCmd::LoadLevel3 { bbox } => {
                    log::debug!("background: loading N0Q Level III");
                    let out = out.clone();
                    std::thread::spawn(move || {
                        log::debug!("N0Q thread started");
                        match radar_data::Level3Source::new().and_then(|src| {
                            log::debug!("N0Q client ready");
                            src.latest_n0q(bbox)
                        }) {
                            Ok(image) => {
                                log::debug!("N0Q fetched {}x{}", image.width, image.height);
                                let _ = out.send(DataMsg::Level3(std::sync::Arc::new(image)));
                            }
                            Err(e) => {
                                log::warn!("N0Q fetch failed: {e:#}");
                                let _ = out.send(DataMsg::Level3Error(format!("Level III: {e:#}")));
                            }
                        }
                    });
                }
                DataCmd::LoadVerification { lat, lon, hours } => {
                    let out = out.clone();
                    std::thread::spawn(move || {
                        let product = match hours {
                            3 => radar_data::mrms::MrmsProduct::MultiSensorQpe03h,
                            6 => radar_data::mrms::MrmsProduct::MultiSensorQpe06h,
                            24 => radar_data::mrms::MrmsProduct::MultiSensorQpe24h,
                            _ => radar_data::mrms::MrmsProduct::MultiSensorQpe01h,
                        };
                        let result = (|| -> anyhow::Result<radar_data::QpeVerification> {
                            let source = radar_data::mrms::MrmsSource::new(
                                radar_data::cache::DiskCache::default_root(),
                            );
                            let qpe = source.latest(
                                product,
                                1,
                                Some(radar_data::local_overlay_box(lat, lon)),
                            )?;
                            let forecasts = radar_data::ForecastSource::new(USER_AGENT)?;
                            let totals = forecasts.station_totals(lat, lon, 20, 150.0)?;
                            Ok(radar_data::verify::compare(&qpe, &totals, hours))
                        })();
                        match result {
                            Ok(report) => {
                                let _ =
                                    out.send(DataMsg::Verification(std::sync::Arc::new(report)));
                            }
                            Err(e) => {
                                log::warn!("QPE verification: {e:#}");
                                let _ = out.send(DataMsg::VerificationError(format!("{e:#}")));
                            }
                        }
                    });
                }
                DataCmd::LoadStormHistory {
                    volumes,
                    key,
                    tilt_deg,
                    lat,
                    lon,
                } => {
                    let out = out.clone();
                    std::thread::spawn(move || {
                        let refs: Vec<&radar_core::Volume> =
                            volumes.iter().map(|v| v.as_ref()).collect();
                        let histories = radar_render::storm_histories(&refs, tilt_deg, lat, lon);
                        let _ = out.send(DataMsg::StormHistory { key, histories });
                    });
                }
                DataCmd::LoadEnsemble { field, lat, lon } => {
                    let out = out.clone();
                    std::thread::spawn(move || {
                        match radar_data::EnsembleSource::new()
                            .and_then(|src| src.fetch(field, lat, lon))
                        {
                            Ok(series) => {
                                let _ = out.send(DataMsg::Ensemble(std::sync::Arc::new(series)));
                            }
                            Err(e) => {
                                log::warn!("ensemble: {e:#}");
                                let _ = out.send(DataMsg::EnsembleError(format!("{e:#}")));
                            }
                        }
                    });
                }
                DataCmd::LoadMidLevel => {
                    let out = out.clone();
                    std::thread::spawn(move || {
                        match radar_data::MidLevelSource::new().and_then(|src| src.latest_run()) {
                            Ok(run) => {
                                let _ = out.send(DataMsg::MidLevel(std::sync::Arc::new(run)));
                            }
                            Err(e) => {
                                log::warn!("mid-level wind: {e:#}");
                                let _ = out.send(DataMsg::MidLevelError(format!("{e:#}")));
                            }
                        }
                    });
                }
                DataCmd::LoadTerrain(lat, lon) => {
                    let out = out.clone();
                    std::thread::spawn(move || {
                        let src = match radar_data::TerrainSource::new(USER_AGENT) {
                            Ok(s) => s,
                            Err(e) => {
                                let _ = out.send(DataMsg::TerrainError(format!("terrain: {e:#}")));
                                return;
                            }
                        };
                        match src.for_site(lat, lon) {
                            Ok(grid) => {
                                let _ = out.send(DataMsg::Terrain(std::sync::Arc::new(grid)));
                            }
                            Err(e) => {
                                let _ = out.send(DataMsg::TerrainError(format!("terrain: {e:#}")));
                            }
                        }
                    });
                }
                DataCmd::LoadHrrr(lat, lon) => {
                    let out = out.clone();
                    std::thread::spawn(move || {
                        let src = match radar_data::SoundingSource::new(USER_AGENT) {
                            Ok(s) => s,
                            Err(e) => {
                                let _ = out.send(DataMsg::HrrrError(format!("HRRR: {e:#}")));
                                return;
                            }
                        };
                        match src.hrrr_series(lat, lon) {
                            Ok(series) => {
                                let _ = out.send(DataMsg::Hrrr(series));
                            }
                            Err(e) => {
                                let _ = out.send(DataMsg::HrrrError(format!("HRRR: {e:#}")));
                            }
                        }
                    });
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
                DataCmd::LoadGlm { west } => {
                    let out = out.clone();
                    std::thread::spawn(move || {
                        let result = radar_data::glm::GlmSource::new()
                            .and_then(|source| source.latest(west));
                        match result {
                            Ok(frame) => {
                                let _ = out.send(DataMsg::Lightning {
                                    strikes: frame.strikes,
                                    satellite: frame.satellite,
                                    key: frame.key,
                                });
                            }
                            Err(error) => {
                                log::warn!("GLM: {error:#}");
                                let _ = out.send(DataMsg::LightningError(format!("{error:#}")));
                            }
                        }
                    });
                }
                DataCmd::AiBrief {
                    kind,
                    config,
                    system,
                    history,
                    user,
                    session,
                } => {
                    // The model may take a while; never hold up the data queue.
                    let out = out.clone();
                    std::thread::spawn(move || {
                        let turns: Vec<radar_data::ai::ChatTurn> = history
                            .into_iter()
                            .map(|(role, content)| radar_data::ai::ChatTurn { role, content })
                            .collect();
                        let result = radar_data::ai::complete_chat(
                            &config, &system, &turns, &user, &session,
                        )
                        .map_err(|e| format!("{e:#}"));
                        let _ = out.send(DataMsg::AiBrief { kind, result });
                    });
                }
                DataCmd::AiTest { config } => {
                    let out = out.clone();
                    std::thread::spawn(move || {
                        // Unique per test: reusing one fixed session id made
                        // repeated Test connection taps collide as 409s.
                        let session = format!(
                            "stormscope-ai-test-{}",
                            chrono::Utc::now().timestamp_millis()
                        );
                        let result = radar_data::ai::complete(
                            &config,
                            "You are a connectivity test.",
                            "Reply with exactly: connected",
                            &session,
                        )
                        .map_err(|e| format!("{e:#}"));
                        let _ = out.send(DataMsg::AiTest(result));
                    });
                }
                DataCmd::Shutdown => break,
                DataCmd::CheckUpdate {
                    current,
                    asset_suffix,
                } => {
                    log::info!("update check: current {current} ({asset_suffix})");
                    let out = out.clone();
                    std::thread::spawn(move || {
                        let result = radar_data::update::check_latest(&current, &asset_suffix)
                            .map_err(|e| format!("{e:#}"));
                        log::info!("update check result: {result:?}");
                        let _ = out.send(DataMsg::UpdateCheck(result));
                    });
                }
                DataCmd::DownloadUpdate {
                    url,
                    version,
                    asset_suffix,
                } => {
                    let out = out.clone();
                    std::thread::spawn(move || {
                        let result =
                            radar_data::update::download_asset(&url, &version, &asset_suffix)
                                .map_err(|e| format!("{e:#}"));
                        let _ = out.send(DataMsg::UpdateDownload(result));
                    });
                }
            },
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}

            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }

        // Android: Storm Watch keeps the radar feed alive in the background
        // only while a watch or warning covers the GPS fix or a map marker.
        // When the last one expires, stop the poller and stop downloading;
        // the foreground app is unaffected.
        #[cfg(target_os = "android")]
        if !crate::android_lifecycle::is_foreground() {
            let (markers, saved_gps, watch_enabled, live_mode) = shared
                .lock()
                .map(|shared| {
                    (
                        shared.watch_markers.clone(),
                        shared.gps_fix,
                        shared.storm_watch_enabled,
                        shared.live_mode,
                    )
                })
                .unwrap_or_default();
            // Read Java's latest fix while backgrounded so movement in or out
            // of a watch/warning is handled without a UI frame. Fall back to
            // the last point mirrored by the UI if JNI is unavailable.
            let gps = crate::android_location::read_fix().or(saved_gps);
            let mut points = Vec::with_capacity(markers.len() + usize::from(gps.is_some()));
            if let Some(fix) = gps {
                points.push(fix);
            }
            points.extend(markers);
            let mut seen = std::collections::HashSet::new();
            points.retain(|point| seen.insert((point.0.to_bits(), point.1.to_bits())));
            points.truncate(12);
            let point_count = points.len();
            let points_changed = points != background_points;
            let exact_due = background_exact_checked_at.elapsed() >= Duration::from_secs(60);
            let near_due = background_near_checked_at.elapsed() >= Duration::from_secs(300);
            if !watch_enabled || !live_mode || points.is_empty() {
                background_exact_covered = false;
                background_near_covered = false;
                background_covered = false;
            } else {
                if points_changed || exact_due {
                    if let Some(alerts) = alerts.as_ref()
                        && let Some(covered) = exact_watch_warning_at(alerts, &points)
                    {
                        background_exact_covered = covered;
                    }
                    background_exact_checked_at = std::time::Instant::now();
                }
                // The nationwide listing is ~1.7 MB, so the 50-mile sweep
                // runs every five minutes. Exact point checks still run every
                // minute and catch a warning that begins covering the point.
                if !background_exact_covered && (points_changed || near_due) {
                    if let Some(alerts) = alerts.as_ref()
                        && let Some(covered) = nearby_watch_warning(alerts, &points)
                    {
                        background_near_covered = covered;
                    }
                    background_near_checked_at = std::time::Instant::now();
                }
                background_covered = background_exact_covered || background_near_covered;
                background_points = points;
            }
            if background_covered {
                if !background_active {
                    background_active = true;
                    log::info!(
                        "background radar active: watch/warning covers {} point(s)",
                        point_count
                    );
                }
                if live.is_none() {
                    start_live(&source, &current_site, &mut live, &out);
                    // Surface the newest scan immediately; the poller then
                    // follows subsequent chunks as they arrive.
                    if let Some(ts) = archive_catchup(&source, &current_site, last_delivered, &out)
                    {
                        last_delivered = Some(ts);
                    }
                }
                if let Some(ts) = drain_live(&mut live, &out) {
                    last_delivered = Some(ts);
                }
                continue;
            }
            if background_active || live.is_some() {
                background_active = false;
                log::info!("background radar paused: no watch/warning covers the GPS or markers");
                if live.is_some() {
                    live = None;
                }
                let _ = out.send(DataMsg::Status(
                    "background radar paused (no watch or warning)".into(),
                ));
            }
            continue;
        }

        // Warning-archive backfill (opt-in, newest-first, rate-limited):
        // at most one small day-file per interval; days already cached are
        // skipped forever, and the daily budget bounds the total.
        if let Some(res) = crawl.step() {
            match res {
                Ok(day) => log::debug!("warning archive: cached {day}"),
                Err(e) => log::warn!("warning archive fetch: {e:#}"),
            }
            let _ = out.send(DataMsg::ArchiveCrawl(crawl.status()));
        }

        // Replay the queued refill one slice at a time. Decoding costs ~1 s
        // per volume, so draining it inside the command handler would stall
        // the next command — and the next site switch — for tens of seconds.
        if !backfill.is_empty() {
            if fill_total == 0 {
                fill_total = backfill.len() as u32;
                fill_done = 0;
                fill_label = format!("{current_site} live radar");
                let _ = out.send(DataMsg::LoadProgress {
                    label: fill_label.clone(),
                    done: 0,
                    total: fill_total,
                    cached: true,
                });
            }
            const BACKFILL_SLICE: Duration = Duration::from_millis(150);
            let deadline = std::time::Instant::now() + BACKFILL_SLICE;
            while let Some(r) = backfill.pop_front() {
                let cached = is_cached(&source, &r);
                let _ = emit_volume(&source, &r, &out);
                fill_done += 1;
                let _ = out.send(DataMsg::LoadProgress {
                    label: fill_label.clone(),
                    done: fill_done,
                    total: fill_total,
                    cached,
                });
                if std::time::Instant::now() >= deadline {
                    break;
                }
            }
            if backfill.is_empty() {
                let _ = out.send(DataMsg::LoadProgress {
                    label: fill_label.clone(),
                    done: fill_total,
                    total: fill_total,
                    cached: true,
                });
                fill_total = 0;
                fill_done = 0;
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
        if last_reports.elapsed() >= REPORTS_REFRESH {
            last_reports = std::time::Instant::now();
            refresh_reports(&reports, &out);
        }
        if last_mcds.elapsed() >= MCD_REFRESH {
            last_mcds = std::time::Instant::now();
            refresh_mcds(&mcds, &out);
        }
        if last_alerts_fast.elapsed() >= ALERTS_FAST_REFRESH {
            last_alerts_fast = std::time::Instant::now();
            refresh_alerts_at_station(&alerts, &mut zones, &current_site, &out);
        }
        if last_alerts.elapsed() >= ALERTS_REFRESH {
            last_alerts = std::time::Instant::now();
            refresh_alerts(&alerts, &current_site, &out);
        }
        if last_outlook.elapsed() >= OUTLOOK_REFRESH {
            last_outlook = std::time::Instant::now();
            if let Some(src) = outlooks.clone() {
                let out = out.clone();
                let day = outlook_day;
                let ty = outlook_type;
                std::thread::spawn(move || refresh_outlook(&src, day, ty, &out));
            }
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
#[allow(clippy::too_many_arguments)]
fn open_archive_day(
    source: &S3Source,
    site: &str,
    date: NaiveDate,
    target_minute: Option<u32>,
    current_site: &mut String,
    out: &Sender<DataMsg>,
    cancel: &std::sync::atomic::AtomicBool,
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
    // vols is newest-first; default window is the head (newest of the day),
    // or the one covering the requested time of day.
    let window_start = target_minute
        .map(|m| window_start_for_minute(&vols, m, window_len))
        .unwrap_or(0);
    let mut day = ArchiveDay { vols, window_start };
    open_archive_window(
        source,
        site,
        date,
        &mut day,
        window_start,
        window_len,
        out,
        cancel,
    );
    Some(day)
}

/// Window start (newest-first index) whose span covers the given Central
/// time of day: the closest scan to `minute`, then centered in the window
/// (clamped to the day's bounds). Circular distance, so 23:50 finds the
/// 00:00 scan of the same listing day.
fn window_start_for_minute(
    vols: &[radar_data::ArchiveVolumeRef],
    minute: u32,
    window_len: usize,
) -> usize {
    let total = vols.len();
    if total == 0 {
        return 0;
    }
    use chrono::Timelike;
    let tz = crate::timefmt::central();
    let target = i64::from(minute % 1440);
    let idx = vols
        .iter()
        .enumerate()
        .min_by_key(|(_, v)| {
            let t = v.volume_start.with_timezone(tz).time();
            let m = i64::from(t.hour() * 60 + t.minute());
            let d = (m - target).abs() % 1440;
            d.min(1440 - d)
        })
        .map(|(i, _)| i)
        .unwrap_or(0);
    let max_start = total.saturating_sub(window_len);
    idx.saturating_sub(window_len / 2).min(max_start)
}

/// Clear the ring, stream one window and report its position — the single
/// path both the day-load and the paging buttons use.
#[allow(clippy::too_many_arguments)]
fn open_archive_window(
    source: &S3Source,
    site: &str,
    date: NaiveDate,
    day: &mut ArchiveDay,
    new_start: usize,
    window_len: usize,
    out: &Sender<DataMsg>,
    cancel: &std::sync::atomic::AtomicBool,
) {
    day.window_start = new_start;
    // The window *replaces* whatever was on the ring — without this the live
    // frame still sitting there made every older archive scan look
    // out-of-order and get dropped.
    let _ = out.send(DataMsg::ClearFrames);
    stream_window(source, site, date, day, window_len, out, cancel);
    let _ = out.send(DataMsg::ArchiveInfo {
        total: day.vols.len(),
        start: new_start,
        len: window_len,
    });
}

/// Page the archive window earlier (delta<0) or later (delta>0) by one
/// window, clear the ring, and stream the new window.
fn shift_archive_window(
    source: &S3Source,
    day: &mut ArchiveDay,
    delta: i32,
    out: &Sender<DataMsg>,
    cancel: &std::sync::atomic::AtomicBool,
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
    let site = day.vols[0].site.clone();
    let date = day.vols[0].date;
    open_archive_window(source, &site, date, day, new_start, window_len, out, cancel);
}

/// Fetch+emit one window's volumes oldest→newest into the UI ring.
fn stream_window(
    source: &S3Source,
    site: &str,
    date: NaiveDate,
    day: &ArchiveDay,
    window_len: usize,
    out: &Sender<DataMsg>,
    cancel: &std::sync::atomic::AtomicBool,
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
    let label = format!("{site} archive · {}", date.format("%b %-d"));
    let total = slice.len() as u32;
    for (i, r) in slice.iter().rev().enumerate() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = out.send(DataMsg::Status(
                "archive load cancelled — back to live".into(),
            ));
            return;
        }
        let cached = is_cached(source, r);
        let _ = out.send(DataMsg::LoadProgress {
            label: label.clone(),
            done: i as u32,
            total,
            cached,
        });
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
    let _ = out.send(DataMsg::LoadProgress {
        label,
        done: total,
        total,
        cached: true,
    });
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
    // Two hours of NEXRAD scans at the usual ~5-minute cadence. The live
    // ring may retain fewer on a phone, but the background prefetch still
    // covers the full two-hour window instead of only the cached subset.
    const PRIME_SCANS: usize = 24;
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

    let newest = vols[0].volume_start;

    // Drop whatever is on the ring before pushing, or a frame from the
    // previous station lingers under the new one.
    let _ = out.send(DataMsg::ClearFrames);

    // Show the newest volume first, whether it comes from S3 or the disk
    // cache. The rest of the two-hour window is only queued after this call:
    // the map gets a current frame before any backfill download competes for
    // bandwidth, and motion/tracks fill in as the previous scans arrive.
    let delivered = emit_volume(source, &vols[0], out);
    if delivered.is_some() {
        log::info!(
            "live refresh: scan {} (newest first)",
            crate::timefmt::fmt_central(&newest)
        );
    }

    // Do not replay the cached two-hour window on startup or resume: that
    // was a multi-second NAND read of every scan on every reopen and a quick
    // app switch. Only the scan immediately before the newest is queued,
    // because motion/tracks need a second frame; the explicit Last 2hr action
    // owns history.
    let mut queue: VecDeque<radar_data::ArchiveVolumeRef> = VecDeque::new();
    if let Some(previous) = vols.get(1) {
        queue.push_back(previous.clone());
        if !is_cached(source, previous) {
            log::info!(
                "prime {site}: fetching the previous scan ({}) for motion",
                crate::timefmt::fmt_central(&previous.volume_start)
            );
        }
    }
    (delivered, queue)
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

/// Load a recent window of scans into the ring, oldest→newest, for immediate
/// looping. The caller chooses the window; scans are fetched explicitly by
/// this action (the startup path never auto-downloads the whole window).
fn load_recent_window(
    source: &S3Source,
    site: &str,
    scans: usize,
    label: &str,
    clear: bool,
    range: Option<(chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)>,
    out: &Sender<DataMsg>,
) {
    let today = chrono::Utc::now()
        .with_timezone(crate::timefmt::central())
        .date_naive();
    let mut vols = list_central_day(source, site, today).unwrap_or_default();
    // Early morning Central: today has few scans, so top up from yesterday's
    // newest so the requested window always fills.
    if vols.len() < scans {
        if let Some(yday) = today.pred_opt() {
            let y = list_central_day(source, site, yday).unwrap_or_default();
            for r in y {
                if vols.len() >= scans {
                    break;
                }
                vols.push(r);
            }
        }
    }
    if let Some((after, before)) = range {
        vols.retain(|r| r.volume_start >= after && r.volume_start <= before);
    } else {
        vols.truncate(scans);
    }
    if vols.is_empty() {
        let _ = out.send(DataMsg::Status(format!("no recent scans for {site}")));
        return;
    }
    if clear {
        let _ = out.send(DataMsg::ClearFrames);
    }
    let _ = out.send(DataMsg::Status(format!(
        "loading {label} ({})…",
        vols.len()
    )));
    // Newest-first: the latest scan is visible immediately while older scans
    // fill in behind it. The ring inserts by time, so playback order is still
    // chronological.
    for r in &vols {
        match source.fetch_archive(r) {
            Ok(volume) => {
                let _ = out.send(DataMsg::Volume(volume));
            }
            Err(e) => {
                log::warn!("{label} fetch {} failed: {e:#}", r.file_name());
                let _ = out.send(DataMsg::Error(format!("{}: {e:#}", r.file_name())));
            }
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    log::info!("{label} loop loaded for {site}");
}

/// Load the newest ~1 hour of scans (~13 at NEXRAD's ~4.5-min cadence).
fn load_last_hour(source: &S3Source, site: &str, out: &Sender<DataMsg>) {
    load_recent_window(source, site, 13, "last hour", true, None, out);
}

/// Load the newest ~2 hours of scans (~24 at NEXRAD's ~4.5-min cadence).
///
/// Does not clear the ring: the live poller's newest frame is often ahead of
/// the archive listing, and clearing it made the map jump backwards to the
/// newest archived scan while the real live frame disappeared.
fn load_last_two_hours(
    source: &S3Source,
    site: &str,
    newest: Option<chrono::DateTime<chrono::Utc>>,
    out: &Sender<DataMsg>,
) {
    let before = newest.unwrap_or_else(chrono::Utc::now);
    let after = before - chrono::Duration::hours(2);
    load_recent_window(
        source,
        site,
        24,
        "last 2 hours",
        false,
        Some((after, before)),
        out,
    );
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

/// Exact-point alerts: NWS `?point=` already includes zone-based products
/// that have no polygon. Returns None when every point request failed.
#[cfg(target_os = "android")]
fn exact_watch_warning_at(alerts: &AlertSource, points: &[(f64, f64)]) -> Option<bool> {
    let mut checked_any = false;
    let mut covered = false;
    for (lat, lon) in points {
        match alerts.active_at_point(*lat, *lon) {
            Ok(warnings) => {
                checked_any = true;
                if warnings.iter().any(is_watch_or_warning) {
                    covered = true;
                    break;
                }
            }
            Err(e) => log::debug!("background point check at {lat:.3},{lon:.3}: {e:#}"),
        }
    }
    checked_any.then_some(covered)
}

/// A watch/warning whose polygon is within 50 miles of any GPS/marker point.
/// Uses one nationwide listing so multiple points do not multiply downloads.
#[cfg(target_os = "android")]
fn nearby_watch_warning(alerts: &AlertSource, points: &[(f64, f64)]) -> Option<bool> {
    let all = alerts.active_all().ok()?;
    Some(
        all.iter()
            .any(|w| is_watch_or_warning(w) && crate::voice::near_points(w, points)),
    )
}

/// True for every watch, warning or emergency product, plus the two winter
/// advisories Storm Watch treats as actionable (Winter Weather and Ice
/// Storm). Other advisories and statements do not keep the radar feed alive.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn is_watch_or_warning(w: &radar_core::warnings::Warning) -> bool {
    let event = &w.event;
    let watch_warning_emergency =
        event.contains("Watch") || event.contains("Warning") || event.contains("Emergency");
    let winter_advisory =
        event.contains("Winter Weather Advisory") || event.contains("Ice Storm Advisory");
    (watch_warning_emergency || winter_advisory) && !event.contains("Statement")
}

/// Re-fetch active NWS alerts around the current site and emit them. Zones
/// are looked up from the site catalog instead of a hardcoded point (the old
/// default evaluated alerts around Norman, OK no matter where the user was).
/// The minute-cadence check for alerts that apply to the station itself.
/// Errors are logged at debug: a failed fast poll costs nothing and the slow
/// sweep will still run.
fn refresh_alerts_at_station(
    alerts: &Option<AlertSource>,
    zones: &mut Option<ZoneSource>,
    site: &str,
    out: &Sender<DataMsg>,
) {
    let Some(a) = alerts else { return };
    let Some(site) = radar_core::sites::site(site) else {
        return;
    };
    match a.active_at_point(site.latitude, site.longitude) {
        Ok(mut w) => {
            if let Some(zones) = zones.as_mut() {
                zones.enrich(&mut w);
            }
            log::debug!("{} alerts apply to {}", w.len(), site.id);
            // Desktop speech must not depend on the UI frame loop (a hidden
            // Wayland workspace may not receive frame callbacks). Android's
            // background service handles the closed-app case instead.
            #[cfg(not(target_os = "android"))]
            crate::voice::notify_near(&w);
            let _ = out.send(DataMsg::AlertsFast(w));
        }
        Err(e) => log::debug!("station alert check failed: {e:#}"),
    }
}

/// Five-minute storm-report pull: the feed is one list request, the texts
/// come from the source's cache (only new products are fetched, paced). The
/// work happens on a worker thread so a cold fill never stalls the
/// coordinator's 250 ms command loop.
fn refresh_reports(
    src: &Option<std::sync::Arc<std::sync::Mutex<radar_data::LsrSource>>>,
    out: &Sender<DataMsg>,
) {
    let Some(src) = src else { return };
    let src = std::sync::Arc::clone(src);
    let out = out.clone();
    std::thread::spawn(move || {
        let mut s = match src.lock() {
            Ok(g) => g,
            Err(_) => return, // a panicking worker poisoned it; drop the poll
        };
        match s.recent(chrono::Duration::hours(12)) {
            Ok(v) => {
                let spotter = v.iter().filter(|r| r.is_spotter()).count();
                log::debug!("{} storm reports ({} from spotters)", v.len(), spotter);
                let _ = out.send(DataMsg::Reports(v));
            }
            Err(e) => log::debug!("storm reports: {e:#}"),
        }
    });
}

/// Five-minute mesoscale-discussion pull (feed + any new discussion page),
/// threaded for the same reason as the reports fill.
fn refresh_mcds(
    src: &Option<std::sync::Arc<std::sync::Mutex<radar_data::McdSource>>>,
    out: &Sender<DataMsg>,
) {
    let Some(src) = src else { return };
    let src = std::sync::Arc::clone(src);
    let out = out.clone();
    std::thread::spawn(move || {
        let mut s = match src.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        match s.active() {
            Ok(v) => {
                log::debug!("{} active mesoscale discussions", v.len());
                let _ = out.send(DataMsg::Mcds(v));
            }
            Err(e) => log::debug!("mesoscale discussions: {e:#}"),
        }
    });
}

fn refresh_alerts(alerts: &Option<AlertSource>, site: &str, out: &Sender<DataMsg>) {
    let Some(a) = alerts else { return };
    let (lat, lon) = radar_core::sites::site(site)
        .map(|s| (s.latitude, s.longitude))
        .unwrap_or((35.3331, -97.2778));
    match a.active_near(lat, lon, crate::state::STATION_ALERT_RADIUS_KM) {
        Ok(w) => {
            let n = w.len();
            #[cfg(not(target_os = "android"))]
            crate::voice::notify_near(&w);
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
            #[cfg(not(target_os = "android"))]
            crate::voice::notify_near(&w);
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
    // UV index (Open-Meteo — NWS does not publish one). One small request per
    // refresh cadence.
    match f.uv_index(lat, lon) {
        Ok(uv) => {
            let _ = out.send(DataMsg::UvIndex(uv));
        }
        Err(e) => log::debug!("UV index fetch failed: {e:#}"),
    }
}

/// Fetch every SPC outlook product SPC publishes for `day` — categorical
/// plus tornado/wind/hail probabilities — so the map can show them at once.
/// The selected type goes first so its legend fills in without waiting on the
/// other three downloads. Errors for the extra products are logged only; the
/// status bar reports a failure of the selected product.
fn refresh_outlook(
    src: &OutlookSource,
    day: u8,
    selected: radar_core::outlooks::OutlookType,
    out: &Sender<DataMsg>,
) {
    use radar_core::outlooks::OutlookType as T;
    let available = radar_core::outlooks::probabilities_available(day);
    let selected = if selected.is_probability() && !available {
        T::Categorical
    } else {
        selected
    };
    let mut types = vec![selected];
    for ty in [T::Categorical, T::Tornado, T::Wind, T::Hail] {
        if ty == selected || (ty.is_probability() && !available) {
            continue;
        }
        types.push(ty);
    }
    for ty in types {
        match src.fetch(day, ty) {
            Ok(o) => {
                let issued = o
                    .issue
                    .map(|t| crate::timefmt::fmt_central(&t))
                    .unwrap_or_else(|| "?".into());
                log::info!(
                    "outlook day {day} {}: {} areas, issued {issued}",
                    ty.label(),
                    o.features.len()
                );
                let _ = out.send(DataMsg::Outlook(o));
            }
            Err(e) => {
                log::warn!("outlook day {day} {} fetch failed: {e:#}", ty.label());
                if ty == selected {
                    let _ = out.send(DataMsg::Error(format!(
                        "outlook day {day} {}: {e:#}",
                        ty.label()
                    )));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Background radar follows every category that Storm Watch offers:
    /// tornado/severe warnings, watches, and all other warnings. Advisories
    /// and statements deliberately do not trigger it.
    #[test]
    fn background_radar_covers_every_watch_and_warning_family() {
        let warning = |event: &str| radar_core::warnings::Warning {
            id: event.into(),
            event: event.into(),
            severity: "Severe".into(),
            urgency: "Expected".into(),
            expires: Utc::now(),
            office: "OUN".into(),
            headline: String::new(),
            description: String::new(),
            instruction: String::new(),
            area: String::new(),
            polygon: None,
            zone_urls: Vec::new(),
            zone_only: false,
        };
        for event in [
            "Tornado Warning",
            "Tornado Watch",
            "Severe Thunderstorm Warning",
            "Severe Thunderstorm Watch",
            "Flash Flood Warning",
            "Winter Storm Warning",
            "Blizzard Warning",
            "Winter Weather Advisory",
            "Ice Storm Advisory",
            "High Wind Warning",
            "Flash Flood Emergency",
        ] {
            assert!(is_watch_or_warning(&warning(event)), "{event}");
        }
        for event in ["Wind Advisory", "Special Weather Statement"] {
            assert!(!is_watch_or_warning(&warning(event)), "{event}");
        }
    }

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

    /// Time-of-day targeting: the window covers the requested (Central)
    /// minute, wraps across midnight, and clamps at the day's edges.
    #[test]
    fn archive_window_covers_the_requested_time_of_day() {
        use chrono::TimeZone;
        // 18 scans, Central hours 00:00..17:00. Central is UTC-5 (CDT), so
        // UTC hour = h + 5 on the same date.
        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 14).unwrap();
        let mut vols: Vec<radar_data::ArchiveVolumeRef> = (0..18)
            .map(|h| radar_data::ArchiveVolumeRef {
                site: "KTWX".into(),
                date,
                key: format!("2026/10/14/KTWX/KTWX20261014_{:02}0000_V06", h + 5),
                volume_start: chrono::Utc
                    .with_ymd_and_hms(2026, 10, 14, (h as u32) + 5, 0, 0)
                    .unwrap(),
                size_bytes: 0,
            })
            .collect();
        vols.sort_by(|a, b| b.volume_start.cmp(&a.volume_start)); // newest-first

        // 09:00 is the 9th-newest entry (index 8); centered in a 6-wide window.
        assert_eq!(
            window_start_for_minute(&vols, 9 * 60, 6),
            8usize.saturating_sub(3)
        );
        // 00:30 wraps to the 00:00 scan (newest index 17) and clamps low.
        assert_eq!(window_start_for_minute(&vols, 30, 6), 12);
        // 23:50 is circularly closest to 00:00 as well.
        assert_eq!(window_start_for_minute(&vols, 23 * 60 + 50, 6), 12);
        // Degenerate input.
        assert_eq!(window_start_for_minute(&[], 600, 6), 0);
    }
}
