//! Winter hydrometeors: the classification vocabulary shared by the derived
//! product, the color table and the probe readouts.
//!
//! The classifier itself lives with the other derived products
//! ([`crate::derived::hydrometeor_sweep`]); this module owns only the classes
//! and their display names, so a readout can say "wet snow" instead of "4".

/// A simplified, winter-focused hydrometeor classification.
///
/// The operational HCA has a dozen or so fuzzy-logic classes for every
/// season; this one is a documented decision tree over the dual-pol inputs
/// (Z, ZDR, CC and KDP) tuned for the question winter radar work actually
/// asks: *is that snow, rain, or the melting layer?* The encoded value in a
/// sweep is the class index (1..=8), so DN 0 stays "no data".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HydrometeorClass {
    Rain = 1,
    HeavyRain = 2,
    DrySnow = 3,
    WetSnow = 4,
    Mixed = 5,
    IceCrystals = 6,
    GraupelHail = 7,
    Clutter = 8,
}

impl HydrometeorClass {
    pub const ALL: [HydrometeorClass; 8] = [
        HydrometeorClass::Rain,
        HydrometeorClass::HeavyRain,
        HydrometeorClass::DrySnow,
        HydrometeorClass::WetSnow,
        HydrometeorClass::Mixed,
        HydrometeorClass::IceCrystals,
        HydrometeorClass::GraupelHail,
        HydrometeorClass::Clutter,
    ];

    /// The encoded value in a sweep (`DN 0` = no data).
    pub fn value(self) -> f32 {
        self as u8 as f32
    }

    pub fn from_value(value: f32) -> Option<Self> {
        if !value.is_finite() {
            return None;
        }
        let index = value.round() as i32;
        Self::ALL
            .iter()
            .copied()
            .find(|class| class.value() as i32 == index)
    }

    pub fn label(self) -> &'static str {
        match self {
            HydrometeorClass::Rain => "rain",
            HydrometeorClass::HeavyRain => "heavy rain",
            HydrometeorClass::DrySnow => "dry snow",
            HydrometeorClass::WetSnow => "wet snow",
            HydrometeorClass::Mixed => "mixed",
            HydrometeorClass::IceCrystals => "ice crystals",
            HydrometeorClass::GraupelHail => "graupel/hail",
            HydrometeorClass::Clutter => "clutter/debris",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_round_trip_through_their_encoded_values() {
        for class in HydrometeorClass::ALL {
            assert_eq!(HydrometeorClass::from_value(class.value()), Some(class));
            assert!(!class.label().is_empty());
            assert!(class.value() >= 1.0 && class.value() <= 8.0);
        }
        // Junk and no-data never become a class.
        assert_eq!(HydrometeorClass::from_value(0.0), None);
        assert_eq!(HydrometeorClass::from_value(9.0), None);
        assert_eq!(HydrometeorClass::from_value(f32::NAN), None);
    }
}
