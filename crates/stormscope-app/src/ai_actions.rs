//! The whitelisted map commands an AI reply may carry.
//!
//! The model is asked to put one optional `MAP: {...}` line at the end of a
//! brief when a view would explain it better — focus the marker, switch to
//! velocity, open the 3D pane on a cross-section. Only the fields below are
//! parsed; everything is clamped to ranges the UI itself uses, and the previous
//! view is saved so the whole manoeuvre can be undone automatically after an
//! hour.

use crate::state::{AiAnnotation, AiAnnotationKind, AiViewSaved, AppState, PaneMax};
use radar_core::Moment;

/// One view manoeuvre. Every field is optional; the model is told to send only
/// what the explanation needs.
#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct MapAction {
    #[serde(default)]
    pub center_lat: Option<f64>,
    #[serde(default)]
    pub center_lon: Option<f64>,
    #[serde(default)]
    pub zoom: Option<f64>,
    /// "REF" | "VEL" | "SW" | "ZDR" | "PHI" | "KDP" | "CC" | "VIL" | "TOPS" |
    /// "MEHS" | "SRM" | "HCA" | "SNOW" | "MELT".
    #[serde(default)]
    pub product: Option<String>,
    #[serde(default)]
    pub tilt_deg: Option<f32>,
    /// "none" / "off" closes the 3D pane; "split" opens it beside the map;
    /// "max3d" and "map" pick which pane fills the centre.
    #[serde(default)]
    pub pane: Option<String>,
    #[serde(default)]
    pub view3d_yaw: Option<f32>,
    #[serde(default)]
    pub view3d_pitch: Option<f32>,
    #[serde(default)]
    pub view3d_dist_km: Option<f32>,
    #[serde(default)]
    pub view3d_ve: Option<f32>,
    #[serde(default)]
    pub section: Option<bool>,
    #[serde(default)]
    pub section_az: Option<f32>,
    #[serde(default)]
    pub show_melting_layer: Option<bool>,
    /// Marks to draw on the maps. `Some([])` clears previous marks.
    #[serde(default)]
    pub annotations: Option<Vec<AnnotationAction>>,
}

/// One mark from the model.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AnnotationAction {
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub radius_mi: Option<f64>,
    #[serde(default)]
    pub alt_kft: Option<f32>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

impl MapAction {
    fn is_empty(&self) -> bool {
        self.center_lat.is_none()
            && self.center_lon.is_none()
            && self.zoom.is_none()
            && self.product.is_none()
            && self.tilt_deg.is_none()
            && self.pane.is_none()
            && self.view3d_yaw.is_none()
            && self.view3d_pitch.is_none()
            && self.view3d_dist_km.is_none()
            && self.view3d_ve.is_none()
            && self.section.is_none()
            && self.section_az.is_none()
            && self.show_melting_layer.is_none()
            && self.annotations.is_none()
    }

    fn touches_3d(&self) -> bool {
        self.pane.is_some()
            || self.view3d_yaw.is_some()
            || self.view3d_pitch.is_some()
            || self.view3d_dist_km.is_some()
            || self.view3d_ve.is_some()
            || self.section.is_some()
            || self.section_az.is_some()
    }
}

/// A request to sample the radar somewhere the snapshot did not cover. The
/// app answers it with a second snapshot, so the model can look before it
/// speaks instead of guessing about a place it cannot see.
#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct LookRequest {
    #[serde(default)]
    pub points: Vec<LookPoint>,
    /// Product to put on screen while looking (optional).
    #[serde(default)]
    pub product: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct LookPoint {
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub label: Option<String>,
    /// Radius of the area to examine, miles. Defaults to 20: a point sample
    /// is useless for "is it raining near X" — the area summary reports the
    /// strongest echo and its distance/direction instead.
    #[serde(default)]
    pub radius_mi: Option<f64>,
}

