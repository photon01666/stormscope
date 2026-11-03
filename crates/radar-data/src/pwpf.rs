//! WPC Probabilistic Winter Precipitation Forecasts (PWPF) fetch/parse.
//!
//! Each product/day is a KMZ zip containing one KML file. Snow products are
//! vector polygons; ice products are raster GroundOverlays. The files are
//! public and small (~20–60 kB), so a single request per selection is cheap.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use anyhow::{Context, Result, bail};
use radar_core::pwpf::{PwpfDay, PwpfFeature, PwpfGeometry, PwpfLayer, PwpfProduct, PwpfRaster};

const BASE: &str = "https://www.wpc.ncep.noaa.gov/pwpf/latest_kml_GE";
const WPC_ROOT: &str = "https://www.wpc.ncep.noaa.gov";

#[derive(Clone)]
pub struct PwpfSource {
    client: reqwest::blocking::Client,
}

impl PwpfSource {
    pub fn new(user_agent: &str) -> Result<Self> {
        let ua = user_agent.trim();
        if ua.len() < 8 {
            bail!("WPC requests need a descriptive User-Agent; got {user_agent:?}");
        }
        let client = reqwest::blocking::Client::builder()
            .user_agent(ua)
            .timeout(std::time::Duration::from_secs(25))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(Self { client })
    }

    pub fn fetch(&self, product: PwpfProduct, day: PwpfDay) -> Result<PwpfLayer> {
        let url = format!(
            "{BASE}/{}_f{:03}_latest_GE.kmz",
            product.prefix(),
            day.forecast_hour()
        );
        let resp = self
            .client
            .get(&url)
            .send()
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            bail!("WPC PWPF returned HTTP {status}");
        }
        let bytes = resp.bytes().context("reading WPC PWPF KMZ")?;
        let kml = kmz_kml(&bytes)?;
        let (issue, valid) = metadata(&kml);

        let styles = parse_styles(&kml);
        let features = parse_features(&kml, &styles);
        if !features.is_empty() {
            return Ok(PwpfLayer {
                product,
                day,
                issue,
                valid,
                geometry: PwpfGeometry::Vector(features),
            });
        }

        let Some((href, bounds)) = ground_overlay(&kml) else {
            bail!("WPC PWPF has neither polygons nor a usable overlay");
        };
        let png_url = if href.starts_with("http") {
            href
        } else if href.starts_with('/') {
            format!("{WPC_ROOT}{href}")
        } else {
            format!("{BASE}/{href}")
        };
        let png = self
            .client
            .get(&png_url)
            .send()
            .with_context(|| format!("GET {png_url}"))?
            .error_for_status()
            .with_context(|| format!("GET {png_url}"))?
            .bytes()
            .context("reading WPC PWPF overlay")?;
        let image = image::load_from_memory(&png)
            .context("decoding WPC PWPF overlay")?
            .to_rgba8();
        Ok(PwpfLayer {
            product,
            day,
            issue,
            valid,
            geometry: PwpfGeometry::Raster(PwpfRaster {
                width: image.width() as usize,
                height: image.height() as usize,
                rgba: image.into_raw(),
                bounds,
            }),
        })
    }
}

fn kmz_kml(bytes: &[u8]) -> Result<String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).context("opening WPC PWPF KMZ")?;
    let name = archive
        .file_names()
        .find(|name| name.ends_with(".kml"))
        .map(str::to_string)
        .context("WPC PWPF KMZ has no KML file")?;
    let mut file = archive.by_name(&name)?;
    let mut kml = String::new();
    file.read_to_string(&mut kml)?;
    Ok(kml)
}

fn metadata(kml: &str) -> (String, String) {
    let valid = tag_text(kml, "snippet")
        .unwrap_or_default()
        .trim()
        .trim_start_matches("Valid ")
        .to_string();
    let issue = kml
        .find("Issued:")
        .map(|start| {
            let after = &kml[start + "Issued:".len()..];
            let end = after
                .find(|c: char| c == '<' || c == '\n')
                .unwrap_or(after.len());
            after[..end].trim().to_string()
        })
        .unwrap_or_default();
    (issue, valid)
}

fn parse_styles(kml: &str) -> HashMap<String, [u8; 3]> {
    let mut styles = HashMap::new();
    let mut rest = kml;
    while let Some(start) = rest.find("<Style id=\"") {
        let after = &rest[start + "<Style id=\"".len()..];
        let Some(quote) = after.find('"') else {
            break;
        };
        let id = &after[..quote];
        let block_end = after.find("</Style>").unwrap_or(after.len());
        let block = &after[..block_end];
        if let Some(poly) = block.find("<PolyStyle>") {
            let poly = &block[poly..];
            if let Some(hex) = tag_text(poly, "color")
                && let Some(rgb) = kml_rgb(&hex)
            {
                styles.insert(id.to_string(), rgb);
            }
        }
        rest = &after[block_end..];
    }
    styles
}

