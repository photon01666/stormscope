//! Moment -> RGBA color tables in the style of NWS/GR2Analyst.
//!
//! Every table is a plain list of `(value, RGBA)` stops with linear
//! interpolation between neighbours, so ramps are easy to read and tune.
//! Values below the first stop render fully transparent (no-data /
//! below-range); values above the last stop clamp to its color.

use radar_core::Moment;

/// One color-table stop: physical value + straight (non-premultiplied) RGBA.
pub type Stop = (f32, [u8; 4]);

/// Value -> RGBA ramp for one radar moment.
pub struct ColorTable {
    kind: Moment,
    stops: &'static [Stop],
}

// ---------------------------------------------------------------------------
// Reflectivity (dBZ): transparent -> gray -> green -> yellow -> orange -> red
// -> magenta -> white, spanning 5..75 dBZ.
// ---------------------------------------------------------------------------
const REFLECTIVITY_STOPS: &[Stop] = &[
    (5.0, [0, 0, 0, 0]),          // below 5 dBZ: nothing to show
    (8.0, [160, 160, 160, 255]),  // faint gray
    (12.0, [90, 200, 90, 255]),   // gray-green
    (16.0, [0, 220, 0, 255]),     // green
    (22.0, [0, 245, 0, 255]),     // bright green
    (26.0, [255, 255, 0, 255]),   // yellow
    (32.0, [255, 190, 0, 255]),   // gold
    (36.0, [255, 130, 0, 255]),   // orange
    (42.0, [255, 60, 0, 255]),    // red-orange
    (48.0, [255, 0, 0, 255]),     // red
    (56.0, [255, 0, 255, 255]),   // magenta
    (64.0, [255, 120, 255, 255]), // pink-magenta
    (70.0, [255, 255, 255, 255]), // white
    (75.0, [255, 255, 255, 255]), // clamped top
];

// ---------------------------------------------------------------------------
// Velocity (m/s), NEXRAD convention: negative = inbound (toward the radar),
// positive = outbound. GR2-style discrete steps at +/-5, 10, 15, 20, 25, 30,
// 35 m/s: weak = dark, fast = bright, extremes = white. Values below -50 are
// below-range (transparent); values above +50 clamp to white. Step edges are
// realized as stop pairs 0.01 apart so linear interpolation yields crisp
// boundaries.
// ---------------------------------------------------------------------------
const VEL_W: [u8; 4] = [255, 255, 255, 255];
const VEL_G7: [u8; 4] = [200, 255, 200, 255]; // -35..-30
const VEL_G6: [u8; 4] = [150, 255, 150, 255]; // -30..-25
const VEL_G5: [u8; 4] = [100, 255, 100, 255]; // -25..-20
const VEL_G4: [u8; 4] = [40, 255, 40, 255]; //   -20..-15
const VEL_G3: [u8; 4] = [0, 240, 0, 255]; //     -15..-10
const VEL_G2: [u8; 4] = [0, 180, 0, 255]; //     -10..-5
const VEL_G1: [u8; 4] = [0, 110, 0, 255]; //      -5..0
const VEL_R1: [u8; 4] = [110, 0, 0, 255]; //       0..5
const VEL_R2: [u8; 4] = [180, 0, 0, 255]; //       5..10
const VEL_R3: [u8; 4] = [240, 0, 0, 255]; //      10..15
const VEL_R4: [u8; 4] = [255, 40, 40, 255]; //    15..20
const VEL_R5: [u8; 4] = [255, 100, 100, 255]; //  20..25
const VEL_R6: [u8; 4] = [255, 150, 150, 255]; //  25..30
const VEL_R7: [u8; 4] = [255, 200, 200, 255]; //  30..35

const VELOCITY_STOPS: &[Stop] = &[
    (-50.0, VEL_W),
    (-35.01, VEL_W),
    (-35.0, VEL_G7),
    (-30.01, VEL_G7),
    (-30.0, VEL_G6),
    (-25.01, VEL_G6),
    (-25.0, VEL_G5),
    (-20.01, VEL_G5),
    (-20.0, VEL_G4),
    (-15.01, VEL_G4),
    (-15.0, VEL_G3),
    (-10.01, VEL_G3),
    (-10.0, VEL_G2),
    (-5.01, VEL_G2),
    (-5.0, VEL_G1),
    (-0.01, VEL_G1),
    (0.0, VEL_R1),
    (4.99, VEL_R1),
    (5.0, VEL_R2),
    (9.99, VEL_R2),
    (10.0, VEL_R3),
    (14.99, VEL_R3),
    (15.0, VEL_R4),
    (19.99, VEL_R4),
    (20.0, VEL_R5),
    (24.99, VEL_R5),
    (25.0, VEL_R6),
    (29.99, VEL_R6),
    (30.0, VEL_R7),
    (34.99, VEL_R7),
    (35.0, VEL_W),
    (50.0, VEL_W),
];

