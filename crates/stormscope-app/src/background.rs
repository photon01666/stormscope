//! Background data coordination: owns the fetch/decode/alert threads and
//! shuttles results to the UI. Runs on its own thread (spawned by StormApp).
//!
//! This is the real data coordinator — no more heartbeat stubs. A site
//! change restarts the [`S3Source`] real-time poller; an archive request
//! lists + fetches that date's volumes off-thread. Everything heavy happens
//! here, never on the UI thread.

use crate::app::{DataCmd, DataMsg};
use chrono::NaiveDate;
use radar_data::{AlertSource, DiskCache, S3Source};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

/// How far back an archive request reaches when listing a day's volumes.
const ARCHIVE_MAX_VOLUMES: usize = 24;

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
    let mut live: Option<Receiver<radar_core::Volume>> = None;

    let _ = out.send(DataMsg::Status("starting live feed…".into()));
    start_live(&source, "KTLX", &mut live, &out);

    while let Ok(cmd) = cmds.recv() {
        match cmd {
            DataCmd::SetSite(site) => {
                let _ = out.send(DataMsg::Status(format!("switching to {site}…")));
                start_live(&source, &site, &mut live, &out);
            }
            DataCmd::LoadArchiveDate(date) => {
                let _ = out.send(DataMsg::Status(format!("loading archive {date}…")));
                load_archive(&source, "KTLX", date, &out);
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
        }

        // Drain any live volume that arrived while we were handling the
        // command, so backpressure doesn't stall the poller across a pause.
        drain_live(&mut live, &out);
    }
}

/// Restart the real-time poller for `site`.
fn start_live(
    source: &S3Source,
    site: &str,
    live: &mut Option<Receiver<radar_core::Volume>>,
    out: &Sender<DataMsg>,
) {
    match source.start_realtime(site) {
        Ok(rx) => {
            *live = Some(rx);
            let _ = out.send(DataMsg::Status(format!("live {site}: polling…")));
        }
        Err(e) => {
            *live = None;
            let _ = out.send(DataMsg::Error(format!("live {site}: {e:#}")));
        }
    }
}

/// Forward any volumes the live poller has produced. Non-blocking.
fn drain_live(live: &mut Option<Receiver<radar_core::Volume>>, out: &Sender<DataMsg>) {
    let Some(rx) = live else { return };
    loop {
        match rx.try_recv() {
            Ok(volume) => {
                let scan = volume.meta.volume_start;
                let _ = out.send(DataMsg::Volume(std::sync::Arc::new(volume)));
                let _ = out.send(DataMsg::Status(format!("live scan {}", scan.format("%H:%M:%S"))));
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                let _ = out.send(DataMsg::Status("live feed ended".into()));
                *live = None;
                break;
            }
        }
    }
}

/// List + fetch a day's archive volumes, emitting each decoded volume.
/// Runs on this background thread; the UI just sees DataMsg::Volume.
fn load_archive(source: &S3Source, site: &str, date: NaiveDate, out: &Sender<DataMsg>) {
    let vols = match source.list_archive(site, date) {
        Ok(v) => v,
        Err(e) => {
            let _ = out.send(DataMsg::Error(format!("archive {date}: {e:#}")));
            return;
        }
    };
    let total = vols.len().min(ARCHIVE_MAX_VOLUMES);
    if total == 0 {
        let _ = out.send(DataMsg::Status(format!("no volumes for {site} on {date}")));
        return;
    }
    let _ = out.send(DataMsg::Status(format!("{total} volumes for {date}, fetching…")));
    // Oldest first so the UI ring buffer plays forward in time.
    for r in vols.into_iter().take(ARCHIVE_MAX_VOLUMES).rev() {
        match source.fetch_archive(&r) {
            Ok(volume) => {
                let scan = volume.meta.volume_start;
                let _ = out.send(DataMsg::Volume(volume));
                let _ = out.send(DataMsg::Status(format!(
                    "archive {} {}/{}",
                    scan.format("%H:%M:%S"),
                    site,
                    date.format("%m-%d")
                )));
            }
            Err(e) => {
                log::warn!("archive fetch {} failed: {e:#}", r.file_name());
                let _ = out.send(DataMsg::Error(format!("{}: {e:#}", r.file_name())));
            }
        }
        // Politeness gap between archive downloads.
        std::thread::sleep(Duration::from_millis(120));
    }
}
