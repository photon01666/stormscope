//! Preliminary Local Storm Reports (LSR) — the NWS's realtime feed of storm
//! reports, most of them from trained spotters.
//!
//! Every weather office issues an LSR product when a report comes in; the
//! API lists them at `/products/types/LSR` and the product *text* carries
//! everything needed to plot one: report time, event, magnitude, city,
//! county/state, **lat/lon**, and the source (`Trained Spotter`,
//! `Public`, `Law Enforcement`, …). Reports arrive within minutes of the
//! event, so this is the "what just happened on the ground" layer.
//!
//! The text is a fixed-width product whose columns come straight from its own
//! header template (`..TIME... ...EVENT... ...CITY LOCATION... ...LAT.LON...`),
//! which is what [`parse_product`] slices against.

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashMap;

const LIST_URL: &str = "https://api.weather.gov/products/types/LSR";
const PRODUCT_URL: &str = "https://api.weather.gov/products/";

/// What kind of event a report describes, for coloring and filtering. Matched
/// from the event text, which is abbreviated (`Non-Tstm Wnd Gst` and friends).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportKind {
    Tornado,
    Wind,
    Hail,
    Flood,
    Rain,
    Snow,
    Other,
}

impl ReportKind {
    pub fn from_event(event: &str) -> Self {
        let e = event.to_ascii_lowercase();
        // Order matters: "Flash Flood" must reach Flood, wind spellings
        // include the non-thunderstorm abbreviations, and rain/snow totals
        // come through as their own events.
        if e.contains("tornado") {
            Self::Tornado
        } else if e.contains("hail") {
            Self::Hail
        } else if e.contains("wnd") || e.contains("wind") || e.contains("gust") {
            Self::Wind
        } else if e.contains("flood") {
            Self::Flood
        } else if e.contains("rain") {
            Self::Rain
        } else if e.contains("snow")
            || e.contains("winter")
            || e.contains("ice")
            || e.contains("freezing")
        {
            Self::Snow
        } else {
            Self::Other
        }
    }

    /// Short label for map markers and tooltips.
    pub fn label(self) -> &'static str {
        match self {
            Self::Tornado => "tornado",
            Self::Wind => "wind",
            Self::Hail => "hail",
            Self::Flood => "flood",
            Self::Rain => "rain",
            Self::Snow => "snow/ice",
            Self::Other => "report",
        }
    }
}

/// One report line from an LSR product. Times are the *local* wall-clock
/// strings the product printed (`"0744 PM"` + `"10/15/2026"`) — shown as
/// reported, because decoding each state's local time zone from the office's
/// issuance line would be guesswork; [`Lsr::issued`] carries the absolute
/// product issuance for age.
#[derive(Debug, Clone, PartialEq)]
pub struct Lsr {
    /// Product id (uuid) plus the report's index within it — stable across
    /// polls, used to dedupe and to look the full text up again.
    pub id: String,
    pub event: String,
    pub time: String,
    pub date: String,
    pub magnitude: String,
    pub city: String,
    pub county: String,
    pub state: String,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub source: String,
    pub remarks: String,
    /// Issuing WFO (e.g. `KMPX`).
    pub office: String,
    /// RFC3339 issuance of the product, for age.
    pub issued: String,
    /// The whole product text (what the detail popup shows).
    pub text: String,
}

impl Lsr {
    pub fn kind(&self) -> ReportKind {
        ReportKind::from_event(&self.event)
    }

    /// Plot position, if the product carried coordinates.
    pub fn center(&self) -> Option<(f64, f64)> {
        match (self.lat, self.lon) {
            (Some(la), Some(lo)) => Some((la, lo)),
            _ => None,
        }
    }

    /// Whether the report came from a trained spotter (the "verified
    /// spotters" of the storm-reporting world) rather than the general
    /// public, law enforcement, or a broadcast media observer.
    pub fn is_spotter(&self) -> bool {
        self.source.to_ascii_lowercase().contains("spotter")
    }