// ---------------------------------------------------------------------------
// Spectrum width (m/s): dark blue -> cyan -> yellow -> red over 0..15.
// ---------------------------------------------------------------------------
const SPECTRUM_WIDTH_STOPS: &[Stop] = &[
    (0.0, [30, 30, 70, 255]),
    (2.0, [0, 90, 255, 255]),
    (5.0, [0, 220, 220, 255]),
    (8.0, [255, 255, 0, 255]),
    (11.0, [255, 130, 0, 255]),
    (15.0, [255, 0, 0, 255]),
];

// ---------------------------------------------------------------------------
// Differential reflectivity (dB) over -2..6: blue (negative) -> near black
// (around 0) -> green -> yellow -> red.
// ---------------------------------------------------------------------------
const ZDR_STOPS: &[Stop] = &[
    (-2.0, [0, 80, 255, 255]),
    (-0.5, [0, 40, 130, 255]),
    (0.5, [30, 30, 30, 255]),
    (1.5, [0, 180, 0, 255]),
    (2.5, [255, 255, 0, 255]),
    (4.0, [255, 130, 0, 255]),
    (6.0, [255, 0, 0, 255]),
];

// ---------------------------------------------------------------------------
// Differential phase (deg) over 0..360: cyclic rainbow (wraps at the ends).
// ---------------------------------------------------------------------------
const PHI_STOPS: &[Stop] = &[
    (0.0, [255, 0, 0, 255]),
    (60.0, [255, 255, 0, 255]),
    (120.0, [0, 255, 0, 255]),
    (180.0, [0, 255, 255, 255]),
    (240.0, [0, 0, 255, 255]),
    (300.0, [255, 0, 255, 255]),
    (360.0, [255, 0, 0, 255]),
];

// ---------------------------------------------------------------------------
// Specific differential phase (deg/km) over -2..6: purple/blue for negative,
// dark near 0, green -> yellow -> red for positive.
// ---------------------------------------------------------------------------
const KDP_STOPS: &[Stop] = &[
    (-2.0, [120, 0, 255, 255]),
    (-0.5, [0, 60, 200, 255]),
    (0.2, [30, 30, 30, 255]),
    (1.0, [0, 200, 0, 255]),
    (2.5, [255, 255, 0, 255]),
    (4.0, [255, 80, 0, 255]),
    (6.0, [255, 0, 0, 255]),
];

// ---------------------------------------------------------------------------
// Correlation coefficient (unitless) over 0.2..1.0: gray -> yellow -> green.
// Low CC (debris/tornado signatures) reads gray, high CC reads green.
// ---------------------------------------------------------------------------
const CC_STOPS: &[Stop] = &[
    (0.2, [120, 120, 120, 255]),
    (0.55, [255, 255, 0, 255]),
    (0.75, [180, 255, 0, 255]),
    (1.0, [0, 255, 0, 255]),
];

/// VIL (kg/m²): a rainfall-intensity-style ramp — cool through green to
/// yellow, red and magenta for the extreme columns.
const VIL_STOPS: &[Stop] = &[
    (0.0, [30, 60, 120, 0]),
    (2.0, [0, 0, 180, 255]),
    (10.0, [0, 160, 160, 255]),
    (25.0, [0, 180, 0, 255]),
    (40.0, [255, 220, 0, 255]),
    (55.0, [255, 120, 0, 255]),
    (70.0, [255, 0, 0, 255]),
    (90.0, [255, 0, 255, 255]),
];

/// Echo Tops (kft, thousands of feet — the operational product unit): a
/// height ramp — cool low, hot high.
const TOPS_STOPS: &[Stop] = &[
    (0.0, [30, 60, 120, 0]),
    (10.0, [0, 120, 255, 255]),
    (20.0, [0, 200, 200, 255]),
    (30.0, [0, 200, 0, 255]),
    (40.0, [255, 220, 0, 255]),
    (50.0, [255, 120, 0, 255]),
    (60.0, [255, 0, 0, 255]),
    (70.0, [255, 0, 255, 255]),
];

