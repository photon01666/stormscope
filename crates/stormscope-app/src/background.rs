//! Background data coordination: owns the fetch/decode/alert threads and
//! shuttles results to the UI. Runs on its own thread (spawned by StormApp).
//!
//! This is the real data coordinator — no more heartbeat stubs. A site
//! change restarts the [`S3Source`] real-time poller; an archive request
//! lists + fetches that date's volumes off-thread. Everything heavy happens
//! here, never on the UI thread.

use crate::app::{DataCmd, DataMsg};
use chrono::{NaiveDate, Utc};
use radar_data::{AlertSource, DiskCache, S3Source};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

/// How far back an archive request reaches when listing a day's volumes.
const ARCHIVE_MAX_VOLUMES: usize = 24;

/// Inclusive-exclusive UTC instant window of a Central calendar day.
fn central_day_window(date: NaiveDate) -> (chrono::DateTime<Utc>, chrono::DateTime<Utc>) {
    use chrono::TimeZone;
    let start = Utc
        .from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
        .with_timezone(crate::timefmt::central())
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
    for utc_date in [start.date_naive(), (end - chrono::Duration::seconds(1)).date_naive()] {
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
    let alerts = AlertSource::new("stormscope/0.1 (Linux NEXRAD radar viewer)").ok();

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
    let mut last_delivered: Option<chrono::DateTime<Utc>> = None;
    let mut watchdog_enabled = true; // off while browsing archive/last-hour
    let mut last_watchdog = std::time::Instant::now();
    const ARCHIVE_CATCHUP_INTERVAL: Duration = Duration::from_secs(60);

    let _ = out.send(DataMsg::Status("starting live feed…".into()));
    start_live(&source, radar_core::DEFAULT_SITE, &mut live, &out);
    // First frame in seconds, not minutes: the poller warms up in parallel.
    last_delivered = instant_refresh(&source, radar_core::DEFAULT_SITE, &out);

    // Dev harness: STORMSCOPE_BOOTSTRAP_ARCHIVE=YYYY-MM-DD[=SITE] pre-loads
    // an archive day at startup so the archive path can be exercised (and
    // demoed) without driving the GUI.
    if let Ok(spec) = std::env::var("STORMSCOPE_BOOTSTRAP_ARCHIVE") {
        let mut parts = spec.split('=');
        let date = parts.next().and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
        let site = parts.next().unwrap_or(radar_core::DEFAULT_SITE).to_string();
        if let Some(date) = date {
            let _ = out.send(DataMsg::Status(format!("bootstrap: loading {site} {date}…")));
            watchdog_enabled = false; // browsing a day, not live
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
                    // Radar for the new site within seconds; the poller's
                    // first complete scan would otherwise take ~4-5 min.
                    if let Some(ts) = instant_refresh(&source, &site, &out) {
                        last_delivered = Some(ts);
                    }
                }
                DataCmd::LoadArchiveDate(site, date) => {
                    let _ = out.send(DataMsg::Status(format!("loading {site} archive {date}…")));
                    watchdog_enabled = false; // browsing a day, not live
                    archive = open_archive_day(&source, &site, date, &mut current_site, &out);
                }
                DataCmd::LoadLastHour(site) => {
                    current_site = site.clone();
                    archive = None;
                    watchdog_enabled = false; // browsing a fixed window
                    let _ = out.send(DataMsg::ArchiveInfo { total: 0, start: 0, len: 0 });
                    load_last_hour(&source, &site, &out);
                }
                DataCmd::ArchiveWindowShift(delta) => {
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
                    if let Some(ts) = instant_refresh(&source, &current_site, &out) {
                        last_delivered = Some(ts);
                    }
                }
                DataCmd::RefreshAlerts => {
                    if let Some(a) = &alerts {
                        match a.active_near(35.3331, -97.2778, 400.0) {
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
                    }
                }
                DataCmd::Shutdown => break,
            },
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }

        // Always drain whatever the live poller has produced this tick, so
        // volumes flow to the UI even when no command ever arrives.
        if let Some(ts) = drain_live(&mut live, &out) {
            last_delivered = Some(ts);
        }

        // Archive catch-up watchdog: if the archive bucket already has a
        // newer scan than we've shown, take it — the app is then only as
        // slow as the fastest upstream. ~2 S3 list req/min, no rate risk.
        if watchdog_enabled && last_watchdog.elapsed() >= ARCHIVE_CATCHUP_INTERVAL {
            last_watchdog = std::time::Instant::now();
            if let Some(ts) = archive_catchup(&source, &current_site, last_delivered, &out) {
                last_delivered = Some(ts);
            }
        }
    }
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
    let mut day = ArchiveDay { vols, window_start: 0 };
    stream_window(source, site, date, &day, window_len, out);
    let _ = out.send(DataMsg::ArchiveInfo { total, start: 0, len: window_len });
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
    let new_start = (day.window_start as i32 + delta * window_len as i32).clamp(0, max_start as i32) as usize;
    if new_start == day.window_start {
        let _ = out.send(DataMsg::Status(
            "already at the end of that day".into(),
        ));
        return;
    }
    day.window_start = new_start;
    let site = day.vols[0].site.clone();
    let date = day.vols[0].date;
    let _ = out.send(DataMsg::ClearFrames);
    stream_window(source, &site, date, day, window_len, out);
    let _ = out.send(DataMsg::ArchiveInfo { total, start: new_start, len: window_len });
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

/// Immediately fetch + emit the newest completed scan of `site` for *today*
/// from the archive bucket. Returns the delivered scan time (for freshness
/// tracking). The live poller only produces a frame every ~4-7 min (a full
/// NEXRAD scan takes that long), which felt like "not downloading".
fn instant_refresh(
    source: &S3Source,
    site: &str,
    out: &Sender<DataMsg>,
) -> Option<chrono::DateTime<Utc>> {
    let today = chrono::Utc::now()
        .with_timezone(crate::timefmt::central())
        .date_naive();
    match list_central_day(source, site, today) {
        Ok(vols) => {
            let r = vols.first()?;
            match source.fetch_archive(r) {
                Ok(v) => {
                    log::info!("live refresh: scan {}", crate::timefmt::fmt_central(&r.volume_start));
                    let _ = out.send(DataMsg::Volume(v));
                    let _ = out.send(DataMsg::Status(format!(
                        "live · {}",
                        crate::timefmt::fmt_central(&r.volume_start)
                    )));
                    Some(r.volume_start)
                }
                Err(e) => {
                    log::warn!("instant refresh fetch failed: {e:#}");
                    None
                }
            }
        }
        Err(e) => {
            log::warn!("instant refresh listing failed: {e:#}");
            None
        }
    }
}

/// One polite catch-up check: if the archive bucket has a scan *newer* than
/// what we've already shown, fetch and emit it now. Combined with the live
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
        return None; // we're already current
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
    let Some((rx, _poller)) = live else { return None };
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

