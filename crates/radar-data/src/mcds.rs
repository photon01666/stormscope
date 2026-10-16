//! Mesoscale Discussions (MCD) — SPC's short-fused "what is organizing now"
//! write-ups, issued between outlooks and warnings (typically highlighting an
//! area under threat and the chance a watch gets issued within the hour).
//!
//! Sources (SPC hosts these itself; api.weather.gov does not carry MCDs):
//! * List: `https://www.spc.noaa.gov/products/spcmdrss.xml` — RSS with one
//!   item per active discussion (and a single "No MDs are in effect" item
//!   when nothing is running, which the parser drops because its link has no
//!   discussion number).
//! * Text: `https://www.spc.noaa.gov/products/md/mdNNNN.html` — the full
//!   product inside one `<pre>` block, including the discussion area polygon
//!   as a `LAT...LON` line of packed pairs: latitude as `DDdd` (degrees and
//!   hundredths), then longitude as `DDdd`/`DDDdd` — west, so negated. Validated
//!   against MD 2380: the decoded ring (38.2–39.5 N, −85.4…−88.2) sits exactly
//!   across the IL/IN/KY/OH WFOs listed in the product's `ATTN` line.

use anyhow::{Context, Result, bail};
use std::collections::HashMap;

const FEED_URL: &str = "https://www.spc.noaa.gov/products/spcmdrss.xml";

#[derive(Debug, Clone, PartialEq)]
pub struct Mcd {
    pub number: u32,
    pub page: String,
    /// "far southeastern Illinois into southern Indiana"
    pub areas: String,
    /// "Severe potential...Watch possible" (the `Concerning` line)
    pub concerning: String,
    /// "152308Z - 160015Z"
    pub valid: String,
    /// "40 percent"
    pub watch_prob: String,
    pub forecaster: String,
    /// WFOs flagged in the product's `ATTN` line.
    pub offices: Vec<String>,
    /// Discussion area as one ring, `(lon, lat)` — the same order the
    /// warning polygons use, so the map draws it the same way.
    pub polygon: Vec<Vec<(f64, f64)>>,
    /// Full product text (what the detail popup shows).
    pub text: String,
    /// RSS `pubDate` (RFC 2822) — the age of the discussion.
    pub issued: String,
}

impl Mcd {
    /// First line of the area: what region this is about.
    pub fn headline(&self) -> String {
        if self.areas.is_empty() {
            format!("Mesoscale Discussion {}", self.number)
        } else {
            self.areas.clone()
        }
    }

    pub fn issued_at(&self) -> Option<chrono::DateTime<chrono::FixedOffset>> {
        chrono::DateTime::parse_from_rfc2822(&self.issued).ok()
    }
}

pub struct McdSource {
    client: reqwest::blocking::Client,
    /// Page HTML by URL: fetched once, reused across polls.
    pages: HashMap<String, String>,
}

impl McdSource {
    pub fn new(user_agent: &str) -> Result<Self> {
        let ua = user_agent.trim();
        if ua.len() < 8 {
            bail!("SPC requests need a descriptive User-Agent; got {user_agent:?}");
        }
        let client = reqwest::blocking::Client::builder()
            .user_agent(ua)
            .timeout(std::time::Duration::from_secs(20))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(Self {
            client,
            pages: HashMap::new(),
        })
    }

    /// Every discussion the feed lists, parsed. Pages are cached by URL, so a
    /// poll costs the feed plus whatever discussion appeared since last time.
    pub fn active(&mut self) -> Result<Vec<Mcd>> {
        let feed = self
            .client
            .get(FEED_URL)
            .send()
            .with_context(|| format!("GET {FEED_URL}"))?
            .error_for_status()?
            .text()?;
        let refs = parse_feed(&feed);
        let mut out = Vec::new();
        for r in refs {
            if !self.pages.contains_key(&r.page) {
                match self.client.get(&r.page).send() {
                    Ok(resp) if resp.status().is_success() => {
                        if let Ok(html) = resp.text() {
                            self.pages.insert(r.page.clone(), html);
                        }
                    }
                    Ok(resp) => log::debug!("MCD {} HTTP {}", r.number, resp.status()),
                    Err(e) => log::debug!("MCD {} {e:#}", r.number),
                }
            }
            if let Some(html) = self.pages.get(&r.page) {
                if let Some(m) = parse_mcd_page(html, &r.page, &r.issued) {
                    out.push(m);
                }
            }
        }
        out.sort_by(|a, b| b.number.cmp(&a.number));
        Ok(out)
    }
}