/// A request to fetch optional data before answering. Everything is optional;
/// the app fills only what is missing.
#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct DataRequest {
    #[serde(default)]
    pub mrms: Option<bool>,
    #[serde(default)]
    pub goes: Option<bool>,
    #[serde(default)]
    pub forecast: Option<DataPoint>,
    #[serde(default)]
    pub totals: Option<DataPoint>,
    #[serde(default)]
    pub spc: Option<DataSpc>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct DataPoint {
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct DataSpc {
    #[serde(default)]
    pub day: Option<u8>,
    /// "categorical" | "tornado" | "wind" | "hail".
    #[serde(default)]
    pub kind: Option<String>,
}

/// Split a reply into the prose the user reads, an optional manoeuvre, an
/// optional LOOK and an optional data request. Directive lines never reach the
/// report; unparseable ones are dropped too.
pub fn split_reply(
    reply: &str,
) -> (
    String,
    Option<MapAction>,
    Option<LookRequest>,
    Option<DataRequest>,
) {
    let mut prose = Vec::new();
    let mut action = None;
    let mut look = None;
    let mut data = None;
    for line in reply.lines() {
        let trimmed = line.trim();
        if let Some(json) = strip_directive(trimmed, "MAP:") {
            if action.is_none() {
                action = serde_json::from_str::<MapAction>(json.trim()).ok();
            }
            continue;
        }
        if let Some(json) = strip_directive(trimmed, "LOOK:") {
            if look.is_none() {
                look = serde_json::from_str::<LookRequest>(json.trim()).ok();
            }
            continue;
        }
        if let Some(json) = strip_directive(trimmed, "DATA:") {
            if data.is_none() {
                data = serde_json::from_str::<DataRequest>(json.trim()).ok();
            }
            continue;
        }
        prose.push(line);
    }
    (prose.join("\n").trim().to_string(), action, look, data)
}

fn strip_directive<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let lower_prefix = prefix.to_ascii_lowercase();
    let head = line.get(..prefix.len())?.to_ascii_lowercase();
    (head == lower_prefix).then(|| &line[prefix.len()..])
}

/// Apply a manoeuvre to the app state and return a one-line summary for the
/// report window. The previous view is saved first (once).
pub fn apply(action: &MapAction, state: &mut AppState) -> String {
    if action.is_empty() {
        return String::new();
    }
    state.ai_view_before = Some(AiViewSaved {
        center_lat: state.view.center_lat,
        center_lon: state.view.center_lon,
        zoom: state.view.zoom,
        moment: state.moment,
        tilt_deg: state.tilt_deg,
        view3d_open: state.view3d_open,
        pane_max: state.pane_max,
        view3d_yaw: state.view3d_yaw,
        view3d_pitch: state.view3d_pitch,
        view3d_dist_km: state.view3d_dist_km,
        view3d_ve: state.view3d_ve,
        view3d_section: state.view3d_section,
        view3d_section_az: state.view3d_section_az,
        show_melting_layer: state.show_melting_layer,
    });
    state.ai_view_restore_at =
        Some(std::time::Instant::now() + std::time::Duration::from_secs(60 * 60));

    let mut parts: Vec<String> = Vec::new();
    let moved = action.center_lat.is_some() || action.center_lon.is_some() || action.zoom.is_some();
    if let Some(lat) = action.center_lat {
        state.view.center_lat = lat.clamp(-85.0, 85.0);
    }
    if let Some(lon) = action.center_lon {
        state.view.center_lon = lon.clamp(-180.0, 180.0);
    }
    if let Some(zoom) = action.zoom {
        state.view.zoom = zoom.clamp(3.0, 14.0);
    }
    if moved {
        parts.push(format!(
            "centred {:.2}, {:.2} · z {:.1}",
            state.view.center_lat, state.view.center_lon, state.view.zoom
        ));
    }
    if let Some(name) = &action.product {
        if let Some(moment) = moment_for(name) {
            state.moment = moment;
            parts.push(moment.label().to_string());
        }
    }
    if let Some(tilt) = action.tilt_deg {
        state.tilt_deg = tilt.clamp(0.0, 20.0);
        parts.push(format!("{:.1}° tilt", state.tilt_deg));
    }
    if let Some(on) = action.show_melting_layer {
        state.show_melting_layer = on;
        parts.push(if on {
            "0 °C level on".into()
        } else {
            "0 °C level off".into()
        });
    }
    if let Some(pane) = action.pane.as_deref() {
        match pane.trim().to_ascii_lowercase().as_str() {
            "none" | "off" | "close" | "hidden" => {
                state.view3d_open = false;
                parts.push("3D pane closed".into());
            }
            "split" | "on" | "beside" => {
                state.view3d_open = true;
                state.pane_max = PaneMax::None;
                parts.push("3D pane split".into());
            }
            "max3d" | "max" | "3d" | "full3d" => {
                state.view3d_open = true;
                state.pane_max = PaneMax::View3d;
                parts.push("3D maximized".into());
            }
            "map" | "2d" | "fullmap" => {
                state.view3d_open = true;
                state.pane_max = PaneMax::Map;
                parts.push("2D maximized".into());
            }
            _ => {}
        }
    }
    if action.touches_3d() {
        state.view3d_open = true;
    }
    if let Some(yaw) = action.view3d_yaw {
        state.view3d_yaw = yaw.rem_euclid(360.0);
        parts.push(format!("yaw {:.0}°", state.view3d_yaw));
    }
    if let Some(pitch) = action.view3d_pitch {
        state.view3d_pitch = pitch.clamp(5.0, 89.0);
        parts.push(format!("pitch {:.0}°", state.view3d_pitch));
    }
    if let Some(dist) = action.view3d_dist_km {
        state.view3d_dist_km = dist.clamp(0.0, 800.0);
    }
    if let Some(ve) = action.view3d_ve {
        state.view3d_ve = ve.clamp(1.0, 20.0);
        parts.push(format!("height ×{:.0}", state.view3d_ve));
    }
    if let Some(section) = action.section {
        state.view3d_section = section;
        state.view3d_open = true;
        parts.push(if section {
            "cross-section on".into()
        } else {
            "cross-section off".into()
        });
    }
    if let Some(annotations) = &action.annotations {
        state.ai_annotations.clear();
        for annotation in annotations.iter().take(6) {
            if !annotation.lat.is_finite() || !annotation.lon.is_finite() {
                continue;
            }
            if !(-85.0..=85.0).contains(&annotation.lat)
                || !(-180.0..=180.0).contains(&annotation.lon)
            {
                continue;
            }
            let kind = annotation_kind(annotation.kind.as_deref());
            state.ai_annotations.push(AiAnnotation {
                lat: annotation.lat,
                lon: annotation.lon,
                radius_mi: annotation.radius_mi.unwrap_or(3.0).clamp(0.3, 40.0),
                alt_kft: annotation.alt_kft.unwrap_or(0.0).clamp(0.0, 60.0),
                label: annotation
                    .label
                    .as_deref()
                    .map(|l| l.trim().chars().take(48).collect())
                    .filter(|l: &String| !l.is_empty())
                    .unwrap_or_else(|| "AI mark".to_string()),
                kind,
            });
        }
        if !state.ai_annotations.is_empty() {
            let labels: Vec<&str> = state
                .ai_annotations
                .iter()
                .map(|mark| mark.label.as_str())
                .collect();
            parts.push(format!(
                "{} mark{}: {}",
                labels.len(),
                if labels.len() == 1 { "" } else { "s" },
                labels.join(", ")
            ));
        }
    }
    if let Some(az) = action.section_az {
        state.view3d_section_az = az.rem_euclid(360.0);
        state.view3d_section = true;
        state.view3d_open = true;
        parts.push(format!("slice {:.0}°", state.view3d_section_az));
    }
    parts.join(" · ")
}

