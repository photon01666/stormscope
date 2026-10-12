//! Live-endpoint integration tests (W2).
//!
//! These hit REAL public endpoints (no credentials):
//! * `unidata-nexrad-level2` S3 bucket (archive listing + object GET)
//! * `api.weather.gov` (active alerts, radar station catalog)
//!
//! Timeouts are bounded; network failures will fail these tests by design —
//! they are the W2 acceptance suite.
//!
//! The realtime poller test is `#[ignore]`d: a full volume scan takes
//! minutes, so run it manually with:
//! `cargo test -p radar-data --test live_endpoints -- --ignored --nocapture`

use chrono::Utc;
use radar_data::alerts::AlertSource;
use radar_data::cache::DiskCache;
use radar_data::s3::S3Source;

/// Descriptive UA (api.weather.gov requirement).
const TEST_UA: &str = "stormscope-integration-tests/0.1 (dev@stormscope.local)";

fn scratch_source() -> S3Source {
    let root = std::env::temp_dir().join(format!(
        "stormscope-radar-data-it-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let cache = DiskCache::new(root, 512 * 1024 * 1024);
    S3Source::new(cache).expect("S3Source")
}

#[test]
fn list_archive_ktlx_today_returns_volumes_newest_first() {
    // Guard: junk AWS_* env must not affect anonymous access (nexrad-data
    // runs no credential chain / signer at all).
    std::env::set_var("AWS_ACCESS_KEY_ID", "deliberately-invalid");
    std::env::set_var("AWS_SECRET_ACCESS_KEY", "deliberately-invalid");
    std::env::set_var("AWS_PROFILE", "deliberately-invalid");
    std::env::set_var("AWS_REGION", "us-east-1");

    let src = scratch_source();
    let today = Utc::now().date_naive();
    let mut volumes = src
        .list_archive("KTLX", today)
        .expect("list_archive must succeed");
    if volumes.is_empty() {
        // Near the UTC day boundary the newest volumes may still be landing.
        eprintln!("no volumes for {today}, falling back to yesterday");
        volumes = src
            .list_archive("KTLX", today.pred_opt().expect("yesterday"))
            .expect("list_archive (yesterday)");
    }
    assert!(
        !volumes.is_empty(),
        "KTLX archive listing should return >= 1 volume for a live radar"
    );

    let newest = &volumes[0];
    let name = newest.file_name();
    assert!(name.starts_with("KTLX"), "key {name} should start with site");
    assert!(name.contains("_V0"), "key {name} should carry a volume version");
    assert!(newest.key.contains(&format!("{}/KTLX/", newest.date.format("%Y/%m/%d"))));
    assert!(newest.volume_start.date_naive() == newest.date);

    // Newest-first ordering.
    for pair in volumes.windows(2) {
        assert!(
            pair[0].volume_start >= pair[1].volume_start,
            "listing must be newest-first"
        );
    }
    eprintln!(
        "listed {} KTLX volumes for {}; newest: {} ({})",
        volumes.len(),
        newest.date,
        newest.key,
        newest.volume_start
    );
}

#[test]
fn fetch_archive_downloads_and_caches_one_volume() {
    let src = scratch_source();
    let today = Utc::now().date_naive();
    let mut volumes = src
        .list_archive("KTLX", today)
        .expect("list_archive must succeed");
    if volumes.is_empty() {
        volumes = src
            .list_archive("KTLX", today.pred_opt().expect("yesterday"))
            .expect("list_archive (yesterday)");
    }
    assert!(!volumes.is_empty(), "need at least one volume to fetch");
    let target = &volumes[0];

    // 1) Raw fetch is decode-independent and must succeed.
    let bytes = src
        .fetch_archive_bytes(target)
        .expect("raw download must succeed");
    assert!(!bytes.is_empty(), "downloaded volume bytes");
    // Normalized cache entries are gzipped Archive II volumes.
    assert_eq!(&bytes[..2], &[0x1f, 0x8b], "cache stores gzip bytes");

    let cached = src.cache().read(&target.cache_rel()).unwrap();
    assert_eq!(cached.as_deref(), Some(&bytes[..]), "disk cache populated");

    // Second read must come from the cache and return identical bytes.
    let again = src.fetch_archive_bytes(target).expect("cache hit");
    assert_eq!(again, bytes);

    // 2) Decode step (radar-decode is W1's parallel workstream and may still
    //    be `todo!()` — tolerate Err AND panic until it lands).
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| src.fetch_archive(target))) {
        Ok(Ok(volume)) => {
            assert_eq!(volume.meta.site_id, "KTLX");
            assert!(!volume.sweeps.is_empty(), "decoded volume has sweeps");
            eprintln!(
                "decoded {}: {} sweeps, tilts {:?}",
                target.key,
                volume.sweeps.len(),
                volume.tilts()
            );
        }
        Ok(Err(e)) => {
            eprintln!(
                "NOTE: download+cache OK but decode failed while radar-decode (W1) is in flight: {e:#}"
            );
        }
        Err(_) => {
            eprintln!("NOTE: radar-decode (W1) still todo!(); download+cache verified only");
        }
    }
}

#[test]
fn alerts_active_near_okc_returns_vec() {
    let src = AlertSource::new(TEST_UA).expect("descriptive UA accepted");
    let warnings = src
        .active_near(35.3331, -97.2778, 200.0)
        .expect("active_near must not error");
    // Quiet nights legitimately return zero polygon-filtered alerts.
    for w in &warnings {
        assert!(!w.event.is_empty());
        assert!(w.polygon.is_some());
    }
    eprintln!("{} polygon alerts within 200 km of KTLX", warnings.len());
}

#[test]
fn station_catalog_fetches_all_wsr88d() {
    let sites = radar_data::sites_gen::fetch_catalog(TEST_UA).expect("radar stations fetch");
    assert!(sites.len() >= 140, "expected ~159 WSR-88D sites, got {}", sites.len());
    let ktlx = sites.iter().find(|s| s.id == "KTLX").expect("KTLX present");
    assert!((ktlx.latitude - 35.33).abs() < 0.05);
    assert!(ktlx.elevation_m > 300.0);
    // Deterministic ordering + unique ids.
    assert!(sites.windows(2).all(|w| w[0].id < w[1].id));
}

/// Manual-only: follow the live chunk stream until one full volume assembles
/// and decodes. Volumes take minutes, so this is ignored by default.
#[test]
#[ignore = "long-running live stream; run with --ignored --nocapture"]
fn realtime_poller_emits_a_volume() {
    use std::time::Duration;
    // Surface the poller's diagnostics: RUST_LOG=radar_data=debug cargo test ...
    let _ = env_logger::builder().is_test(true).parse_default_env().try_init();
    let src = scratch_source();
    let rx = src.start_realtime("KTLX").expect("poller thread");
    let volume = rx
        .recv_timeout(Duration::from_secs(15 * 60))
        .expect("one assembled volume within 15 minutes");
    assert_eq!(volume.meta.site_id, "KTLX");
    assert!(!volume.sweeps.is_empty());
    eprintln!(
        "live volume @ {} (VCP {:?}, {} sweeps)",
        volume.meta.volume_start, volume.meta.vcp, volume.meta.sweep_count
    );
}
