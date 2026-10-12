//! VCP (Volume Coverage Pattern) → scan-mode classification for display.
//!
//! NEXRAD runs two families of patterns: fast "precip mode" scans (full
//! volume every ~4-6 min) and slow "clear-air mode" scans (~9-12 min, used
//! when the atmosphere is quiet — returns are sparse/thin by design).

/// Which family the radar is scanning in.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ScanMode {
    Precip,
    ClearAir,
    Unknown,
}

impl ScanMode {
    /// Classify a known VCP number. Returns `None` for unrecognized VCPs
    /// (experimental/maintenance patterns) so callers can fall back.
    pub fn from_vcp(vcp: u16) -> Option<ScanMode> {
        match vcp {
            // Precipitation mode (fast, 14-15 cuts in ~4.5 min).
            11 | 12 | 21 | 112 | 211 | 212 | 215 | 221 => Some(ScanMode::Precip),
            // Clear-air mode: 35 = default (9 cuts/~7 min), 31 = long-pulse
            // special (5 cuts/~10 min). Confirmed against NOAA jetstream docs.
            31 | 32 | 35 => Some(ScanMode::ClearAir),
            _ => None,
        }
    }

    /// Full classification: VCP table first, else scan-to-scan cadence.
    /// NOAA cadences: precip ~4.5-6 min/volume, clear-air ~7 min (VCP 35) to
    /// ~10 min (VCP 31), so the split sits just under 6.6 min.
    pub fn detect(vcp: Option<u16>, median_dt_min: Option<f64>) -> ScanMode {
        if let Some(m) = vcp.and_then(Self::from_vcp) {
            return m;
        }
        if let Some(dt) = median_dt_min {
            if (1.0..=6.5).contains(&dt) {
                return ScanMode::Precip;
            }
            if (6.6..=30.0).contains(&dt) {
                return ScanMode::ClearAir;
            }
        }
        ScanMode::Unknown
    }

    /// Badge text for the map overlay.
    pub fn label(self) -> &'static str {
        match self {
            ScanMode::Precip => "PRECIP MODE",
            ScanMode::ClearAir => "CLEAR-AIR MODE",
            ScanMode::Unknown => "SCAN MODE UNKNOWN",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vcp_table_classifies() {
        assert_eq!(ScanMode::from_vcp(12), Some(ScanMode::Precip));
        assert_eq!(ScanMode::from_vcp(215), Some(ScanMode::Precip));
        assert_eq!(ScanMode::from_vcp(32), Some(ScanMode::ClearAir));
        // VCP 35 is NOAA's *default clear-air* pattern (9 cuts / ~7 min).
        assert_eq!(ScanMode::from_vcp(35), Some(ScanMode::ClearAir));
        assert_eq!(ScanMode::from_vcp(999), None);
    }

    #[test]
    fn cadence_fallback_classifies() {
        // Unknown VCP, but 5-minute scans => precip.
        assert_eq!(ScanMode::detect(Some(999), Some(5.1)), ScanMode::Precip);
        // Unknown VCP, 7-minute scans => clear-air (VCP 35 cadence).
        assert_eq!(ScanMode::detect(None, Some(7.0)), ScanMode::ClearAir);
        // Known VCP wins over cadence (35 at fast-looking 5 min => still CA).
        assert_eq!(ScanMode::detect(Some(35), Some(5.0)), ScanMode::ClearAir);
        // No information at all.
        assert_eq!(ScanMode::detect(None, None), ScanMode::Unknown);
    }
}