/// One item from the MCD RSS feed.
#[derive(Debug, Clone, PartialEq)]
pub struct McdRef {
    pub number: u32,
    pub page: String,
    pub issued: String,
}

/// Parse the RSS: each `<item>`'s link carries the discussion number
/// (`.../md/md2380.html`); the "no MDs in effect" sentinel has a bare
/// directory link and is dropped by the number extraction.
pub fn parse_feed(xml: &str) -> Vec<McdRef> {
    let mut out = Vec::new();
    for chunk in xml.split("<item>").skip(1) {
        let item = chunk.split("</item>").next().unwrap_or(chunk);
        let Some(link) = between(item, "<link>", "</link>") else {
            continue;
        };
        let Some(tail) = link.rsplit("/md/md").next() else {
            continue;
        };
        let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
        let Ok(number) = digits.parse::<u32>() else {
            continue; // sentinel or non-discussion link
        };
        let issued = between(item, "<pubDate>", "</pubDate>")
            .unwrap_or_default()
            .to_string();
        out.push(McdRef {
            number,
            page: link.trim().to_string(),
            issued,
        });
    }
    out
}

/// Extract the single `<pre>…</pre>` product block from an MCD page,
/// unescaping the few entities NWS pages use.
pub fn extract_pre(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<pre")?;
    let open = lower[start..].find('>').map(|i| start + i)? + 1;
    let end = lower[open..].find("</pre>").map(|i| open + i)?;
    Some(unescape(&html[open..end]))
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let (token, len) = if tail.starts_with("&gt;") {
            (">", 4)
        } else if tail.starts_with("&lt;") {
            ("<", 4)
        } else if tail.starts_with("&amp;") {
            ("&", 5)
        } else if tail.starts_with("&quot;") {
            ("\"", 6)
        } else if tail.starts_with("&#39;") {
            ("'", 5)
        } else if tail.starts_with("&#8217;") || tail.starts_with("&#8220;") {
            ("'", 7)
        } else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        out.push_str(token);
        rest = &tail[len..];
    }
    out.push_str(rest);
    out
}