/// Maximum Estimated Hail Size (inches): transparent below a quarter inch,
/// then the hail-size ramp viewers read for severe cores.
const MEHS_STOPS: &[Stop] = &[
    (0.0, [30, 60, 120, 0]),
    (0.25, [0, 120, 255, 255]),
    (0.5, [0, 200, 200, 255]),
    (0.75, [0, 200, 0, 255]),
    (1.0, [255, 220, 0, 255]),
    (1.5, [255, 120, 0, 255]),
    (2.0, [255, 0, 0, 255]),
    (3.0, [255, 0, 255, 255]),
];

fn stops_for(kind: Moment) -> &'static [Stop] {
    match kind {
        Moment::Reflectivity | Moment::CompositeReflectivity => REFLECTIVITY_STOPS,
        Moment::Velocity => VELOCITY_STOPS,
        Moment::SpectrumWidth => SPECTRUM_WIDTH_STOPS,
        Moment::DifferentialReflectivity => ZDR_STOPS,
        Moment::DifferentialPhase => PHI_STOPS,
        Moment::SpecificDifferentialPhase => KDP_STOPS,
        Moment::CorrelationCoefficient => CC_STOPS,
        // Derived products share the "intensity" ramps: VIL and hail size
        // read like rainfall intensity, echo tops like a height ramp.
        Moment::Vil => VIL_STOPS,
        Moment::EchoTops => TOPS_STOPS,
        Moment::Mesh => MEHS_STOPS,
        Moment::StormRelativeVelocity => VELOCITY_STOPS,
    }
}

impl ColorTable {
    pub fn for_moment(kind: Moment) -> Self {
        Self {
            kind,
            stops: stops_for(kind),
        }
    }

    /// The moment this table renders (for `label()` / `unit()`).
    pub fn kind(&self) -> Moment {
        self.kind
    }