fn parse_features(kml: &str, styles: &HashMap<String, [u8; 3]>) -> Vec<PwpfFeature> {
    let mut features = Vec::new();
    let mut rest = kml;
    while let Some(start) = rest.find("<Placemark>") {
        let after = &rest[start + "<Placemark>".len()..];
        let end = after.find("</Placemark>").unwrap_or(after.len());
        let block = &after[..end];
        let label = tag_text(block, "name").unwrap_or_default();
        let style = tag_text(block, "styleUrl")
            .unwrap_or_default()
            .trim()
            .trim_start_matches('#')
            .to_string();
        let color = styles.get(&style).copied().unwrap_or([205, 205, 205]);
        let polygons = placemark_polygons(block);
        if !polygons.is_empty() {
            features.push(PwpfFeature {
                label,
                color,
                polygons,
            });
        }
        rest = &after[end..];
    }
    features
}

fn placemark_polygons(block: &str) -> Vec<Vec<Vec<(f64, f64)>>> {
    let mut out = Vec::new();
    let mut rest = block;
    while let Some(start) = rest.find("<Polygon>") {
        let after = &rest[start + "<Polygon>".len()..];
        let end = after.find("</Polygon>").unwrap_or(after.len());
        let polygon = &after[..end];
        if let Some(outer) = polygon.find("<outerBoundaryIs>") {
            let outer = &polygon[outer..];
            if let Some(coords) = tag_text(outer, "coordinates") {
                let points = parse_coordinates(&coords);
                if points.len() >= 3 {
                    out.push(vec![points]);
                }
            }
        }
        rest = &after[end..];
    }
    out
}

fn parse_coordinates(text: &str) -> Vec<(f64, f64)> {
    text.split_whitespace()
        .filter_map(|pos| {
            let mut parts = pos.split(',');
            let lon = parts.next()?.parse::<f64>().ok()?;
            let lat = parts.next()?.parse::<f64>().ok()?;
            Some((lon, lat))
        })
        .collect()
}

fn ground_overlay(kml: &str) -> Option<(String, [f64; 4])> {
    let mut rest = kml;
    while let Some(start) = rest.find("<GroundOverlay>") {
        let after = &rest[start + "<GroundOverlay>".len()..];
        let end = after.find("</GroundOverlay>").unwrap_or(after.len());
        let block = &after[..end];
        if tag_text(block, "name").is_some_and(|name| name.eq_ignore_ascii_case("Product Legend")) {
            rest = &after[end..];
            continue;
        }
        let href = tag_text(block, "href")?;
        let south = tag_text(block, "south")?.parse().ok()?;
        let west = tag_text(block, "west")?.parse().ok()?;
        let north = tag_text(block, "north")?.parse().ok()?;
        let east = tag_text(block, "east")?.parse().ok()?;
        return Some((href, [south, west, north, east]));
    }
    None
}

fn tag_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].to_string())
}

/// KML colors are `aabbggrr`; keep the RGB part.
fn kml_rgb(hex: &str) -> Option<[u8; 3]> {
    let hex = hex.trim();
    if hex.len() < 8 {
        return None;
    }
    let red = u8::from_str_radix(&hex[6..8], 16).ok()?;
    let green = u8::from_str_radix(&hex[4..6], 16).ok()?;
    let blue = u8::from_str_radix(&hex[2..4], 16).ok()?;
    Some([red, green, blue])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vector_bands_and_colors() {
        let kml = r#"<kml><Document>
            <snippet>Valid 00Z 11/03/2026 - 00Z 11/04/2026</snippet>
            <description>Issued: 0157Z TUE NOV 03, 2026</description>
            <Style id="poly_1"><PolyStyle><color>7f80e579</color></PolyStyle></Style>
            <Placemark>
              <name>1-2"</name><styleUrl>#poly_1</styleUrl>
              <MultiGeometry><Polygon><outerBoundaryIs><LinearRing><coordinates>
                -100,35,0 -99,35,0 -99,36,0 -100,36,0 -100,35,0
              </coordinates></LinearRing></outerBoundaryIs></Polygon></MultiGeometry>
            </Placemark>
        </Document></kml>"#;
        let styles = parse_styles(kml);
        let features = parse_features(kml, &styles);
        assert_eq!(features.len(), 1);
        assert_eq!(features[0].label, "1-2\"");
        assert_eq!(features[0].color, [0x79, 0xe5, 0x80]);
        assert_eq!(features[0].polygons[0][0].len(), 5);
        let (issue, valid) = metadata(kml);
        assert!(issue.starts_with("0157Z"));
        assert!(valid.starts_with("00Z"));
    }

    #[test]
    fn kml_color_is_abgr() {
        assert_eq!(kml_rgb("7f80e579"), Some([0x79, 0xe5, 0x80]));
        assert_eq!(kml_rgb("ffffffff"), Some([255, 255, 255]));
    }
}