/// Pull one field out of the product text: a line starting with `label`,
/// returning whatever follows its first `...` separator (or the whole rest).
fn field(text: &str, label: &str) -> Option<String> {
    for line in text.lines() {
        let l = line.trim();
        if l.len() >= label.len() && l[..label.len()].eq_ignore_ascii_case(label) {
            let rest = &l[label.len()..];
            let value = match rest.find("...") {
                Some(i) => &rest[i + 3..],
                None => rest,
            };
            let value = value.trim().trim_start_matches('.').trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Parse a full MCD page into its discussion. `None` when the page has no
/// discussion number (unexpected HTML), so bad fetches do not surface.
pub fn parse_mcd_page(html: &str, page: &str, issued: &str) -> Option<Mcd> {
    let text = extract_pre(html)?;
    let mut number = None;
    let mut forecaster = String::new();
    let mut offices = Vec::new();
    let mut polygon: Vec<(f64, f64)> = Vec::new();
    let mut lines = text.lines().map(str::trim);
    let mut in_latlon = false;
    for l in &mut lines {
        if number.is_none() {
            if let Some(rest) = l.strip_prefix("Mesoscale Discussion ") {
                number = rest.trim().parse::<u32>().ok();
            }
        }
        if forecaster.is_empty() {
            // `..Lyons/Smith.. 10/15/2026`
            if l.starts_with("..") && l.ends_with(|c: char| c.is_ascii_digit()) && l.len() > 12 {
                let inner = l.trim_matches('.');
                if let Some((names, _date)) = inner.rsplit_once(' ') {
                    if names.contains('/') || names.chars().all(|c| c.is_ascii_alphabetic()) {
                        forecaster = names.trim().trim_matches('.').to_string();
                    }
                }
            }
        }
        if l.starts_with("ATTN...WFO") {
            // Offices follow the `WFO` label — which is itself three
            // uppercase letters, so collect only *after* seeing it.
            let mut past_label = false;
            offices = l
                .split("...")
                .filter_map(|t| {
                    let t = t.trim();
                    if !past_label {
                        if t == "WFO" {
                            past_label = true;
                        }
                        return None;
                    }
                    (t.len() == 3 && t.chars().all(|c| c.is_ascii_uppercase()))
                        .then(|| t.to_string())
                })
                .collect();
        }
        if l.contains("LAT...LON") {
            in_latlon = true;
            polygon.extend(decode_packed_points(l.split_whitespace()));
        } else if in_latlon {
            // Continuation lines are the bare packed pairs.
            if l.chars()
                .all(|c| c.is_ascii_whitespace() || c.is_ascii_digit())
                && !l.is_empty()
            {
                polygon.extend(decode_packed_points(l.split_whitespace()));
            } else {
                in_latlon = false;
            }
        }
    }
    let number = number?;
    let ring = if polygon.len() >= 3 {
        vec![polygon]
    } else {
        Vec::new()
    };
    Some(Mcd {
        number,
        page: page.to_string(),
        areas: field(&text, "Areas affected").unwrap_or_default(),
        concerning: field(&text, "Concerning").unwrap_or_default(),
        valid: field(&text, "Valid").unwrap_or_default(),
        watch_prob: field(&text, "Probability of Watch Issuance").unwrap_or_default(),
        forecaster,
        offices,
        polygon: ring,
        text,
        issued: issued.to_string(),
    })
}

/// Decode the packed `LAT...LON` tokens: latitude `DDdd`, longitude
/// `DDdd`/`DDDdd` (hundredths of a degree, west so negated). Malformed
/// tokens are skipped rather than poisoning the ring.
pub fn decode_packed_points<'a>(tokens: impl Iterator<Item = &'a str>) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for t in tokens {
        let t = t.trim();
        if t.len() < 8 || !t.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let lat = match parse_hundredths(&t[0..4]) {
            Some(v) if v <= 90.0 => v,
            _ => continue,
        };
        let lon_part = &t[4..];
        let (deg, hh) = lon_part.split_at(lon_part.len() - 2);
        let (Some(d), Some(h)) = (deg.parse::<f64>().ok(), hh.parse::<f64>().ok()) else {
            continue;
        };
        let lon = d + h / 100.0;
        if lon > 180.0 {
            continue;
        }
        out.push((-lon, lat)); // SPC products are west longitude
    }
    out
}

fn parse_hundredths(s: &str) -> Option<f64> {
    let (deg, hh) = s.split_at(2);
    Some(deg.parse::<f64>().ok()? + hh.parse::<f64>().ok()? / 100.0)
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let end = s[start..].find(close)? + start;
    Some(s[start..end].trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MCD_2380: &str = include_str!("../tests/fixtures/mcd_2380.html");

    #[test]
    fn parses_a_real_mcd_page() {
        let m = parse_mcd_page(
            MCD_2380,
            "https://www.spc.noaa.gov/products/md/md2380.html",
            "Fri, 16 Oct 2026 00:15:05 +0000",
        )
        .expect("page parses");
        assert_eq!(m.number, 2380);
        assert_eq!(
            m.areas, "far southeastern Illinois into southern Indiana",
            "areas affected"
        );
        assert_eq!(m.valid, "152308Z - 160015Z");
        assert_eq!(m.watch_prob, "40 percent");
        assert_eq!(m.forecaster, "Lyons/Smith");
        assert_eq!(m.offices, vec!["ILN", "LMK", "IND", "PAH", "ILX"]);
        assert!(
            m.concerning
                .to_ascii_lowercase()
                .contains("severe potential")
        );
        // Polygon: 12 packed points, ring order (lon, lat), west negative.
        assert_eq!(m.polygon.len(), 1, "one ring");
        let ring = &m.polygon[0];
        assert_eq!(ring.len(), 12, "points including the closing repeat");
        assert_eq!(ring[0], (-88.22, 38.52));
        assert!(
            ring.iter().any(|p| *p == (-85.40, 38.93)),
            "westernmost point decoded: {:?}",
            &ring[4..7]
        );
        assert!(ring.iter().all(|(lo, la)| *lo < -85.0 && *la > 38.0));
        assert!(m.text.contains("Mesoscale Discussion 2380"));
        assert!(m.issued_at().is_some(), "RSS pubDate parses");
        assert_eq!(
            m.headline(),
            "far southeastern Illinois into southern Indiana"
        );
    }

    #[test]
    fn packed_points_decode_and_reject_garbage() {
        assert_eq!(
            decode_packed_points(["38528822"].into_iter()),
            vec![(-88.22, 38.52)]
        );
        // Three-digit longitudes (west of 100): 4-digit lat + 5-digit lon.
        assert_eq!(
            decode_packed_points(["342710415"].into_iter()),
            vec![(-104.15, 34.27)]
        );
        assert_eq!(
            decode_packed_points(["38528822", "99", "abc", "99991234"].into_iter()),
            vec![(-88.22, 38.52)],
            "short/non-numeric/out-of-range tokens skipped"
        );
        // The boundary token itself: latitude over 90 is dropped.
        assert_eq!(
            decode_packed_points(["91000800"].into_iter()),
            Vec::<(f64, f64)>::new()
        );
    }

    #[test]
    fn feed_parses_items_and_ignores_the_sentinel() {
        let xml = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
<item><title>SPC - Mesoscale Discussion 2381</title>
<link>https://www.spc.noaa.gov/products/md/md2381.html</link>
<pubDate>Fri, 16 Oct 2026 00:15:05 +0000</pubDate></item>
<item><title>SPC - No MDs are in effect as of Fri Oct 16 00:16:02 UTC 2026</title>
<link>https://www.spc.noaa.gov/products/md/</link>
<pubDate>Fri, 16 Oct 2026 00:15:05 +0000</pubDate></item>
</channel></rss>"#;
        let refs = parse_feed(xml);
        assert_eq!(refs.len(), 1, "sentinel dropped");
        assert_eq!(refs[0].number, 2381);
        assert!(refs[0].page.ends_with("md2381.html"));
        assert_eq!(refs[0].issued, "Fri, 16 Oct 2026 00:15:05 +0000");
        assert!(parse_feed("<channel></channel>").is_empty());
    }

    #[test]
    fn pre_extraction_and_entity_unescaping() {
        assert_eq!(
            extract_pre("<html><pre>a &amp; b &gt; c</pre>tail"),
            Some("a & b > c".into())
        );
        assert_eq!(extract_pre("<PRE>  up  </PRE>"), Some("  up  ".into()));
        assert_eq!(extract_pre("no pre block here"), None);
    }

    #[test]
    fn fields_fall_back_cleanly_on_unexpected_html() {
        assert!(parse_mcd_page("<html>nothing</html>", "x", "now").is_none());
        let partial = "<pre>Mesoscale Discussion 42</pre>";
        let m = parse_mcd_page(partial, "x", "now").expect("number-only page still parses");
        assert_eq!(m.number, 42);
        assert!(m.polygon.is_empty(), "no LAT...LON = no polygon");
        assert!(m.areas.is_empty());
    }
}
