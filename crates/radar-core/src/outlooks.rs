//! SPC convective-outlook model (categorical risk areas).
//!
//! One [`Outlook`] is the full set of nested risk polygons for a single day
//! (Day 1/2/3). SPC's GeoJSON carries its own official stroke/fill colors, so
//! the renderer reproduces SPC's palette rather than inventing one.

use chrono::{DateTime, Utc};

/// One categorized risk area (TSTM/MRGL/SLGT/ENH/MDT/HIGH) with its
/// geometry and the exact colors SPC assigned it.
#[derive(Clone, Debug)]
pub struct OutlookFeature {
    /// SPC numeric category: 2=TSTM, 3=MRGL, 4=SLGT, 5=ENH, 6=MDT, 8=HIGH.
    pub dn: u16,
    /// Short category label, e.g. "SLGT".
    pub label: String,
    /// Human label, e.g. "Slight Risk".
    pub name: String,
    /// Outline color (RGB), taken verbatim from SPC.
    pub stroke: [u8; 3],
    /// Fill color (RGB), taken verbatim from SPC.
    pub fill: [u8; 3],
    /// Geometry: outer index = separate polygons (GeoJSON MultiPolygon),
    /// next index = rings of one polygon (`[exterior, hole, ...]`), and each
    /// ring is a list of `(lon, lat)` vertices.
    pub polygons: Vec<Vec<Vec<(f64, f64)>>>,
}

/// A day's categorical convective outlook.
#[derive(Clone, Debug)]
pub struct Outlook {
    /// Outlook day, `1..=3` (Days 4-8 are probability-only upstream).
    pub day: u8,
    pub issue: Option<DateTime<Utc>>,
    pub valid: Option<DateTime<Utc>>,
    pub expire: Option<DateTime<Utc>>,
    pub forecaster: String,
    pub features: Vec<OutlookFeature>,
}

impl OutlookFeature {
    /// Risk level on the 1..5 scale NWS/SPC use in public messaging
    /// ("Level 3 of 5"), `None` for the non-severe general-thunderstorm area.
    pub fn risk_level(&self) -> Option<u8> {
        match self.label.trim().to_uppercase().as_str() {
            "MRGL" => Some(1),
            "SLGT" => Some(2),
            "ENH" => Some(3),
            "MDT" => Some(4),
            "HIGH" => Some(5),
            // Older/newer feeds occasionally carry only DN.
            _ => match self.dn {
                3 => Some(1),
                4 => Some(2),
                5 => Some(3),
                6 => Some(4),
                8 => Some(5),
                _ => None,
            },
        }
    }

    /// What this category means, in the NWS's own wording (the phrases SPC
    /// uses on its outlook pages and that broadcasters read on air).
    pub fn description(&self) -> &'static str {
        match self.label.trim().to_uppercase().as_str() {
            "TSTM" => "General thunderstorms expected; severe weather is not anticipated.",
            "MRGL" => "Isolated severe storms possible.",
            "SLGT" => "Scattered severe storms possible.",
            "ENH" => "Numerous severe storms possible.",
            "MDT" => "Widespread severe storms likely.",
            "HIGH" => {
                "Widespread severe storms expected, including intense tornadoes, very large hail or damaging winds."
            }
            _ => match self.dn {
                2 => "General thunderstorms expected; severe weather is not anticipated.",
                3 => "Isolated severe storms possible.",
                4 => "Scattered severe storms possible.",
                5 => "Numerous severe storms possible.",
                6 => "Widespread severe storms likely.",
                8 => {
                    "Widespread severe storms expected, including intense tornadoes, very large hail or damaging winds."
                }
                _ => "Severe-storm risk area.",
            },
        }
    }

    /// "Enhanced Risk (3/5)" / "General Thunderstorms" — the label to show
    /// alongside the description.
    pub fn headline(&self) -> String {
        let name = if self.name.is_empty() {
            self.label.clone()
        } else {
            self.name.clone()
        };
        match self.risk_level() {
            Some(level) => format!("{name} ({level}/5)"),
            None => name,
        }
    }
}

impl Outlook {
    /// Features in increasing severity so higher categories paint on top of
    /// lower ones (the inner ENH/MDT/HIGH shapes stay visible).
    pub fn sorted_features(&self) -> Vec<&OutlookFeature> {
        let mut f: Vec<&OutlookFeature> = self.features.iter().collect();
        f.sort_by_key(|x| x.dn);
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feature(label: &str, dn: u16) -> OutlookFeature {
        OutlookFeature {
            dn,
            label: label.into(),
            name: format!("{label} Risk"),
            stroke: [255, 255, 255],
            fill: [0, 0, 0],
            polygons: Vec::new(),
        }
    }

    /// Every categorical label maps to its 1..5 level and the NWS wording for
    /// that level.
    #[test]
    fn risk_levels_and_descriptions_follow_the_nws_scale() {
        let cases = [
            ("MRGL", 1u8, "Isolated severe storms possible."),
            ("SLGT", 2, "Scattered severe storms possible."),
            ("ENH", 3, "Numerous severe storms possible."),
            ("MDT", 4, "Widespread severe storms likely."),
        ];
        for (label, level, wording) in cases {
            let f = feature(label, 4);
            assert_eq!(f.risk_level(), Some(level), "{label} level");
            assert_eq!(f.description(), wording, "{label} description");
            assert!(f.headline().contains(&format!("({level}/5)")));
        }
        // General thunderstorms carry no 1..5 level.
        let tstm = feature("TSTM", 2);
        assert_eq!(tstm.risk_level(), None);
        assert!(tstm.description().contains("not anticipated"));
        assert!(!tstm.headline().contains("/5"));

        // HIGH is the top of the scale and reads as such.
        let high = feature("HIGH", 8);
        assert_eq!(high.risk_level(), Some(5));
        assert!(
            high.description()
                .contains("Widespread severe storms expected")
        );

        // A feed that only carries DN still resolves.
        let by_dn = feature("", 5);
        assert_eq!(by_dn.risk_level(), Some(3));
        assert_eq!(by_dn.description(), "Numerous severe storms possible.");
        // Unknown categories degrade to the generic wording.
        let unknown = feature("XYZ", 99);
        assert_eq!(unknown.risk_level(), None);
        assert_eq!(unknown.description(), "Severe-storm risk area.");
    }
}