    /// Short label for legends, e.g. "REF".
    pub fn label(&self) -> &'static str {
        self.kind.label()
    }

    /// Physical unit for legends, e.g. "dBZ".
    pub fn unit(&self) -> &'static str {
        self.kind.unit()
    }

    /// The raw stops (value, RGBA), for building a legend/gradient.
    pub fn stops(&self) -> &'static [Stop] {
        self.stops
    }

    /// Physical value range covered by the table. Values below the lower
    /// bound render fully transparent ("below-range").
    pub fn range(&self) -> (f32, f32) {
        debug_assert!(self.stops.len() >= 2);
        (self.stops[0].0, self.stops[self.stops.len() - 1].0)
    }

    /// Map a physical value (dBZ, m/s, ...) to straight (non-premultiplied)
    /// RGBA. Values below the table range are `[0, 0, 0, 0]`.
    pub fn map(&self, v: f32) -> [u8; 4] {
        let stops = self.stops;
        if stops.is_empty() || !v.is_finite() {
            return [0, 0, 0, 0];
        }
        if v < stops[0].0 {
            return [0, 0, 0, 0];
        }
        let last = stops[stops.len() - 1];
        if v >= last.0 {
            return last.1;
        }
        let i = match stops
            .binary_search_by(|s| s.0.partial_cmp(&v).unwrap_or(std::cmp::Ordering::Equal))
        {
            // Exact hit on a stop.
            Ok(i) => return stops[i].1,
            // v sits between stops[i - 1] and stops[i]; v >= stops[0] so i >= 1.
            Err(i) => i - 1,
        };
        let (v0, c0) = stops[i];
        let (v1, c1) = stops[i + 1];
        let t = if v1 > v0 { (v - v0) / (v1 - v0) } else { 0.0 };
        let mut out = [0u8; 4];
        for (o, (a, b)) in out.iter_mut().zip(c0.iter().zip(c1.iter())) {
            *o = (*a as f32 + (*b as f32 - *a as f32) * t)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[Moment] = &[
        Moment::Reflectivity,
        Moment::Velocity,
        Moment::SpectrumWidth,
        Moment::DifferentialReflectivity,
        Moment::DifferentialPhase,
        Moment::SpecificDifferentialPhase,
        Moment::CorrelationCoefficient,
        Moment::CompositeReflectivity,
    ];

    #[test]
    fn below_range_is_transparent_for_every_moment() {
        for &m in ALL {
            let t = ColorTable::for_moment(m);
            let (lo, hi) = t.range();
            assert!(lo < hi, "{m:?} empty range");
            let below = t.map(lo - 1.0);
            assert_eq!(below, [0, 0, 0, 0], "{m:?} below-range alpha must be 0");
            assert_eq!(
                t.map(f32::NAN),
                [0, 0, 0, 0],
                "{m:?} NaN must be transparent"
            );
            // Inside the range the color is opaque (upper half at least).
            let mid = t.map((lo + hi) / 2.0);
            assert_eq!(mid[3], 255, "{m:?} mid-range must be opaque");
            // Above the range clamps to the last stop (never transparent).
            assert_eq!(t.map(hi + 1e6)[3], 255, "{m:?} above-range alpha");
        }
    }

    #[test]
    fn reflectivity_ramp_is_ordered() {
        let t = ColorTable::for_moment(Moment::Reflectivity);
        assert_eq!(t.label(), "REF");
        assert_eq!(t.unit(), "dBZ");
        // Warmth (red channel) is monotonically non-decreasing from green
        // through yellow/orange/red/magenta/white.
        let mut prev = 0u8;
        for v in (16..=75).step_by(1) {
            let c = t.map(v as f32);
            assert!(c[0] >= prev, "red decreased at {v} dBZ: {} < {prev}", c[0]);
            prev = c[0];
        }
        // The transparent floor sits at 5 dBZ and fades in by 8.
        assert_eq!(t.map(4.0)[3], 0);
        assert_eq!(t.map(5.0)[3], 0);
        assert_eq!(t.map(8.0)[3], 255);
        assert_eq!(t.map(75.0), [255, 255, 255, 255]);
        // Composite reflectivity shares the REF ramp.
        assert_eq!(
            ColorTable::for_moment(Moment::CompositeReflectivity).map(40.0),
            t.map(40.0)
        );
    }

    #[test]
    fn velocity_sign_colors_and_steps() {
        let t = ColorTable::for_moment(Moment::Velocity);
        // NEXRAD convention: negative = inbound = green, positive = red.
        let inbound = t.map(-12.0);
        assert!(
            inbound[1] > inbound[0] && inbound[1] > inbound[2],
            "inbound must be green"
        );
        let outbound = t.map(12.0);
        assert!(
            outbound[0] > outbound[1] && outbound[0] > outbound[2],
            "outbound must be red"
        );
        assert_eq!(inbound[3], 255);
        // Discrete bands: constant inside [5,10), different at 10.
        assert_eq!(t.map(5.5), t.map(9.99));
        assert_ne!(t.map(9.99), t.map(10.0));
        assert_ne!(t.map(-6.0), t.map(-12.0));
        // Symmetric extremes clamp to white; beyond -50 is below-range.
        assert_eq!(t.map(40.0), [255, 255, 255, 255]);
        assert_eq!(t.map(-40.0), [255, 255, 255, 255]);
        assert_eq!(t.map(-51.0), [0, 0, 0, 0]);
        assert_eq!(t.range(), (-50.0, 50.0));
    }

    #[test]
    fn correlation_coefficient_gray_to_yellow_to_green() {
        let t = ColorTable::for_moment(Moment::CorrelationCoefficient);
        assert_eq!(t.range(), (0.2, 1.0));
        let low = t.map(0.3);
        assert!(
            (low[0] as i32 - low[1] as i32).abs() < 40,
            "low CC should be grayish: {low:?}"
        );
        // Green channel never decreases from 0.2 to 1.0.
        let mut prev = 0u8;
        for i in 0..=100 {
            let v = 0.2 + 0.8 * i as f32 / 100.0;
            let c = t.map(v);
            assert!(c[1] >= prev, "green decreased at {v}: {} < {prev}", c[1]);
            prev = c[1];
        }
        let high = t.map(1.0);
        assert!(high[1] > high[2], "high CC should be green: {high:?}");
        assert_eq!(t.map(0.1), [0, 0, 0, 0]);
    }

    #[test]
    fn stops_are_sorted_and_interpolation_is_linear() {
        for &m in ALL {
            let t = ColorTable::for_moment(m);
            for w in t.stops().windows(2) {
                assert!(
                    w[0].0 < w[1].0,
                    "{m:?} stops not strictly increasing: {}",
                    w[0].0
                );
            }
        }
        let t = ColorTable::for_moment(Moment::Reflectivity);
        // Exactly between two stops the channels sit at the midpoint.
        let a = t.map(42.0);
        let b = t.map(48.0);
        let mid = t.map(45.0);
        for ch in 0..4 {
            let expected = (a[ch] as i32 + b[ch] as i32) / 2;
            assert!(
                (mid[ch] as i32 - expected).abs() <= 1,
                "channel {ch} not linear"
            );
        }
    }
}