    pub fn issued_at(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.issued)
            .ok()
            .map(|d| d.with_timezone(&Utc))
    }

    /// One-line label: `"tornado — 5 N Custer, SD"`.
    pub fn summary(&self) -> String {
        let place = if self.city.is_empty() {
            format!("{}, {}", self.county, self.state)
        } else {
            format!("{}, {}", self.city, self.state)
        };
        format!("{} — {place}", self.kind().label())
    }

    /// The product text without the WMO/AWIPS header, ready to display.
    pub fn display_text(&self) -> &str {
        strip_wmo_header(&self.text)
    }
}

/// Fetches and parses LSR products. Product texts are cached by id, so a
/// poll costs one list request plus whatever *new* products appeared — the
/// first fill of a window is the only expensive one, and it is paced.
pub struct LsrSource {
    client: reqwest::blocking::Client,
    texts: HashMap<String, String>,
}

impl LsrSource {
    pub fn new(user_agent: &str) -> Result<Self> {
        Ok(Self {
            client: crate::alerts::api_client(user_agent)?,
            texts: HashMap::new(),
        })
    }

    /// Reports issued within `max_age`, newest product first. Cached texts
    /// are pruned to the kept set so memory tracks the window.
    pub fn recent(&mut self, max_age: chrono::Duration) -> Result<Vec<Lsr>> {
        let resp = self
            .client
            .get(LIST_URL)
            .header("Accept", "application/geo+json")
            .send()?
            .error_for_status()?;
        let list: Value = resp.json()?;
        let entries = list
            .get("@graph")
            .and_then(|g| g.as_array())
            .ok_or_else(|| anyhow!("api.weather.gov LSR list has no @graph"))?;

        let now = Utc::now();
        // (id, office, issued) for products inside the window.
        let mut keep: Vec<(String, String, String)> = Vec::new();
        for e in entries {
            let Some(id) = e.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            let issued = e
                .get("issuanceTime")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if let Ok(t) = DateTime::parse_from_rfc3339(&issued) {
                let age = now.signed_duration_since(t.with_timezone(&Utc));
                if age > max_age || age < chrono::Duration::zero() - chrono::Duration::minutes(5) {
                    continue;
                }
            }
            let office = e
                .get("issuingOffice")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            keep.push((id.to_string(), office, issued));
        }
        // Bound the first fill: the list spans more products than any view
        // needs, and each unseen product is one request (paced). Newest
        // first, so truncating keeps the most recent reports.
        keep.truncate(80);
        self.texts
            .retain(|id, _| keep.iter().any(|(k, _, _)| k == id));

        // Fetch texts we have not seen yet, paced when it is a bulk fill
        // (first run over a whole window) so we stay a polite client.
        let missing = keep
            .iter()
            .filter(|(id, _, _)| !self.texts.contains_key(id))
            .count();
        let bulk = missing > 5;
        for (id, _, _) in &keep {
            if self.texts.contains_key(id) {
                continue;
            }
            if bulk {
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            match self
                .client
                .get(format!("{PRODUCT_URL}{id}"))
                .header("Accept", "application/geo+json")
                .send()
            {
                Ok(resp) if resp.status().is_success() => {
                    let text = resp
                        .json::<Value>()
                        .ok()
                        .and_then(|p| {
                            p.get("productText")
                                .and_then(|t| t.as_str())
                                .map(str::to_string)
                        })
                        .unwrap_or_default();
                    self.texts.insert(id.clone(), text);
                }
                Ok(resp) => log::debug!("LSR {id}: HTTP {}", resp.status()),
                Err(e) => log::debug!("LSR {id}: {e:#}"),
            }
        }

        let mut out: Vec<Lsr> = Vec::new();
        for (id, office, issued) in &keep {
            if let Some(text) = self.texts.get(id) {
                out.extend(parse_product(text, id, office, issued));
            }
        }
        out.sort_by(|a, b| b.issued.cmp(&a.issued));
        Ok(out)
    }
}

/// Column positions come from the product's own header template.
const TIME_COL: std::ops::Range<usize> = 0..11;
const EVENT_COL: std::ops::Range<usize> = 12..29;
const CITY_COL: std::ops::Range<usize> = 29..53;
const LATLON_COL: std::ops::Range<usize> = 53..usize::MAX;
const DATE_COL: std::ops::Range<usize> = 0..10;
const MAG_COL: std::ops::Range<usize> = 12..29;
const COUNTY_COL: std::ops::Range<usize> = 29..48;
const STATE_COL: std::ops::Range<usize> = 48..53;
const SOURCE_COL: std::ops::Range<usize> = 53..usize::MAX;

/// Slice a byte range of a line without panicking: clamps to the line and
/// backs off a cut that landed inside a multi-byte character.
fn col(line: &str, range: std::ops::Range<usize>) -> &str {
    let mut end = range.end.min(line.len());
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    let mut start = range.start.min(end);
    while start > 0 && !line.is_char_boundary(start) {
        start -= 1;
    }
    line[start..end].trim()
}

/// Parse one LSR product into its reports. A product holds one or more
/// two-line reports (report line + date/mag/county/source line), optionally
/// followed by indented remark lines.
pub fn parse_product(text: &str, product_id: &str, office: &str, issued: &str) -> Vec<Lsr> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        // A report line starts with "HHMM AM|PM".
        if !is_report_line(line) {
            continue;
        }
        let (time, event, city, lat, lon) = match parse_report_line(line) {
            Some(parts) => parts,
            None => continue,
        };
        // Its date/mag/county/source line follows.
        let (date, magnitude, county, state, source) = lines
            .get(i + 1)
            .filter(|l| !l.trim().is_empty())
            .map(|l| parse_detail_line(l))
            .unwrap_or_default();
        // Remarks: indented lines that start after *one* blank (every real
        // product puts one there), ending at the next blank, an unindented
        // line, the next report, or the trailer (`&&` / `$$`).
        let mut remarks: Vec<&str> = Vec::new();
        let mut collecting = false;
        for l in lines.iter().skip(i + 2) {
            if is_report_line(l) || l.starts_with("&&") || l.starts_with("$$") {
                break;
            }
            let t = l.trim();
            if t.is_empty() {
                if collecting {
                    break;
                }
                continue; // the blank between the detail line and remarks
            }
            if l.starts_with(' ') {
                collecting = true;
                remarks.push(t);
            } else {
                break;
            }
        }
        out.push(Lsr {
            id: format!("{product_id}#{}", out.len()),
            event: event.to_string(),
            time: time.to_string(),
            date,
            magnitude,
            city: city.to_string(),
            county,
            state,
            lat,
            lon,
            source,
            remarks: remarks.join(" "),
            office: office.to_string(),
            issued: issued.to_string(),
            text: text.to_string(),
        });
    }
    out
}