/// Restore the view saved before the model's last manoeuvre.
pub fn restore(state: &mut AppState) -> bool {
    state.ai_view_restore_at = None;
    let Some(saved) = state.ai_view_before.take() else {
        return false;
    };
    state.view.center_lat = saved.center_lat;
    state.view.center_lon = saved.center_lon;
    state.view.zoom = saved.zoom;
    state.moment = saved.moment;
    state.tilt_deg = saved.tilt_deg;
    state.view3d_open = saved.view3d_open;
    state.pane_max = saved.pane_max;
    state.view3d_yaw = saved.view3d_yaw;
    state.view3d_pitch = saved.view3d_pitch;
    state.view3d_dist_km = saved.view3d_dist_km;
    state.view3d_ve = saved.view3d_ve;
    state.view3d_section = saved.view3d_section;
    state.view3d_section_az = saved.view3d_section_az;
    state.show_melting_layer = saved.show_melting_layer;
    true
}

fn annotation_kind(name: Option<&str>) -> AiAnnotationKind {
    match name.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "tornado" | "rotation" | "couplet" => AiAnnotationKind::Tornado,
        "hail" | "hailcore" => AiAnnotationKind::Hail,
        "wind" | "gust" | "downburst" => AiAnnotationKind::Wind,
        "flood" | "rain" => AiAnnotationKind::Flood,
        "updraft" | "top" | "stormtop" | "3d" | "overshooting" => AiAnnotationKind::Updraft,
        _ => AiAnnotationKind::Generic,
    }
}

