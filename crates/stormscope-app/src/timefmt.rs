//! Display-time formatting: everything the user sees uses the device/OS local
//! timezone on a 12-hour clock.

use chrono::{DateTime, Utc};
use std::sync::OnceLock;

/// The device/OS IANA timezone. Using `chrono_tz` (instead of `chrono::Local`)
/// gives proper short names such as CDT/CST/PDT rather than a numeric offset.
static LOCAL_TZ: OnceLock<chrono_tz::Tz> = OnceLock::new();

pub fn local() -> &'static chrono_tz::Tz {
    LOCAL_TZ.get_or_init(|| {
        iana_time_zone::get_timezone()
            .ok()
            .and_then(|name| name.parse().ok())
            .unwrap_or(chrono_tz::America::Chicago)
    })
}

pub fn central() -> &'static chrono_tz::Tz {
    local()
}

/// "2:35:04 PM CDT" — scan times, archive stamps.
pub fn fmt_central(dt: &DateTime<Utc>) -> String {
    dt.with_timezone(central())
        .format("%-I:%M:%S %p %Z")
        .to_string()
}