fn is_report_line(line: &str) -> bool {
    let b = line.as_bytes();
    b.len() >= 8
        && b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
        && b[4] == b' '
        && (line[5..7].eq_ignore_ascii_case("AM") || line[5..7].eq_ignore_ascii_case("PM"))
}

/// Report line: time, event, city, lat/lon — fixed columns as printed.
fn parse_report_line(line: &str) -> Option<(&str, &str, &str, Option<f64>, Option<f64>)> {
    let time = col(line, TIME_COL);
    let event = col(line, EVENT_COL);
    let city = col(line, CITY_COL);
    if event.is_empty() {
        return None;
    }
    let (lat, lon) = parse_latlon(col(line, LATLON_COL)).or_else(|| parse_latlon_tail(line))?;
    Some((time, event, city, Some(lat), Some(lon)))
}

/// Date/mag/county/state/source line: the tail after the county column is
/// split on runs of spaces, because county names never contain double spaces
/// while the fields around them are padded to the header's columns.
fn parse_detail_line(line: &str) -> (String, String, String, String, String) {
    // All five fields are fixed columns measured against real products
    // (including two-word counties like "Ketchikan Gateway", which break
    // whitespace-splitting: the state would land mid-name).
    let county = col(line, COUNTY_COL);
    let state = col(line, STATE_COL);
    let source = col(line, SOURCE_COL);
    (
        col(line, DATE_COL).into(),
        col(line, MAG_COL).into(),
        county.into(),
        state.into(),
        source.into(),
    )
}