fn moment_for(name: &str) -> Option<Moment> {
    Some(match name.trim().to_ascii_uppercase().as_str() {
        "REF" | "REFLECTIVITY" | "DBZ" => Moment::Reflectivity,
        "VEL" | "VELOCITY" => Moment::Velocity,
        "SW" | "SPECTRUMWIDTH" => Moment::SpectrumWidth,
        "ZDR" | "DIFFERENTIALREFLECTIVITY" => Moment::DifferentialReflectivity,
        "PHI" | "DIFFERENTIALPHASE" => Moment::DifferentialPhase,
        "KDP" | "SPECIFICDIFFERENTIALPHASE" => Moment::SpecificDifferentialPhase,
        "CC" | "CORRELATIONCOEFFICIENT" => Moment::CorrelationCoefficient,
        "VIL" => Moment::Vil,
        "TOPS" | "ECHOTops" | "ECHOTOP" => Moment::EchoTops,
        "MEHS" | "MESH" | "HAIL" => Moment::Mesh,
        "SRM" | "STORMRELATIVEVELOCITY" => Moment::StormRelativeVelocity,
        "HCA" | "HYDROMETEOR" => Moment::Hydrometeor,
        "SNOW" | "SNOWRATE" => Moment::SnowRate,
        "MELT" | "MELTING" | "MELTINGLAYER" | "ZERO" => Moment::MeltingLayer,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directives_are_stripped_and_parsed() {
        let reply = "**Home**\nA line of storms is 12 mi west.\n\nMAP: {\"product\":\"VEL\",\"tilt_deg\":0.5,\"pane\":\"split\",\"view3d_yaw\":35}\n";
        let (text, action, look, data) = split_reply(reply);
        assert!(!text.contains("MAP"), "{text}");
        assert!(text.contains("12 mi west"));
        assert!(look.is_none());
        assert!(data.is_none());
        let action = action.expect("a directive");
        assert_eq!(action.product.as_deref(), Some("VEL"));
        assert_eq!(action.view3d_yaw, Some(35.0));
    }

    #[test]
    fn look_requests_are_parsed_and_stripped() {
        let reply = "LOOK: {\"points\":[{\"lat\":37.69,\"lon\":-97.34,\"label\":\"Wichita\"}],\"product\":\"REF\"}";
        let (text, action, look, data) = split_reply(reply);
        assert!(text.is_empty());
        assert!(action.is_none());
        assert!(data.is_none());
        let look = look.expect("a look request");
        assert_eq!(look.points.len(), 1);
        assert_eq!(look.points[0].label.as_deref(), Some("Wichita"));
    }

    #[test]
    fn data_requests_are_parsed() {
        let reply = "DATA: {\"forecast\":{\"lat\":39.05,\"lon\":-95.68,\"label\":\"Topeka\"},\"mrms\":true,\"spc\":{\"day\":2,\"kind\":\"tornado\"}}";
        let (text, _, _, data) = split_reply(reply);
        assert!(text.is_empty());
        let data = data.expect("a data request");
        assert_eq!(data.mrms, Some(true));
        assert_eq!(data.forecast.as_ref().map(|p| p.lat), Some(39.05));
        assert_eq!(data.spc.and_then(|s| s.day), Some(2));
    }

    #[test]
    fn bad_or_missing_directives_are_harmless() {
        let (text, action, look, data) = split_reply("Just prose.");
        assert_eq!(text, "Just prose.");
        assert!(action.is_none());
        assert!(look.is_none());
        assert!(data.is_none());
        let (text, action, look, data) = split_reply("Prose.\nMAP: {not json}");
        assert_eq!(text, "Prose.");
        assert!(action.is_none());
        assert!(look.is_none());
        assert!(data.is_none());
    }

    #[test]
    fn applying_clamps_and_switches_product() {
        let mut state = AppState::default();
        let action: MapAction = serde_json::from_str(
            r#"{"center_lat":95,"center_lon":-400,"zoom":99,"product":"VEL","tilt_deg":-3,"pane":"split","view3d_pitch":120,"show_melting_layer":false,"annotations":[{"lat":38.9,"lon":-97.2,"radius_mi":2,"alt_kft":0,"label":"possible tornado","kind":"tornado"}]}"#,
        )
        .unwrap();
        let summary = apply(&action, &mut state);
        assert!((state.view.center_lat - 85.0).abs() < 1e-9);
        assert!((state.view.center_lon + 180.0).abs() < 1e-9);
        assert!((state.view.zoom - 14.0).abs() < 1e-6);
        assert_eq!(state.moment, Moment::Velocity);
        assert!((state.tilt_deg - 0.0).abs() < 1e-6);
        assert!(state.view3d_open);
        assert!((state.view3d_pitch - 89.0).abs() < 1e-6);
        assert!(!state.show_melting_layer);
        assert!(summary.contains("VEL"), "{summary}");
        assert_eq!(state.ai_annotations.len(), 1);
        assert_eq!(state.ai_annotations[0].kind, AiAnnotationKind::Tornado);
        assert_eq!(state.ai_annotations[0].label, "possible tornado");
        assert!(summary.contains("mark: possible tornado"), "{summary}");
        assert!(state.ai_view_before.is_some());
        assert!(state.ai_view_restore_at.is_some());
        assert!(restore(&mut state));
        assert!(state.ai_view_restore_at.is_none());
        assert_eq!(state.moment, Moment::Reflectivity);
        assert!(!state.view3d_open || state.pane_max == PaneMax::None);
    }
}
