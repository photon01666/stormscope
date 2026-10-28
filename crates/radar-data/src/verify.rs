//! Quick QPE-vs-gauge verification: sample MRMS QPE at NWS stations and
//! compare it with the station's matching rolling rainfall bucket.
//!
//! This is a situational-awareness check, not an official verification
//! scorecard. It deliberately keeps the pair list visible so a forecaster can
//! see which gauge/radar pairs are driving the summary.

use chrono::{DateTime, Utc};

use crate::forecast::StationTotals;
use crate::mrms::{Mosaic, MrmsProduct};

#[derive(Clone, Debug)]
pub struct QpeGaugePair {
    pub station_id: String,
    pub station_name: String,
    pub lat: f64,
    pub lon: f64,
    pub gauge_in: f64,
    pub qpe_in: f64,
}

impl QpeGaugePair {
    pub fn error_in(&self) -> f64 {
        self.qpe_in - self.gauge_in
    }
}

#[derive(Clone, Debug)]
pub struct QpeVerification {
    pub product: MrmsProduct,
    pub hours: u16,
    pub qpe_time: DateTime<Utc>,
    pub pairs: Vec<QpeGaugePair>,
    pub n: usize,
    pub mean_gauge_in: f64,
    pub mean_qpe_in: f64,
    pub bias_in: f64,
    pub mae_in: f64,
    pub rmse_in: f64,
    /// Pearson correlation, when both series have non-zero variance.
    pub correlation: Option<f64>,
}

pub fn compare(qpe: &Mosaic, totals: &[StationTotals], hours: u16) -> QpeVerification {
    let mut pairs = Vec::new();
    for station in totals {
        let (Some(lat), Some(lon)) = (station.lat, station.lon) else {
            continue;
        };
        let Some(gauge_in) = gauge_for_hours(station, hours) else {
            continue;
        };
        let Some(mm) = qpe.value_at(lat, lon) else {
            continue;
        };
        if !mm.is_finite() || mm < 0.0 {
            continue;
        }
        let qpe_in = f64::from(mm) / 25.4;
        if !(gauge_in.is_finite() && qpe_in.is_finite()) {
            continue;
        }
        pairs.push(QpeGaugePair {
            station_id: station.station_id.clone(),
            station_name: station.station_name.clone(),
            lat,
            lon,
            gauge_in,
            qpe_in,
        });
    }
    pairs.sort_by(|a, b| a.station_id.cmp(&b.station_id));

    let n = pairs.len();
    let mean_gauge_in = if n == 0 {
        0.0
    } else {
        pairs.iter().map(|p| p.gauge_in).sum::<f64>() / n as f64
    };
    let mean_qpe_in = if n == 0 {
        0.0
    } else {
        pairs.iter().map(|p| p.qpe_in).sum::<f64>() / n as f64
    };
    let bias_in = mean_qpe_in - mean_gauge_in;
    let mae_in = if n == 0 {
        0.0
    } else {
        pairs.iter().map(|p| p.error_in().abs()).sum::<f64>() / n as f64
    };
    let rmse_in = if n == 0 {
        0.0
    } else {
        (pairs
            .iter()
            .map(|p| p.error_in() * p.error_in())
            .sum::<f64>()
            / n as f64)
            .sqrt()
    };
    let correlation = {
        let var_g = pairs
            .iter()
            .map(|p| (p.gauge_in - mean_gauge_in).powi(2))
            .sum::<f64>();
        let var_q = pairs
            .iter()
            .map(|p| (p.qpe_in - mean_qpe_in).powi(2))
            .sum::<f64>();
        if n < 2 || var_g <= f64::EPSILON || var_q <= f64::EPSILON {
            None
        } else {
            let cov = pairs
                .iter()
                .map(|p| (p.gauge_in - mean_gauge_in) * (p.qpe_in - mean_qpe_in))
                .sum::<f64>();
            Some(cov / (var_g.sqrt() * var_q.sqrt()))
        }
    };

    QpeVerification {
        product: qpe.product,
        hours,
        qpe_time: qpe.time,
        pairs,
        n,
        mean_gauge_in,
        mean_qpe_in,
        bias_in,
        mae_in,
        rmse_in,
        correlation,
    }
}

/// The station's rainfall bucket matching `hours` (1, 3, 6 or 24). Returns
/// `None` for a station that does not report that bucket.
pub fn gauge_for_hours(station: &StationTotals, hours: u16) -> Option<f64> {
    let needle = match hours {
        1 => "last hour",
        3 => "last 3 hours",
        6 => "last 6 hours",
        24 => "last 24 hours",
        _ => return None,
    };
    station
        .buckets
        .iter()
        .find(|(label, _)| {
            let l = label.to_ascii_lowercase();
            l.contains("rain") && l.contains(needle) && !l.contains("snow") && !l.contains("ice")
        })
        .map(|(_, value)| *value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn station(id: &str, lat: f64, lon: f64, one_hour: f64) -> StationTotals {
        StationTotals {
            station_id: id.into(),
            station_name: id.into(),
            lat: Some(lat),
            lon: Some(lon),
            last_obs: None,
            buckets: vec![("rain · last hour".into(), one_hour)],
            hourly_sums: vec![],
            window_hours: 1.0,
            obs_count: 1,
        }
    }

    #[test]
    fn matches_rainfall_buckets() {
        let s = station("A", 35.0, -97.0, 0.5);
        assert_eq!(gauge_for_hours(&s, 1), Some(0.5));
        assert_eq!(gauge_for_hours(&s, 3), None);
    }

    #[test]
    fn compares_sampled_qpe_and_gauges() {
        let mosaic = Mosaic {
            product: MrmsProduct::MultiSensorQpe01h,
            time: Utc.with_ymd_and_hms(2026, 10, 27, 12, 0, 0).unwrap(),
            width: 10,
            height: 10,
            north: 36.0,
            west: -98.0,
            south: 34.0,
            east: -96.0,
            values: vec![25.4; 100],
        };
        let v = compare(
            &mosaic,
            &[
                station("A", 35.0, -97.0, 0.5),
                station("B", 35.5, -97.5, 1.0),
            ],
            1,
        );
        assert_eq!(v.n, 2);
        assert!((v.mean_gauge_in - 0.75).abs() < 1e-9);
        assert!((v.mean_qpe_in - 1.0).abs() < 1e-6);
        assert!((v.bias_in - 0.25).abs() < 1e-6);
        assert!(v.mae_in > 0.0);
    }
}