/// `"43.84N 103.62W"` → `(43.84, -103.62)`.
fn parse_latlon(s: &str) -> Option<(f64, f64)> {
    let mut it = s.split_whitespace();
    let lat = coord(it.next()?, true)?;
    let lon = coord(it.next()?, false)?;
    Some((lat, lon))
}

/// Fallback when the lat/lon column did not parse: the last two tokens of
/// the line are almost always the coordinates.
fn parse_latlon_tail(line: &str) -> Option<(f64, f64)> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.len() < 2 {
        return None;
    }
    let lon = coord(tokens[tokens.len() - 1], false)?;
    let lat = coord(tokens[tokens.len() - 2], true)?;
    Some((lat, lon))
}

/// `"43.84N"` / `"103.62W"` → signed degrees (south/west negative).
fn coord(tok: &str, is_lat: bool) -> Option<f64> {
    let hemi = tok.chars().last()?;
    if !hemi.is_ascii_alphabetic() {
        return None;
    }
    let val: f64 = tok[..tok.len() - 1].parse().ok()?;
    let bound = if is_lat { 90.0 } else { 180.0 };
    if val > bound {
        return None;
    }
    Some(match hemi.to_ascii_uppercase() {
        'S' | 'W' => -val,
        'N' | 'E' => val,
        _ => return None,
    })
}

