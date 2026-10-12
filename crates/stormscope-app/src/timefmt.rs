//! Display-time formatting: everything the user sees is Central time
//! (America/Chicago — CDT now, CST in winter) on a 12-hour clock.

use chrono::{DateTime, Utc};

/// Central timezone (DST-aware via chrono-tz).
pub fn central() -> &'static chrono_tz::Tz {
    &chrono_tz::America::Chicago
}

/// "2:35:04 PM CDT" — scan times, archive stamps.
pub fn fmt_central(dt: &DateTime<Utc>) -> String {
    dt.with_timezone(central()).format("%-I:%M:%S %p %Z").to_string()
}

/// "2:35 PM CDT" — shorter form for the timeline ticks.
pub fn fmt_central_short(dt: &DateTime<Utc>) -> String {
    dt.with_timezone(central()).format("%-I:%M %p %Z").to_string()
}
