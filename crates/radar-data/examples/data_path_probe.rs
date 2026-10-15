//! Timing probe for the stormscope data path.
//!
//! Reproduces, step by step and with wall-clock timings, what the app does:
//!   1. archive listing for a Central day (list_central_day equivalent),
//!   2. realtime `get_latest_volume` binary search,
//!   3. `list_chunks_in_volume` for the latest volume,
//!   4. chunk download of one volume's chunk set.
//!
//! Run with: cargo run --release --example data_path_probe -p radar-data [SITE]
//!
//! The probe uses its own scratch cache so it never disturbs the app's.

use radar_data::cache::DiskCache;
use radar_data::s3::S3Source;
use std::time::Instant;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let site = std::env::args().nth(1).unwrap_or_else(|| "KICT".into());

    // The blocking S3Source drives its own runtime (`block_on`), which cannot
    // nest inside an async context — run that half on a plain thread.
    let site_a = site.clone();
    std::thread::spawn(move || archive_half(&site_a))
        .join()
        .expect("archive probe thread");

    println!("== realtime path (what the live poller uses) ==");
    use nexrad_data::aws::realtime::{download_chunk, get_latest_volume, list_chunks_in_volume};
    let t0 = Instant::now();
    let latest = get_latest_volume(&site).await?;
    println!(
        "  get_latest_volume: {:?} in {} calls / {:.2?}",
        latest.volume.as_ref().map(|v| v.as_number()),
        latest.calls,
        t0.elapsed()
    );
    if let Some(volume) = latest.volume {
        let t0 = Instant::now();
        let mut chunks = list_chunks_in_volume(&site, volume, 200).await?;
        chunks.sort_by_key(|c| c.sequence());
        println!(
            "  list_chunks_in_volume({}): {} chunks in {:.2?}",
            volume.as_number(),
            chunks.len(),
            t0.elapsed()
        );
        if let (Some(first), Some(last)) = (chunks.first(), chunks.last()) {
            println!(
                "  first {} (upload {:?}) last {} (upload {:?})",
                first.name(),
                first.upload_date_time(),
                last.name(),
                last.upload_date_time()
            );
            let now = chrono::Utc::now();
            if let Some(u) = last.upload_date_time() {
                println!("  last chunk uploaded {:.0}s ago", (now - u).num_seconds());
            }
        }
        let t0 = Instant::now();
        let mut total = 0usize;
        let mut downloaded = 0usize;
        let mut epochs: Vec<chrono::NaiveDateTime> =
            chunks.iter().map(|c| *c.date_time_prefix()).collect();
        epochs.sort();
        epochs.dedup();
        for c in &chunks {
            match download_chunk(&site, c).await {
                Ok((_, data)) => {
                    downloaded += 1;
                    total += data.data().len();
                }
                Err(e) => println!("    chunk {} download failed: {e}", c.name()),
            }
        }
        println!(
            "  downloaded {downloaded}/{} chunks, {total} bytes in {:.2?}",
            chunks.len(),
            t0.elapsed()
        );
        println!("  distinct chunk epochs (oldest..newest): {epochs:?}");
    }
    Ok(())
}

/// The archive half — same calls as `list_central_day` + `fetch_archive_bytes`.
fn archive_half(site: &str) {
    use chrono::TimeZone;

    println!("== archive path (what prime_site + archive_catchup use) ==");
    let root = std::env::temp_dir().join(format!("stormscope-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let cache = DiskCache::new(root, 1 << 30);
    let src = match S3Source::new(cache) {
        Ok(s) => s,
        Err(e) => {
            println!("  S3Source init failed: {e:#}");
            return;
        }
    };

    // "Central day" == the as-of-now Central date; both UTC prefixes that
    // window can touch, exactly like list_central_day in background.rs.
    let now_central = chrono::Utc::now().with_timezone(&chrono_tz::Tz::America__Chicago);
    let date = now_central.date_naive();
    let start = chrono_tz::America::Chicago
        .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .expect("central midnight")
        .with_timezone(&chrono::Utc);
    let end = start + chrono::Duration::days(1);

    let mut vols = Vec::new();
    for utc_date in [
        start.date_naive(),
        (end - chrono::Duration::seconds(1)).date_naive(),
    ] {
        let t0 = Instant::now();
        match src.list_archive(site, utc_date) {
            Ok(v) => {
                println!(
                    "  list_archive {site} {utc_date}: {} vols in {:.2?}",
                    v.len(),
                    t0.elapsed()
                );
                if let (Some(first), Some(last)) = (v.first(), v.last()) {
                    println!(
                        "    range: {} ({}) .. {} ({})",
                        first.file_name(),
                        first.volume_start,
                        last.file_name(),
                        last.volume_start
                    );
                }
                for r in v {
                    if r.volume_start >= start && r.volume_start < end {
                        vols.push(r);
                    }
                }
            }
            Err(e) => println!(
                "  list_archive {site} {utc_date} FAILED in {:.2?}: {e:#}",
                t0.elapsed()
            ),
        }
    }
    vols.sort_by(|a, b| b.volume_start.cmp(&a.volume_start));
    println!("  {} vols inside the Central day", vols.len());
    if let Some(newest) = vols.first() {
        println!(
            "  newest archive scan: {} ({:.0}s behind now)",
            newest.key,
            (chrono::Utc::now() - newest.volume_start).num_seconds()
        );
        let t0 = Instant::now();
        match src.fetch_archive_bytes(newest) {
            Ok(b) => println!(
                "  download newest: {} bytes in {:.2?}",
                b.len(),
                t0.elapsed()
            ),
            Err(e) => println!("  download newest FAILED in {:.2?}: {e:#}", t0.elapsed()),
        }
    }
}