/// Drop the WMO/AWIPS header (leading `000`, `NWUS53 KUNR ...`, `LSRUNR`,
/// blanks) so the popup shows the report itself. Same idea as the outlook
/// discussion's header strip.
pub fn strip_wmo_header(text: &str) -> &str {
    let idx = text.find("Preliminary Local Storm Report");
    match idx {
        Some(i) => &text[i..],
        None => text.trim_start_matches(|c: char| c == '\n' || c == '\r'),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KUNR: &str = include_str!("../tests/fixtures/lsr_1_kunr.txt");
    const KMPX: &str = include_str!("../tests/fixtures/lsr_2_kmpx.txt");
    const PAJK: &str = include_str!("../tests/fixtures/lsr_flood_pajk.txt");

    #[test]
    fn parses_a_wind_gust_report_from_fixed_columns() {
        let rs = parse_product(KUNR, "p1", "KUNR", "2026-10-16T02:57:00+00:00");
        assert_eq!(rs.len(), 1);
        let r = &rs[0];
        assert_eq!(r.time, "0744 PM");
        assert_eq!(r.date, "10/15/2026");
        assert_eq!(r.event, "Non-Tstm Wnd Gst");
        assert_eq!(r.city, "5 N Custer");
        assert_eq!(r.magnitude, "M51 mph");
        assert_eq!(r.county, "Custer");
        assert_eq!(r.state, "SD");
        assert_eq!(r.source, "Public");
        assert_eq!(r.lat, Some(43.84));
        assert_eq!(r.lon, Some(-103.62));
        assert_eq!(r.office, "KUNR");
        assert!(!r.is_spotter(), "public report is not a spotter one");
        assert_eq!(r.kind(), ReportKind::Wind);
    }

    #[test]
    fn parses_measured_rain_totals_and_trained_spotter() {
        let rs = parse_product(KMPX, "p2", "KMPX", "2026-10-16T02:40:00+00:00");
        assert_eq!(rs.len(), 1);
        let r = &rs[0];
        assert_eq!(r.event, "Rain");
        assert_eq!(r.magnitude, "M1.43 Inch");
        assert_eq!(r.city, "2 NW Lakeville");
        assert_eq!(r.county, "Dakota");
        assert_eq!(r.state, "MN");
        assert_eq!(r.source, "Trained Spotter");
        assert!(r.is_spotter(), "trained spotter must be recognized");
        assert_eq!(r.kind(), ReportKind::Rain);
        assert_eq!(r.lat, Some(44.70));
        assert_eq!(r.lon, Some(-93.28));
        assert!(
            r.remarks.contains("Rain total as of 9:30pm."),
            "{}",
            r.remarks
        );
    }

    #[test]
    fn parses_multi_line_remarks_and_tz_carried_in_text() {
        let rs = parse_product(PAJK, "p3", "PAJK", "2026-10-16T02:44:00+00:00");
        assert_eq!(rs.len(), 1);
        let r = &rs[0];
        assert_eq!(r.event, "Flood");
        assert_eq!(r.source, "Public");
        assert_eq!(r.county, "Ketchikan Gateway");
        assert_eq!(r.state, "AK");
        assert_eq!(r.lat, Some(55.35));
        assert_eq!(r.lon, Some(-131.67));
        assert_eq!(r.kind(), ReportKind::Flood);
        assert!(
            r.remarks.contains("Flooding over Tongass Ave")
                && r.remarks.contains("blocking drainage"),
            "{}",
            r.remarks
        );
        assert!(
            r.display_text()
                .starts_with("Preliminary Local Storm Report"),
            "header must be stripped for display"
        );
    }

    #[test]
    fn one_product_with_two_reports_yields_two() {
        // Two report blocks in a single product: each date line belongs to
        // the report above it.
        let text = format!("{KUNR}\n{}", KMPX.replace(
            "000\nNWUS53 KMPX 160240\nLSRMPX\n\nPreliminary Local Storm Report\nNational Weather Service Twin Cities/Chanhassen MN\n940 PM CDT Thu Oct 15 2026\n\n",
            "",
        ));
        let rs = parse_product(&text, "multi", "KUNR", "2026-10-16T02:57:00+00:00");
        assert_eq!(rs.len(), 2, "both report blocks parse");
        assert_eq!(rs[0].event, "Non-Tstm Wnd Gst");
        assert_eq!(rs[1].event, "Rain");
        assert_ne!(rs[0].id, rs[1].id, "ids are unique within a product");
    }

    #[test]
    fn event_kinds_cover_the_abbreviations() {
        use ReportKind::*;
        assert_eq!(ReportKind::from_event("Tornado"), Tornado);
        assert_eq!(ReportKind::from_event("Severe Thunderstorm Wind"), Wind);
        assert_eq!(ReportKind::from_event("Non-Tstm Wnd Gst"), Wind);
        assert_eq!(ReportKind::from_event("Hail"), Hail);
        assert_eq!(ReportKind::from_event("Flash Flood"), Flood);
        assert_eq!(ReportKind::from_event("Rain"), Rain);
        assert_eq!(ReportKind::from_event("Snow Accumulation"), Snow);
        assert_eq!(ReportKind::from_event("Funnel Cloud"), Other);
        assert_eq!(tornado_label(), "tornado");
    }

    fn tornado_label() -> &'static str {
        ReportKind::Tornado.label()
    }

    #[test]
    fn column_slicing_never_panics_on_short_lines() {
        assert_eq!(col("", TIME_COL), "");
        assert_eq!(col("0500 PM", CITY_COL), "");
        // Line shorter than the column: clamped to what exists ("l" at col 12).
        assert_eq!(col("0500 PM  Hail", EVENT_COL), "l");
        assert!(parse_product("", "x", "KOUN", "2026-10-16T00:00:00+00:00").is_empty());
        assert!(parse_product("junk\n&&\n$$\n", "x", "KOUN", "now").is_empty());
    }

    #[test]
    fn latlon_parsing_rejects_impossible_values() {
        assert_eq!(parse_latlon("43.84N 103.62W"), Some((43.84, -103.62)));
        assert_eq!(parse_latlon("33.45S 151.20E"), Some((-33.45, 151.20)));
        assert_eq!(parse_latlon("91.00N 10.00W"), None, "lat over 90");
        assert_eq!(parse_latlon("43.84X 103.62W"), None, "bad hemisphere");
        assert_eq!(parse_latlon(""), None);
        // Tail fallback finds coordinates at the end of a malformed line.
        assert_eq!(
            parse_latlon_tail("... and then 35.12N 097.45W"),
            Some((35.12, -97.45))
        );
    }

    #[test]
    fn summary_and_issued_helpers_work() {
        let mut rs = parse_product(KMPX, "p2", "KMPX", "2026-10-16T02:40:00+00:00");
        let r = &mut rs[0];
        assert_eq!(r.summary(), "rain — 2 NW Lakeville, MN");
        assert!(r.issued_at().is_some(), "RFC3339 issuance parses");
        r.source = "Broadcast Media".into();
        assert!(!r.is_spotter());
    }
}
