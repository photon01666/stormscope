//! WPC Winter Storm Severity Index (WSSI) model.
//!
//! WSSI is the NWS Weather Prediction Center's impact-based winter outlook:
//! five components (overall, snow amount, snow load, ice accumulation, blowing
//! snow) for Day 1, Day 2, Day 3 and Days 1–3. It is guidance, not a warning.
//!
//! The GeoJSON comes from WPC's public ArcGIS service; this module only holds
//! the decoded types shared by the fetcher and the map.

/// One WSSI component (the "product" in WPC's GIS service).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WssiComponent {
    /// Maximum impact from any component (the default summary).
    Overall,
    SnowAmount,
    SnowLoad,
    IceAccumulation,
    BlowingSnow,
}

impl WssiComponent {
    pub const ALL: [Self; 5] = [
        Self::Overall,
        Self::SnowAmount,
        Self::SnowLoad,
        Self::IceAccumulation,
        Self::BlowingSnow,
    ];

    /// Components in draw order: hazards first, the combined maximum last so
    /// its strongest areas sit on top when every component is shown.
    pub const DRAW_ORDER: [Self; 5] = [
        Self::SnowAmount,
        Self::SnowLoad,
        Self::IceAccumulation,
        Self::BlowingSnow,
        Self::Overall,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Overall => "Overall",
            Self::SnowAmount => "Snow amount",
            Self::SnowLoad => "Snow load",
            Self::IceAccumulation => "Ice accumulation",
            Self::BlowingSnow => "Blowing snow",
        }
    }

    /// First ArcGIS layer id for this component (Day 1). The next three ids
    /// are Day 2, Day 3 and Days 1–3.
    pub fn layer_base(self) -> u32 {
        match self {
            Self::Overall => 1,
            Self::SnowAmount => 6,
            Self::SnowLoad => 11,
            Self::IceAccumulation => 16,
            Self::BlowingSnow => 24,
        }
    }

    pub fn layer_id(self, period: WssiPeriod) -> u32 {
        self.layer_base() + period.offset()
    }

    /// What this WSSI component measures, in WPC's wording.
    pub fn description(self) -> &'static str {
        match self {
            Self::Overall => {
                "Maximum impact from any of the WSSI components: the combined \
                 winter-weather impact for the period."
            }
            Self::SnowAmount => {
                "Potential impacts from the total forecast snowfall. Snow \
                 climatology, population and the road network are factored in, \
                 so the same inches can mean different impacts in different \
                 places."
            }
            Self::SnowLoad => {
                "Potential impacts from the weight of forecast snow on trees, \
                 structures, utility lines and anyone clearing it."
            }
            Self::IceAccumulation => {
                "Potential impacts from ice accretion on roads, trees and \
                 elevated surfaces, with wind as an enhancing factor."
            }
            Self::BlowingSnow => {
                "Potential impacts from visibility reductions during active \
                 snowfall, combined with wind and land cover."
            }
        }
    }
}

/// WSSI forecast period.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WssiPeriod {
    Day1,
    Day2,
    Day3,
    Days1to3,
}

impl WssiPeriod {
    pub const ALL: [Self; 4] = [Self::Days1to3, Self::Day1, Self::Day2, Self::Day3];

    pub fn label(self) -> &'static str {
        match self {
            Self::Day1 => "Day 1",
            Self::Day2 => "Day 2",
            Self::Day3 => "Day 3",
            Self::Days1to3 => "Days 1-3",
        }
    }

    pub fn offset(self) -> u32 {
        match self {
            Self::Day1 => 0,
            Self::Day2 => 1,
            Self::Day3 => 2,
            Self::Days1to3 => 3,
        }
    }
}

/// WSSI impact category, least to most severe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WssiImpact {
    WinterWeatherArea,
    Minor,
    Moderate,
    Major,
    Extreme,
}

impl WssiImpact {
    pub fn from_gis(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "WINTER WEATHER AREA" => Some(Self::WinterWeatherArea),
            "MINOR" => Some(Self::Minor),
            "MODERATE" => Some(Self::Moderate),
            "MAJOR" => Some(Self::Major),
            "EXTREME" => Some(Self::Extreme),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::WinterWeatherArea => "Winter weather area",
            Self::Minor => "Minor impacts",
            Self::Moderate => "Moderate impacts",
            Self::Major => "Major impacts",
            Self::Extreme => "Extreme impacts",
        }
    }

    /// Fill color from WPC's own GIS renderer.
    pub fn color(self) -> [u8; 3] {
        match self {
            Self::WinterWeatherArea => [210, 223, 231],
            Self::Minor => [250, 245, 163],
            Self::Moderate => [247, 150, 47],
            Self::Major => [230, 31, 38],
            Self::Extreme => [120, 83, 161],
        }
    }

    pub fn rank(self) -> u8 {
        match self {
            Self::WinterWeatherArea => 0,
            Self::Minor => 1,
            Self::Moderate => 2,
            Self::Major => 3,
            Self::Extreme => 4,
        }
    }

    /// WPC's plain-language impact definition.
    pub fn description(self) -> &'static str {
        match self {
            Self::WinterWeatherArea => {
                "Winter weather is expected. Winter driving conditions may \
                 occur; drive carefully."
            }
            Self::Minor => {
                "A few inconveniences to daily life. Winter driving conditions \
                 are possible; use caution while driving."
            }
            Self::Moderate => {
                "Disruptions to daily life. Hazardous driving conditions; use \
                 extra caution. Closures and infrastructure disruptions may \
                 occur."
            }
            Self::Major => {
                "Considerable disruptions to daily life. Dangerous or \
                 impossible driving conditions; avoid travel if possible. \
                 Widespread closures and infrastructure disruptions may occur."
            }
            Self::Extreme => {
                "Substantial disruptions to daily life. Extremely dangerous or \
                 impossible driving conditions; travel is not advised. \
                 Extensive closures and infrastructure disruptions may occur."
            }
        }
    }
}

/// One WSSI impact area with its polygons (rings include holes when present).
#[derive(Clone, Debug)]
pub struct WssiFeature {
    pub impact: WssiImpact,
    pub polygons: Vec<Vec<Vec<(f64, f64)>>>,
}

/// One fetched WSSI component/period layer.
#[derive(Clone, Debug)]
pub struct WssiLayer {
    pub component: WssiComponent,
    pub period: WssiPeriod,
    pub issue: String,
    pub valid: String,
    /// Least severe first, so stronger impacts draw on top.
    pub features: Vec<WssiFeature>,
}

impl WssiLayer {
    pub fn sorted_features(&self) -> Vec<&WssiFeature> {
        let mut out: Vec<&WssiFeature> = self.features.iter().collect();
        out.sort_by_key(|f| f.impact.rank());
        out
    }
}
