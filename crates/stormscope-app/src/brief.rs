//! The AI meteorologist's data snapshot.
//!
//! The app can hand an entire session to a model, but only a compact, sampled
//! version is useful: one JSON document holding, for every user marker, what
//! the radar actually measures in that column (surface gates, column maximum,
//! the derived winter products), plus the warnings, storm-cell motions, spotter
//! reports, SPC context and the loaded NWS forecast. The model is told to use
//! nothing else.
//!
//! Sampling reuses the same gate rule as the map probe — nearest ray, gate at
//! the beam's slant range — so a value quoted in the brief is the value the
//! user sees when hovering that spot.

use chrono::{DateTime, Utc};
use radar_core::{Moment, Volume};
use serde_json::{Map, Value, json};

use crate::app::MarkerEta;
use crate::state::MapMarker;
use crate::ui::alerts::ring_contains;

/// Everything the brief may read. Borrowed so building one never copies a
/// volume or an alert list.
pub struct BriefInputs<'a> {
    pub site: &'a radar_core::sites::RadarSite,
    pub mode: &'a str,
    pub volume: &'a Volume,
    /// The map's Min dBZ filter, so the model knows why weak echoes are absent.
    pub min_dbz: f32,
    pub markers: &'a [MapMarker],
    pub marker_etas: &'a [Option<MarkerEta>],
    pub tracks: &'a [radar_render::StormTrack],
    pub alerts: &'a [radar_core::warnings::Warning],
    pub outlook: Option<&'a radar_core::outlooks::Outlook>,
    pub mcds: &'a [radar_data::mcds::Mcd],
    pub lsrs: &'a [radar_data::lsrs::Lsr],
    pub forecast: Option<&'a radar_core::forecast::Forecast>,
    /// Where the loaded forecast applies (station or picked point).
    pub forecast_point: Option<(f64, f64)>,
    /// The device's GPS fix, sampled like a marker so a brief always covers
    /// where the spotter actually is.
    pub gps_fix: Option<(f64, f64)>,
    pub forecast_age_min: Option<u64>,
    pub observation: Option<&'a radar_core::forecast::Observation>,
    pub totals: &'a [radar_data::StationTotals],
    /// MRMS windows (CONUS first, the sharp radar window last).
    pub mrms: Option<&'a [std::sync::Arc<radar_data::mrms::Mosaic>]>,
    /// What the user is looking at right now, so the model can decide what to
    /// change instead of moving the map blindly.
    pub view_moment: Moment,
    pub view_tilt_deg: f32,
    pub view_center: (f64, f64),
    pub view_zoom: f64,
    pub view3d_open: bool,
    pub show_melting_layer: bool,
    /// Places the model asked to sample in a previous LOOK round. Their data
    /// is appended as `requested_samples`.
    pub look_points: &'a [LookPointSample],
    /// The storm-relative volume, when the SRM product has been built.
    pub srm_volume: Option<&'a Volume>,
    /// GOES windows (CONUS first, the sharp radar window last).
    pub goes: Option<&'a [std::sync::Arc<radar_data::goes::GoesImage>]>,
    /// Minutes between the two scans the tracks came from, when known.
    pub scan_interval_min: Option<i64>,
    pub now: DateTime<Utc>,
}

/// One area the model asked to see: a centre, a radius and a human label.
#[derive(Debug, Clone)]
pub struct LookPointSample {
    pub label: String,
    pub lat: f64,
    pub lon: f64,
    pub radius_mi: f64,
}

/// A finished request: the system prompt, the user prompt (JSON) and the
/// short label shown under the report ("KTWX 12:04Z · 3 markers").
pub struct Brief {
    pub system: String,
    pub user: String,
    pub label: String,
}

/// The model's role and its rules. Kept short: every sentence here is paid
/// for on every request.
pub fn system_prompt() -> String {
    "You are a meteorologist explaining the weather to a neighbor in plain English. \
     The user message is one JSON snapshot from stormscope: radar gates sampled at the spotter's \
     current GPS location and marked locations, plus warnings, tracked storm cells, spotter \
     reports, SPC outlook/MCD context, station observations and the NWS forecast.\n\
     Rules:\n\
     - Use only the snapshot. Never invent a measurement or a time. If a variable is missing, say \
     it is not available.\n\
     - Every clock time you state must be in the user's local timezone. `as_of_local`, \
     `local_timezone` and `local_utc_offset` give the current local wall-clock time and zone; \
     `as_of` and anything ending in `_utc` is UTC — never print a `_utc` value as a clock time, \
     and always append the timezone abbreviation to clock times (for example \"9:11 PM MST\").\n\
     - Write for a normal person, not a radar operator. Do not recite radar variables or \
     acronyms (dBZ, kft, HCA, ZDR, CC, MEHS, VIL, spectrum width, echo regions, melting level). \
     Translate them into what they mean: \"a few very light returns\", \"the storm tops are low\", \
     \"no hail signature\", \"hardly any rain\". In the answer use everyday units only: °F, mph, \
     miles, inches. Give the numbers that matter (temperature, arrival time, rainfall, wind, hail \
     size); skip the rest of the data dump. Do not lose detail: timing, amounts and what to watch \
     for still belong, just in everyday words.\n\
     - If the user explicitly asks a technical question or uses radar terms, answer in kind; \
     otherwise keep both briefs and chat replies conversational.\n\
     - `current_location` is the spotter's own GPS fix, sampled exactly like a marker. Include it \
     in every brief when present, even when no markers exist, and never ask the spotter to place \
     a marker while it is present.\n\
     - Lead with the answer in the first sentence: is anything dangerous coming, when, and what \
     should the reader do? For `current_location` and each marker write 2-4 short sentences: what \
     is happening now, what the next 30-90 minutes look like, the specific hazard \
     (tornado/hail/wind/ice/snow/fog/visibility), and how confident you are.\n\
     - Respect warning polygons and tracked motion; `next_arrival_min` is the tracker's closest-\
     approach estimate for that marker, `storm_cells` are nearby cells with their own motion.\n\
     - Surface temperature outranks the radar's winter products. Only describe frozen \
     precipitation when `surface_observation.temperature_f` is 38 F or lower (or the observation \
     text names snow/ice). Above 40 F never mention snow, ice, the melting level or snow rate: HCA \
     \"mixed\" and a low melting level in weak echoes are algorithm artifacts at warm temperatures. \
     Below 20 dBZ with no `echo_regions`, call it light isolated returns (possibly clutter or \
     drizzle), not a hydrometeor type.\n\
     - Plain text only: no tables, no code blocks, no markdown headings. One short line with the \
     marker name, then the sentences. End with a single line starting \"Watch for:\" naming the \
     biggest uncertainty.\n\
     - Length: under 220 words for up to 3 locations, under 350 words otherwise.\n\
     - No screenshot is taken or sent. Every value comes from the decoded radar volume and the \
     loaded overlays sampled by the app.\n\
     - `echo_regions` is the spatial overview: connected areas of >=20 dBZ on the lowest cut \
     (max dBZ, centroid, size, distance/bearing from the radar, nearest marker). Use it to see \
     where precipitation actually is, especially things the markers miss (a line to the south, \
     a cell beyond the pins). If the question is about a direction or area (\"what about the \
     south?\"), find the matching region in this list and look at ITS centroid/edges — never \
     pick points blindly along a compass direction.\n\
     - The snapshot samples only `current_location`, the markers (and any `requested_samples`). If a question needs \
     data somewhere else — a town the spotter names, a cell outside the pins — do not guess: \
     reply with ONLY a line\n\
     LOOK: {\"points\": [{\"lat\": 37.69, \"lon\": -97.34, \"label\": \"Wichita\", \"radius_mi\": 25}], \"product\": \"REF\"}\n\
     The app samples radar/MRMS/warnings/satellite at those AREAS (radius defaults to 20 mi), \
     moves the map to fit them, and sends you a second snapshot. Each area returns the centre \
     column plus `area` with the strongest echo and its distance/direction, % of the disk with \
     >=20 dBZ, the nearest echo, and warnings. At most 2 looks per question and 3 areas per \
     look. Answer from the area summary — never conclude there is nothing from a single gate. If a \
     look comes back clear while `echo_regions` shows a region nearby, look again at that region. \
     When you answer after a LOOK, centre the MAP on that area and choose a zoom that shows the \
     whole disk (not a single point).\n\
     - If the answer needs data that `data_status` says is missing — MRMS, GOES, the NWS \
     forecast, station totals, or a different SPC day/type — reply with ONLY:\n\
     DATA: {\"mrms\": true, \"goes\": true, \"forecast\": {\"lat\": 39.05, \"lon\": -95.68, \
     \"label\": \"Topeka\"}, \"totals\": {\"lat\": 39.05, \"lon\": -95.68}, \"spc\": {\"day\": 2, \
     \"kind\": \"tornado\"}}\n\
     Include only what you need. The app fetches it and sends a second snapshot; then answer \
     fully. One DATA round per question.\n\
     - `cuts` lists every elevation at each sampled column (reflectivity, velocity, ZDR, CC), so \
     a question about a specific tilt uses that tilt's actual gate.\n\
     - SPC: `spc_outlook.features` lists every category with its label/name/risk level, and \
     `spc_risk` at a marker/area is the authoritative inside-this-risk-area answer. If it is \
     absent, `spc_nearest` gives the distance to the closest area's edge. When you mention an \
     SPC distance, quote `spc_risk.summary` or `spc_nearest.summary` verbatim; never invent, \
     round from memory or re-derive a distance from the tilt or coordinates. If the field is \
     absent, say no SPC distance is available. Do not say the polygons or labels are missing.\n\
     - You are at the screen with the spotter. When the answer refers to anything the map can \
     show — a cell, a velocity couplet, a hail core, the echo top, the melting layer, a warning, \
     an approaching line — you MUST use the MAP directive to focus it: centre/zoom the marker or \
     feature, switch product/tilt, open the 3D pane or a cross-section. A real expert points at \
     the evidence; do not merely tell the spotter to look. `current_view` says what they already \
     see; change it when that helps. After the prose, on its own final line, add exactly one \
     directive like:\n\
     MAP: {\"center_lat\": 38.9, \"center_lon\": -97.2, \"zoom\": 8.5, \"product\": \"VEL\", \
     \"tilt_deg\": 0.5, \"pane\": \"split\", \"view3d_yaw\": 35, \"view3d_pitch\": 25, \
     \"view3d_dist_km\": 0, \"view3d_ve\": 6, \"section\": true, \"section_az\": 90, \
     \"show_melting_layer\": true}\n\
     Every field is optional; send only what helps (focus the marker, show the velocity couplet, \
     open the 3D pane or a cross-section, show the 0 °C level). Allowed products: REF, VEL, SW, \
     ZDR, PHI, KDP, CC, VIL, TOPS, MEHS, SRM, HCA, SNOW, MELT. pane: none, split, max3d, map. \n\
     - Mark what you are talking about with an `annotations` array inside the same MAP object: \
     [{\"lat\": 38.9, \"lon\": -97.2, \"radius_mi\": 2, \"alt_kft\": 0, \"label\": \"possible tornado\", \
     \"kind\": \"tornado\"}]. Kinds: tornado, hail, wind, flood, updraft, generic. `alt_kft` > 0 \
     puts the mark at that height in the 3D pane — use it for an updraft top or a hail core \
     that 2D cannot show. At most 4 marks, only where the data supports them, and explain each \
     choice in the prose. Under a tornado warning, put the tornado mark on the velocity \
     couplet/tight rotation (or the warning polygon centroid if velocity is missing) with a \
     1-3 mile radius.\n\
     - Annotations are for a **specific hazard or feature** the answer is about (a tornado \
     couplet, hail core, updraft top, a warning polygon) or something the spotter explicitly \
     asked you to show. Do **not** mark a broad weak-echo area, the nearest precipitation, or \
     anything in a routine quiet-weather brief. Every mark needs a short label and one clause in \
     the prose saying why it is there; keep hazard circles to 1-5 miles. Never mention the \
     directive itself.\n\
     - If the user asks a question instead of requesting a brief, answer that question directly \
     from the snapshot: lead with the answer, give the supporting numbers, and keep it under 150 \
     words unless the data demands more. Keep the same units and the MAP option.\n\
     - For a LOOK/area question, answer about THAT AREA first: coverage, nearest echo, then the \
     strongest echo and its direction. Don't lead with the markers or call an echo not a core \
     without stating what is stronger and where; the user is asking what is happening there. \
     `radar.site` in the snapshot is the radar the samples came from (the app switches to the \
     closest one automatically when needed).\n\
     - Motion: `storm_cells_all` lists every tracked cell on the scope with speed/heading and a \
     vector mean (`mean_motion`); `echo_regions[].motion` gives each area its nearest cell's \
     vector. Use these before saying motion is missing, even when the per-marker \
     `storm_cells` is empty. If no track exists, check `motion` in warnings/MCDs ('moving east at \
     30 mph'). Only after both are absent say motion is unavailable — then give the line's \
     orientation and the trend you can support, and name what is missing (the next scan). Never \
     guess a bearing."
        .to_string()
}

/// Build the snapshot and both prompts.
pub fn build(input: &BriefInputs<'_>) -> Brief {
    let site = input.site;
    let volume = input.volume;
    let scan = volume.meta.volume_start;
    let age_min = (input.now - scan).num_minutes().max(0);

    let mut root = Map::new();
    root.insert(
        "as_of".into(),
        json!(input.now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
    );
    // Wall-clock time for the model. The snapshot's _utc fields are there for
    // arithmetic; without these, a model reads 02:11Z as "2:11 AM" and quotes
    // a UTC instant as the user's local clock.
    let local_now = input.now.with_timezone(&chrono::Local);
    root.insert(
        "as_of_local".into(),
        json!(local_now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
    );
    root.insert(
        "local_timezone".into(),
        json!(iana_time_zone::get_timezone().unwrap_or_else(|_| "local".to_string())),
    );
    root.insert(
        "local_utc_offset".into(),
        json!(local_now.format("%:z").to_string()),
    );
    root.insert(
        "radar".into(),
        json!({
            "site": site.id,
            "name": site.name,
            "mode": input.mode,
            "scan_time_utc": scan.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "scan_age_min": age_min,
            "vcp": volume.meta.vcp,
            "elevation_cuts": volume.meta.sweep_count,
            "products": volume.meta.moments.iter().map(|m| m.label()).collect::<Vec<_>>(),
            "min_dbz_filter": round(input.min_dbz),
            "scan_interval_min": input.scan_interval_min,
            "storm_tracks": input.tracks.len(),
            "motion_source": if input.tracks.is_empty() {
                "none yet (a motion vector needs two scans)"
            } else {
                "radar tracks from two consecutive scans"
            },
            "note": "min_dbz_filter hides weaker echoes from the map; the brief samples the raw gates.",
        }),
    );

    if !input.tracks.is_empty() {
        let mut cells = Vec::with_capacity(input.tracks.len().min(16));
        for track in input.tracks.iter().take(16) {
            let d = radar_render::surface_distance_m(
                site.longitude,
                site.latitude,
                track.lon,
                track.lat,
            ) / 1609.344;
            let nearest = input
                .markers
                .iter()
                .map(|m| {
                    let dm = radar_render::surface_distance_m(track.lon, track.lat, m.lon, m.lat)
                        / 1609.344;
                    (dm, m)
                })
                .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            cells.push(json!({
                "lat": round4(track.lat),
                "lon": round4(track.lon),
                "peak_dbz": round(track.max_dbz),
                "speed_mph": round1(track.speed_kmh / 1.609_344),
                "heading": compass16(track.bearing_deg),
                "bearing_deg": round1(track.bearing_deg),
                "distance_from_radar_mi": round1(d),
                "nearest_marker": nearest.map(|(dm, m)| json!({
                    "name": m.display(),
                    "distance_mi": round1(dm),
                })),
            }));
        }
        // Vector mean: a usable "how is the line moving" number even when
        // individual cells are scattered across the scope.
        let (mut sum_e, mut sum_n) = (0.0f64, 0.0f64);
        let mut speed_total = 0.0f64;
        for track in input.tracks {
            sum_e += track.vel_e;
            sum_n += track.vel_n;
            speed_total += track.speed_kmh / 1.609_344;
        }
        let n = input.tracks.len() as f64;
        let mean_speed = speed_total / n;
        let mean_bearing = sum_e.atan2(sum_n).to_degrees().rem_euclid(360.0);
        root.insert(
            "storm_cells_all".into(),
            json!({
                "count": input.tracks.len(),
                "cells": cells,
                "mean_motion": {
                    "speed_mph": round1(mean_speed),
                    "heading": compass16(mean_bearing),
                    "bearing_deg": round1(mean_bearing),
                },
                "note": "every tracked cell on the scope (the app only tracks cores >=40 dBZ); mean_motion is the vector average",
            }),
        );
    }

    root.insert(
        "data_status".into(),
        json!({
            "mrms_loaded": input.mrms.is_some(),
            "mrms_time_utc": input.mrms.and_then(|m| m.first()).map(|m| m.time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            "goes_loaded": input.goes.is_some(),
            "goes_time_utc": input.goes.and_then(|g| g.first()).map(|g| g.time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            "forecast_loaded": input.forecast.is_some(),
            "forecast_place": input.forecast.map(|f| f.place.clone()),
            "observation_loaded": input.observation.is_some(),
            "storm_reports_loaded": !input.lsrs.is_empty(),
            "mcds_loaded": !input.mcds.is_empty(),
            "station_totals_loaded": !input.totals.is_empty(),
            "spc_outlook_loaded": input.outlook.is_some(),
            "spc_day": input.outlook.map(|o| o.day),
            "spc_type": input.outlook.map(|o| o.ty.label()),
            "srm_loaded": input.srm_volume.is_some(),
            "note": "MRMS/GOES/forecast/totals/spc/srm are only present when loaded; the model can request missing layers with DATA and the app fetches them. Rain/snow values are in `rain_snow_totals`, in inches.",
        }),
    );

    // Rain/snow totals in one top-level list, independent of whether a
    // station carries geometry. Per-marker `station_totals` remains for
    // distance context, but this guarantees the model can see every loaded
    // station value.
    root.insert(
        "rain_snow_totals".into(),
        json!(
            input
                .totals
                .iter()
                .take(8)
                .map(|station| json!({
                    "station": station.station_id,
                    "name": station.station_name,
                    "lat": station.lat,
                    "lon": station.lon,
                    "last_obs_utc": station.last_obs.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                    "window_hours": round1(station.window_hours),
                    "obs_count": station.obs_count,
                    "latest": station.buckets.iter().take(4).map(|(k, v)| json!({"label": k, "inches": round1(*v)})).collect::<Vec<_>>(),
                    "hourly_sums": station.hourly_sums.iter().take(3).map(|(k, v)| json!({"label": k, "inches": round1(*v)})).collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>()
        ),
    );

    root.insert(
        "echo_regions".into(),
        json!(echo_regions(volume, site, input.markers, input.tracks)),
    );

    root.insert(
        "current_view".into(),
        json!({
            "product": input.view_moment.label(),
            "tilt_deg": round1f(input.view_tilt_deg),
            "center_lat": round4(input.view_center.0),
            "center_lon": round4(input.view_center.1),
            "zoom": round1(input.view_zoom),
            "view3d_open": input.view3d_open,
            "melting_layer_on": input.show_melting_layer,
        }),
    );

    if let Some(obs) = input.observation {
        root.insert(
            "surface_observation".into(),
            json!({
                "station": obs.station_id,
                "name": obs.station_name,
                "time_utc": obs.timestamp.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                "conditions": obs.text,
                "temperature_f": obs.temperature_c.map(c_to_f).map(round1f),
                "dewpoint_f": obs.dewpoint_c.map(c_to_f).map(round1f),
                "wind_mph": obs.wind_speed_kmh.map(kmh_to_mph).map(round1f),
                "gust_mph": obs.wind_gust_kmh.map(kmh_to_mph).map(round1f),
                "wind_from_deg": obs.wind_direction_deg.map(round),
                "humidity_pct": obs.humidity_pct.map(round),
                "visibility_mi": obs.visibility_m.map(|m| round1f(m / 1609.344)),
                "pressure_inhg": obs.pressure_pa.map(|p| round2(p / 3386.389)),
                "precip_last_hour_in": obs.precip_last_hour_mm.map(|mm| round2(mm / 25.4)),
            }),
        );
        if let Some(temp_f) = obs.temperature_c.map(c_to_f) {
            let guidance = if temp_f >= 45.0 {
                "Surface temperature is well above freezing: snow and ice are NOT possible. Do                  not mention snow, ice, the melting level, or snow rate. Weak-echo HCA 'mixed'                  values are clutter or melting-layer artifacts."
            } else if temp_f >= 34.0 {
                "Surface temperature is near freezing: rain or a rain/snow mix is possible. Do                  not call it all snow unless the observation text says snow."
            } else {
                "Surface temperature is at or below freezing: snow and ice are possible; use HCA,                  snow rate and the melting level."
            };
            root.insert("precip_type_guidance".into(), json!(guidance));
        }
    }

    if let Some(forecast) = input.forecast {
        let point = input.forecast_point;
        // Every period the forecast window shows: the model must be able to
        // answer questions about the whole list, not just the first few days.
        let periods: Vec<Value> = forecast
            .periods
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "start_utc": p.start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    "is_daytime": p.is_daytime,
                    "temperature_f": round(p.temperature),
                    "wind": p.wind,
                    "short": p.short,
                    "detailed": clip(&p.detailed, 320),
                })
            })
            .collect();
        root.insert(
            "forecast".into(),
            json!({
                "office": forecast.office,
                "place": forecast.place,
                "point": point.map(|(lat, lon)| json!({"lat": round4(lat), "lon": round4(lon)})),
                "updated_utc": forecast.updated.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                "fetched_age_min": input.forecast_age_min,
                "periods": periods,
            }),
        );
    }

    if let Some(outlook) = input.outlook {
        root.insert(
            "spc_outlook".into(),
            json!({
                "day": outlook.day,
                "type": outlook.ty.label(),
                "valid_utc": outlook.valid.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                "expire_utc": outlook.expire.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                "features": outlook.features.iter().map(|f| json!({
                    "label": f.label,
                    "name": f.name,
                    "dn": f.dn,
                    "risk_level": f.risk_level(),
                    "area_sq_mi": round1(outlook_area_sq_mi(f)),
                })).collect::<Vec<_>>(),
            }),
        );
    }

    let shown = &input.markers[..input.markers.len().min(12)];
    let mut markers: Vec<Value> = Vec::with_capacity(shown.len());
    for (i, marker) in shown.iter().enumerate() {
        markers.push(marker_snapshot(input, marker, i));
    }
    root.insert("markers".into(), json!(markers));

    // Always sample the spotter's actual position: a brief must cover where
    // they are, even if they have not dropped a pin.
    if let Some((lat, lon)) = input.gps_fix {
        let marker = MapMarker {
            lat,
            lon,
            label: "My location (GPS)".into(),
        };
        let mut sample = marker_snapshot(input, &marker, input.markers.len());
        if let Some(obj) = sample.as_object_mut() {
            obj.insert("source".into(), json!("device GPS fix"));
        }
        root.insert("current_location".into(), sample);
    }

    if !input.look_points.is_empty() {
        // Data the model asked for by name; same sampling as a marker, but
        // not persisted as a pin and without an ETA cache entry.
        let requested: Vec<Value> = input
            .look_points
            .iter()
            .map(|point| look_snapshot(input, point))
            .collect();
        root.insert("requested_samples".into(), json!(requested));
    }
    if input.markers.len() > shown.len() {
        root.insert(
            "markers_omitted".into(),
            json!(input.markers.len() - shown.len()),
        );
    }

    // Warnings that are in effect and close to the radar, for situational
    // awareness the per-marker filter may have excluded.
    let mut near: Vec<&radar_core::warnings::Warning> = input
        .alerts
        .iter()
        .filter(|w| w.expires > input.now && w.centroid().is_some())
        .collect();
    near.sort_by_key(|w| w.expires);
    let nearest: Vec<Value> = near
        .into_iter()
        .take(8)
        .map(|w| {
            json!({
                "event": w.event,
                "severity": w.severity,
                "expires_in_min": (w.expires - input.now).num_minutes().max(0),
                "area": clip(&w.area, 120),
            })
        })
        .collect();
    root.insert("warnings_near_radar".into(), json!(nearest));

    let mut user = String::from("Snapshot (JSON):\n");
    user.push_str(&serde_json::to_string(&Value::Object(root)).unwrap_or_else(|_| "{}".into()));

    Brief {
        system: system_prompt(),
        user,
        label: if shown.is_empty() && input.gps_fix.is_some() {
            format!("{} {}Z · GPS location", site.id, scan.format("%H:%M"))
        } else {
            format!(
                "{} {}Z · {} marker{}{}",
                site.id,
                scan.format("%H:%M"),
                shown.len(),
                if shown.len() == 1 { "" } else { "s" },
                if input.gps_fix.is_some() {
                    " + GPS"
                } else {
                    ""
                }
            )
        },
    }
}

/// Connected areas of ≥20 dBZ on the lowest reflectivity cut — a coarse
/// spatial overview the model can reason about before it asks to sample a
/// point. Without this the brief only knows the marker columns and cannot
/// tell where a line of storms is.
fn echo_regions(
    volume: &Volume,
    site: &radar_core::sites::RadarSite,
    markers: &[MapMarker],
    tracks: &[radar_render::StormTrack],
) -> Vec<Value> {
    const MIN_DBZ: f32 = 20.0;
    const CELL_KM: f32 = 8.0;

    // The lowest cut carrying reflectivity.
    let Some(sweep) = volume
        .sweeps
        .iter()
        .find(|s| s.moment(Moment::Reflectivity).is_some())
    else {
        return Vec::new();
    };
    let Some(data) = sweep.moment(Moment::Reflectivity) else {
        return Vec::new();
    };
    let range_km = (data.range_start_m + data.gate_count as f32 * data.gate_size_m) / 1000.0;
    let n = ((2.0 * range_km) / CELL_KM).ceil() as usize + 1;
    if n == 0 || n > 200 {
        return Vec::new();
    }
    let mut grid = vec![f32::MIN; n * n];
    // Sample every ~2° and every ~1.5 km: plenty for 8 km cells.
    let ray_step = (data.ray_count / 180).max(1);
    let gate_step = ((1500.0 / data.gate_size_m).round() as usize).max(1);
    for ray in (0..data.ray_count).step_by(ray_step) {
        let Some(&az) = sweep.azimuths_deg.get(ray) else {
            continue;
        };
        if !az.is_finite() {
            continue;
        }
        let (sin, cos) = az.to_radians().sin_cos();
        for gate in (0..data.gate_count).step_by(gate_step) {
            let Some(v) = data.value(ray, gate) else {
                continue;
            };
            if v < MIN_DBZ {
                continue;
            }
            let r_km = (data.range_start_m + gate as f32 * data.gate_size_m) / 1000.0;
            if r_km < 12.0 {
                continue; // the cone of silence / nearby clutter
            }
            let x = r_km * sin;
            let y = r_km * cos;
            let ix = ((x + range_km) / CELL_KM).round() as isize;
            let iy = ((y + range_km) / CELL_KM).round() as isize;
            if ix < 0 || iy < 0 || ix >= n as isize || iy >= n as isize {
                continue;
            }
            let idx = iy as usize * n + ix as usize;
            if v > grid[idx] {
                grid[idx] = v;
            }
        }
    }

    // Flood fill the occupied cells into regions.
    let mut seen = vec![false; n * n];
    let mut regions: Vec<(f64, Value)> = Vec::new();
    let cell_mi = CELL_KM as f64 / 1.609_344;
    for start in 0..n * n {
        if seen[start] || grid[start] < MIN_DBZ {
            continue;
        }
        let mut stack = vec![start];
        seen[start] = true;
        let mut cells = 0usize;
        let mut max_dbz = f32::MIN;
        let mut strongest = (0.0f64, 0.0f64);
        let mut sum_x = 0.0f64;
        let mut sum_y = 0.0f64;
        let mut min_x = f64::MAX;
        let mut max_x = f64::MIN;
        let mut min_y = f64::MAX;
        let mut max_y = f64::MIN;
        while let Some(idx) = stack.pop() {
            let (ix, iy) = (idx % n, idx / n);
            cells += 1;
            let x = ix as f64 * CELL_KM as f64 - range_km as f64;
            let y = iy as f64 * CELL_KM as f64 - range_km as f64;
            if grid[idx] > max_dbz {
                max_dbz = grid[idx];
                strongest = (x, y);
            }
            sum_x += x;
            sum_y += y;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
            for (dx, dy) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
                let nx = ix as i32 + dx;
                let ny = iy as i32 + dy;
                if nx < 0 || ny < 0 || nx >= n as i32 || ny >= n as i32 {
                    continue;
                }
                let nidx = ny as usize * n + nx as usize;
                if !seen[nidx] && grid[nidx] >= MIN_DBZ {
                    seen[nidx] = true;
                    stack.push(nidx);
                }
            }
        }
        if cells < 4 {
            continue;
        }
        let cx = sum_x / cells as f64;
        let cy = sum_y / cells as f64;
        let lat = site.latitude + cy / 110.574;
        let lon = site.longitude + cx / (111.32 * site.latitude.to_radians().cos());
        let area_sq_mi = cells as f64 * cell_mi * cell_mi;
        let width_mi = (max_x - min_x) / 1.609_344 + cell_mi;
        let height_mi = (max_y - min_y) / 1.609_344 + cell_mi;
        // Distance and direction from the site.
        let (site_dist, site_bearing) = {
            let d = cx.hypot(cy);
            let b = cx.atan2(cy).to_degrees().rem_euclid(360.0);
            (d / 1.609_344, b)
        };
        // Nearest marker, so the model can relate a region to the user's pins.
        let mut nearest: Option<(f64, &str, f64)> = None;
        for marker in markers {
            let d = radar_render::surface_distance_m(lon, lat, marker.lon, marker.lat) / 1609.344;
            if nearest.is_none_or(|(nd, _, _)| d < nd) {
                let bearing = radar_render::initial_bearing_deg(marker.lon, marker.lat, lon, lat);
                nearest = Some((d, marker.label.as_str(), bearing));
            }
        }
        let nearest_json = nearest.map(|(d, name, bearing)| {
            json!({
                "marker": if name.trim().is_empty() { "marker" } else { name },
                "distance_mi": round1(d),
                "direction": compass16(bearing),
            })
        });
        // The nearest tracked cell gives the region a motion vector.
        let motion = tracks
            .iter()
            .map(|t| {
                (
                    radar_render::surface_distance_m(lon, lat, t.lon, t.lat) / 1000.0,
                    t,
                )
            })
            .filter(|(d, _)| *d <= 60.0)
            .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(d, t)| {
                json!({
                    "speed_mph": round1(t.speed_kmh / 1.609_344),
                    "heading": compass16(t.bearing_deg),
                    "cell_distance_mi": round1(d / 1.609_344),
                })
            });
        let value = json!({
            "max_dbz": round(max_dbz),
            "motion": motion,
            "area_sq_mi": round1(area_sq_mi),
            "width_mi": round1(width_mi),
            "height_mi": round1(height_mi),
            "centroid": { "lat": round4(lat), "lon": round4(lon) },
            "strongest_at": {
                "lat": round4(site.latitude + strongest.1 / 110.574),
                "lon": round4(site.longitude
                    + strongest.0 / (111.32 * site.latitude.to_radians().cos())),
            },
            "distance_from_radar_mi": round1(site_dist),
            "bearing_from_radar": compass16(site_bearing),
            "nearest_marker": nearest_json,
        });
        regions.push((area_sq_mi * max_dbz as f64, value));
    }
    regions.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    regions.into_iter().take(6).map(|(_, v)| v).collect()
}

/// A LOOK area: the centre column plus a disk summary (strongest echo, how
/// far/direction, coverage, nearest echo) so "near Wichita" is answered from
/// an area, not one gate.
fn look_snapshot(input: &BriefInputs<'_>, point: &LookPointSample) -> Value {
    let site = input.site;
    let distance_m =
        radar_render::surface_distance_m(site.longitude, site.latitude, point.lon, point.lat);
    let range_km = (distance_m / 1000.0) as f32;
    let bearing =
        radar_render::initial_bearing_deg(site.longitude, site.latitude, point.lon, point.lat)
            as f32;

    let mut root = Map::new();
    root.insert("label".into(), json!(point.label));
    root.insert("lat".into(), json!(round4(point.lat)));
    root.insert("lon".into(), json!(round4(point.lon)));
    root.insert("radius_mi".into(), json!(round1(point.radius_mi)));
    root.insert(
        "distance_from_radar_mi".into(),
        json!(round1(distance_m / 1609.344)),
    );
    root.insert(
        "bearing_from_radar".into(),
        json!(compass16(bearing as f64)),
    );
    root.insert(
        "radar_at_center".into(),
        radar_column(
            input.volume,
            bearing,
            range_km,
            frozen_possible(input, point.lat, point.lon) == Some(true),
        ),
    );
    root.insert(
        "area".into(),
        area_summary(input, point.lat, point.lon, point.radius_mi),
    );
    if let Some(outlook) = input.outlook {
        let (inside, nearest) = spc_for_point(outlook, point.lat, point.lon);
        if let Some(feature) = inside {
            root.insert(
                "spc_risk".into(),
                json!({
                    "day": outlook.day,
                    "category": feature.label,
                    "name": feature.name,
                    "risk_level": feature.risk_level(),
                    "summary": format!("inside {} ({})", feature.name, feature.label),
                }),
            );
        } else if let Some((feature, distance_mi)) = nearest {
            root.insert(
                "spc_nearest".into(),
                json!({
                    "day": outlook.day,
                    "category": feature.label,
                    "name": feature.name,
                    "distance_mi": round1(distance_mi),
                    "summary": format!(
                        "outside all SPC risk areas; the nearest is {} ({}) about {} miles away",
                        feature.name,
                        feature.label,
                        round1(distance_mi)
                    ),
                }),
            );
        }
    }
    Value::Object(root)
}

/// Summarise the lowest reflectivity cut inside a disk. The model uses this
/// for "what about X?" questions: `echo_coverage_pct` says whether there is
/// precipitation in the area at all, and `max_dbz_at` says where the
/// strongest part is relative to the centre.
fn area_summary(input: &BriefInputs<'_>, lat: f64, lon: f64, radius_mi: f64) -> Value {
    const MIN_DBZ: f32 = 20.0;
    const CELL_KM: f64 = 4.0;
    let radius_km = (radius_mi.max(1.0)) * 1.609_344;
    let Some(sweep) = input
        .volume
        .sweeps
        .iter()
        .find(|s| s.moment(Moment::Reflectivity).is_some())
    else {
        return json!({ "available": false });
    };
    let Some(data) = sweep.moment(Moment::Reflectivity) else {
        return json!({ "available": false });
    };
    let n = ((2.0 * radius_km) / CELL_KM).ceil() as usize + 1;
    let mut occupied = vec![false; n * n];
    let mut in_disk = 0usize;
    for iy in 0..n {
        for ix in 0..n {
            let dx = ix as f64 * CELL_KM - radius_km;
            let dy = iy as f64 * CELL_KM - radius_km;
            if dx.hypot(dy) <= radius_km {
                in_disk += 1;
            }
        }
    }
    let mut max: Option<(f32, f64, f64, f64)> = None; // dbz, glat, glon, distance
    let mut nearest: Option<f64> = None;
    let ray_step = (data.ray_count / 180).max(1);
    let gate_step = ((1500.0 / data.gate_size_m).round() as usize).max(1);
    let cos_lat = lat.to_radians().cos();
    for ray in (0..data.ray_count).step_by(ray_step) {
        let Some(&az) = sweep.azimuths_deg.get(ray) else {
            continue;
        };
        if !az.is_finite() {
            continue;
        }
        for gate in (0..data.gate_count).step_by(gate_step) {
            let Some(v) = data.value(ray, gate) else {
                continue;
            };
            let slant_m = data.range_start_m + gate as f32 * data.gate_size_m;
            let (glat, glon) = radar_render::gate_latlon(
                input.site.latitude,
                input.site.longitude,
                az,
                sweep.elevation_deg,
                slant_m,
            );
            let dx = (glon - lon) * 111.32 * cos_lat;
            let dy = (glat - lat) * 110.574;
            let d = dx.hypot(dy);
            if d > radius_km {
                continue;
            }
            let ix = ((dx + radius_km) / CELL_KM).round() as isize;
            let iy = ((dy + radius_km) / CELL_KM).round() as isize;
            if ix >= 0 && iy >= 0 && (ix as usize) < n && (iy as usize) < n && v >= MIN_DBZ {
                occupied[iy as usize * n + ix as usize] = true;
                nearest = Some(nearest.map_or(d, |n| n.min(d)));
            }
            if max.is_none_or(|(bv, _, _, _)| v > bv) {
                max = Some((v, glat, glon, d));
            }
        }
    }
    let cells = occupied.iter().filter(|c| **c).count();
    let coverage = if in_disk == 0 {
        0.0
    } else {
        cells as f64 / in_disk as f64 * 100.0
    };
    let max_at = max.map(|(dbz, glat, glon, d)| {
        json!({
            "dbz": round(dbz),
            "distance_from_center_mi": round1(d / 1.609_344),
            "direction_from_center": compass16(radar_render::initial_bearing_deg(lon, lat, glon, glat)),
            "lat": round4(glat),
            "lon": round4(glon),
        })
    });
    let warnings: Vec<Value> = input
        .alerts
        .iter()
        .filter(|w| w.expires > input.now)
        .filter(|w| {
            w.polygon
                .as_ref()
                .is_some_and(|rings| rings.iter().any(|ring| ring_contains(ring, lon, lat)))
                || w.centroid().is_some_and(|(clat, clon)| {
                    radar_render::surface_distance_m(lon, lat, clon, clat) / 1609.344
                        <= radius_mi.max(5.0)
                })
        })
        .take(3)
        .map(|w| json!({ "event": w.event, "expires_in_min": (w.expires - input.now).num_minutes().max(0) }))
        .collect();
    json!({
        "available": true,
        "threshold_dbz": MIN_DBZ,
        "echo_coverage_pct": round1(coverage),
        "nearest_echo_mi": nearest.map(|d| round1(d / 1.609_344)),
        "max_dbz_at": max_at,
        "warnings": warnings,
        "note": "coverage = % of this disk with >=20 dBZ on the lowest cut; max_dbz_at is the strongest gate inside the disk",
    })
}

fn marker_snapshot(input: &BriefInputs<'_>, marker: &MapMarker, index: usize) -> Value {
    let site = input.site;
    let (lat, lon) = (marker.lat, marker.lon);
    let distance_m = radar_render::surface_distance_m(site.longitude, site.latitude, lon, lat);
    let range_km = (distance_m / 1000.0) as f32;
    let bearing = radar_render::initial_bearing_deg(site.longitude, site.latitude, lon, lat) as f32;

    let mut root = Map::new();
    root.insert("name".into(), json!(marker.display()));
    root.insert("lat".into(), json!(round4(lat)));
    root.insert("lon".into(), json!(round4(lon)));
    root.insert(
        "distance_from_radar_mi".into(),
        json!(round1(distance_m / 1609.344)),
    );
    root.insert(
        "bearing_from_radar".into(),
        json!(compass16(bearing as f64)),
    );
    if let Some(eta) = input.marker_etas.get(index).and_then(|e| e.as_ref()) {
        root.insert(
            "next_arrival_min".into(),
            json!({
                "minutes": round1(eta.minutes),
                "cell_speed_mph": round1(eta.speed_mph),
                "cell_heading": eta.compass,
            }),
        );
    }
    root.insert(
        "radar".into(),
        radar_column(
            input.volume,
            bearing,
            range_km,
            frozen_possible(input, lat, lon) == Some(true),
        ),
    );

    // Nearby tracked cells (not only the one that triggers next_arrival).
    let mut cells: Vec<(f64, Value)> = Vec::new();
    for track in input.tracks {
        let d = radar_render::surface_distance_m(lon, lat, track.lon, track.lat) / 1609.344;
        if d > 60.0 {
            continue;
        }
        cells.push((
            d,
            json!({
                "distance_mi": round1(d),
                "direction": compass16(radar_render::initial_bearing_deg(lon, lat, track.lon, track.lat)),
                "peak_dbz": round(track.max_dbz),
                "speed_mph": round1(track.speed_kmh / 1.609344),
                "heading": compass16(track.bearing_deg),
            }),
        ));
    }
    cells.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    root.insert(
        "storm_cells".into(),
        json!(
            cells
                .into_iter()
                .take(3)
                .map(|(_, v)| v)
                .collect::<Vec<_>>()
        ),
    );

    // Warnings: containing the marker first, then nearby ones.
    let mut warnings: Vec<(bool, i64, Value)> = Vec::new();
    for warning in input.alerts {
        let contains = warning
            .polygon
            .as_ref()
            .is_some_and(|rings| rings.iter().any(|ring| ring_contains(ring, lon, lat)));
        let near = warning
            .centroid()
            .map(|(clat, clon)| {
                radar_render::surface_distance_m(lon, lat, clon, clat) / 1609.344 <= 10.0
            })
            .unwrap_or(false);
        if !contains && !near {
            continue;
        }
        let expires_in = (warning.expires - input.now).num_minutes();
        warnings.push((
            contains,
            expires_in,
            json!({
                "event": warning.event,
                "severity": warning.severity,
                "urgency": warning.urgency,
                "contains_marker": contains,
                "expires_in_min": expires_in.max(0),
                "area": clip(&warning.area, 120),
                "headline": clip(&warning.headline, 200),
                "description": clip(&warning.description, 420),
                "instruction": clip(&warning.instruction, 200),
                "motion": motion_from_text(&format!(
                    "{} {} {}",
                    warning.headline, warning.description, warning.instruction
                )),
            }),
        ));
    }
    warnings.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    root.insert(
        "warnings".into(),
        json!(
            warnings
                .into_iter()
                .take(4)
                .map(|(_, _, v)| v)
                .collect::<Vec<_>>()
        ),
    );

    // SPC risk at the marker: the highest category containing it, or the
    // distance to the nearest area if it is outside all of them.
    if let Some(outlook) = input.outlook {
        let (inside, nearest) = spc_for_point(outlook, lat, lon);
        if let Some(feature) = inside {
            root.insert(
                "spc_risk".into(),
                json!({
                    "day": outlook.day,
                    "category": feature.label,
                    "name": feature.name,
                    "risk_level": feature.risk_level(),
                    "summary": format!(
                        "inside {} ({})",
                        feature.name, feature.label
                    ),
                    "valid_utc": outlook.valid.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                }),
            );
        } else if let Some((feature, distance_mi)) = nearest {
            root.insert(
                "spc_nearest".into(),
                json!({
                    "day": outlook.day,
                    "category": feature.label,
                    "name": feature.name,
                    "distance_mi": round1(distance_mi),
                    "summary": format!(
                        "outside all SPC risk areas; the nearest is {} ({}) about {} miles away",
                        feature.name,
                        feature.label,
                        round1(distance_mi)
                    ),
                    "note": "authoritative computed distance — quote the summary",
                }),
            );
        }
    }

    // Mesoscale discussions whose area covers the marker.
    let mcds: Vec<Value> = input
        .mcds
        .iter()
        .filter(|m| m.polygon.iter().any(|ring| ring_contains(ring, lon, lat)))
        .take(2)
        .map(|m| {
            json!({
                "number": m.number,
                "areas": clip(&m.areas, 160),
                "concerning": m.concerning,
                "watch_probability": m.watch_prob,
                "valid": m.valid,
                "motion": motion_from_text(&m.text),
            })
        })
        .collect();
    root.insert("mesoscale_discussions".into(), json!(mcds));

    // Preliminary local storm reports within 25 mi.
    let mut reports: Vec<(f64, Value)> = Vec::new();
    for lsr in input.lsrs {
        let Some((rlat, rlon)) = lsr.center() else {
            continue;
        };
        let d = radar_render::surface_distance_m(lon, lat, rlon, rlat) / 1609.344;
        if d > 25.0 {
            continue;
        }
        reports.push((
            d,
            json!({
                "event": lsr.event,
                "time_utc": lsr.time,
                "magnitude": lsr.magnitude,
                "location": format!("{}, {}", lsr.city, lsr.state),
                "distance_mi": round1(d),
                "remarks": clip(&lsr.remarks, 160),
            }),
        ));
    }
    reports.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    root.insert(
        "storm_reports".into(),
        json!(
            reports
                .into_iter()
                .take(3)
                .map(|(_, v)| v)
                .collect::<Vec<_>>()
        ),
    );

    // Rain/snow totals from stations near the marker.
    let mut totals: Vec<(f64, Value)> = Vec::new();
    for station in input.totals {
        let (Some(slat), Some(slon)) = (station.lat, station.lon) else {
            continue;
        };
        let d = radar_render::surface_distance_m(lon, lat, slon, slat) / 1609.344;
        if d > 40.0 {
            continue;
        }
        totals.push((
            d,
            json!({
                "station": station.station_id,
                "name": station.station_name,
                "distance_mi": round1(d),
                "last_obs_utc": station.last_obs.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                "window_hours": round1(station.window_hours),
                "latest": station.buckets.iter().take(4).map(|(k, v)| json!({"label": k, "inches": round1(*v)})).collect::<Vec<_>>(),
                "hourly_sums": station.hourly_sums.iter().take(3).map(|(k, v)| json!({"label": k, "inches": round1(*v)})).collect::<Vec<_>>(),
            }),
        ));
    }
    totals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    root.insert(
        "station_totals".into(),
        json!(
            totals
                .into_iter()
                .take(3)
                .map(|(_, v)| v)
                .collect::<Vec<_>>()
        ),
    );

    // The loaded forecast only applies near its own point.
    let forecast_distance = input
        .forecast_point
        .map(|(flat, flon)| radar_render::surface_distance_m(lon, lat, flon, flat) / 1609.344);
    if let Some(d) = forecast_distance.filter(|d| *d <= 60.0) {
        root.insert("forecast_distance_mi".into(), json!(round1(d)));
    }

    if let Some(srm) = input.srm_volume {
        if let Some((v, _)) = gate(srm, Moment::StormRelativeVelocity, bearing, range_km) {
            root.insert(
                "srm_mph".into(),
                json!({
                    "surface": round(ms_to_mph(v)),
                    "note": "storm-relative (toward-motion subtraction); positive = away from the radar",
                }),
            );
        }
    }

    if let Some(goes) = goes_at(input.goes, lat, lon) {
        root.insert("satellite".into(), goes);
    }

    if let Some(mosaic) = mrms_at(input.mrms, lat, lon) {
        root.insert(
            "mrms".into(),
            json!({
                "dbz": mosaic.0,
                "time_utc": mosaic.1,
                "note": "national multi-radar composite at this point",
            }),
        );
    }

    Value::Object(root)
}

/// Is frozen precipitation plausible at `lat/lon` according to the loaded
/// surface observation? `None` when there is no observation, or it belongs to
/// a point too far away to represent this marker.
fn frozen_possible(input: &BriefInputs<'_>, lat: f64, lon: f64) -> Option<bool> {
    let obs = input.observation?;
    let temp_f = c_to_f(obs.temperature_c?);
    let (olat, olon) = input
        .forecast_point
        .unwrap_or((input.site.latitude, input.site.longitude));
    let d_mi = radar_render::surface_distance_m(lon, lat, olon, olat) / 1609.344;
    if d_mi > 60.0 {
        return None;
    }
    Some(temp_f <= 38.0)
}

/// One marker's radar column, sampled with the map probe's gate rule.
fn radar_column(volume: &Volume, bearing: f32, range_km: f32, include_winter: bool) -> Value {
    let mut root = Map::new();

    // Surface gates (the lowest cut carrying each moment).
    let mut surface = Map::new();
    if let Some((v, h)) = gate(volume, Moment::Reflectivity, bearing, range_km) {
        surface.insert("ref_dbz".into(), json!(round(v)));
        surface.insert("height_kft".into(), json!(round1f(h)));
    }
    if let Some((v, _)) = gate(volume, Moment::Velocity, bearing, range_km) {
        surface.insert("velocity_mph".into(), json!(round(ms_to_mph(v))));
    }
    if let Some((v, _)) = gate(volume, Moment::SpectrumWidth, bearing, range_km) {
        surface.insert("spectrum_width_mph".into(), json!(round(ms_to_mph(v))));
    }
    if let Some((v, _)) = gate(volume, Moment::DifferentialPhase, bearing, range_km) {
        surface.insert("phi_deg".into(), json!(round(v)));
    }
    if let Some((v, _)) = gate(volume, Moment::DifferentialReflectivity, bearing, range_km) {
        surface.insert("zdr_db".into(), json!(round1f(v)));
    }
    if let Some((v, _)) = gate(volume, Moment::CorrelationCoefficient, bearing, range_km) {
        surface.insert("cc".into(), json!(round2(v)));
    }
    if let Some((v, _)) = gate(volume, Moment::SpecificDifferentialPhase, bearing, range_km) {
        surface.insert("kdp_deg_km".into(), json!(round1f(v)));
    }
    root.insert("surface".into(), Value::Object(surface));

    // Reflectivity structure: the lowest echo, the column maximum and its
    // height, and an interpolated profile at handy heights.
    let mut max_ref: Option<(f32, f32)> = None;
    for sweep in &volume.sweeps {
        let Some(md) = sweep.moment(Moment::Reflectivity) else {
            continue;
        };
        let Some((v, h)) = gate_on_sweep(md, sweep, bearing, range_km) else {
            continue;
        };
        if v < 0.0 {
            continue;
        }
        if max_ref.is_none_or(|(bv, _)| v > bv) {
            max_ref = Some((v, h));
        }
    }
    let mut column = Map::new();
    if let Some((v, h)) = max_ref {
        column.insert("max_ref_dbz".into(), json!(round(v)));
        column.insert("max_ref_height_kft".into(), json!(round1f(h)));
    }
    if let Some((v, _)) = gate(volume, Moment::EchoTops, bearing, range_km) {
        column.insert("echo_top_kft".into(), json!(round1f(v)));
    }
    if let Some((v, _)) = gate(volume, Moment::Vil, bearing, range_km) {
        column.insert("vil_kg_m2".into(), json!(round1f(v)));
    }
    if let Some((v, _)) = gate(volume, Moment::Mesh, bearing, range_km) {
        column.insert("mehs_in".into(), json!(round2(v)));
    }
    if let Some((v, _)) = gate(volume, Moment::Hydrometeor, bearing, range_km) {
        if let Some(class) = radar_core::winter::HydrometeorClass::from_value(v) {
            column.insert("hca".into(), json!(class.label()));
        }
    }
    // Snow rate and the melting level only make sense with a cold surface;
    // including them in a 60 F column invited the model to call weak clutter
    // "wet snow" (the melting-layer algorithm reports a level even in clear
    // air, and the snow-rate product is nonzero on mixed echoes).
    if include_winter {
        if let Some((v, _)) = gate(volume, Moment::SnowRate, bearing, range_km) {
            column.insert("snow_rate_in_hr".into(), json!(round2(v)));
        }
        if let Some((v, _)) = gate(volume, Moment::MeltingLayer, bearing, range_km) {
            column.insert("melting_level_kft".into(), json!(round1f(v)));
        }
    }

    // Every cut at this column, so tilt-specific questions get the actual
    // gate: reflectivity, velocity, ZDR and CC at that elevation.
    let mut cuts: Vec<Value> = Vec::new();
    for sweep in &volume.sweeps {
        let Some(ref_md) = sweep.moment(Moment::Reflectivity) else {
            continue;
        };
        let Some((ref_dbz, height)) = gate_on_sweep(ref_md, sweep, bearing, range_km) else {
            continue;
        };
        let mut cut = Map::new();
        cut.insert(
            "elev_deg".into(),
            json!(round1f(
                if sweep.elevation_measured_deg.is_finite() && sweep.elevation_measured_deg != 0.0 {
                    sweep.elevation_measured_deg
                } else {
                    sweep.elevation_deg
                }
            )),
        );
        cut.insert("height_kft".into(), json!(round1f(height)));
        cut.insert("ref_dbz".into(), json!(round(ref_dbz)));
        if let Some(md) = sweep.moment(Moment::Velocity) {
            if let Some((v, _)) = gate_on_sweep(md, sweep, bearing, range_km) {
                cut.insert("vel_mph".into(), json!(round(ms_to_mph(v))));
            }
        }
        if let Some(md) = sweep.moment(Moment::DifferentialReflectivity) {
            if let Some((v, _)) = gate_on_sweep(md, sweep, bearing, range_km) {
                cut.insert("zdr_db".into(), json!(round1f(v)));
            }
        }
        if let Some(md) = sweep.moment(Moment::CorrelationCoefficient) {
            if let Some((v, _)) = gate_on_sweep(md, sweep, bearing, range_km) {
                cut.insert("cc".into(), json!(round2(v)));
            }
        }
        cuts.push(Value::Object(cut));
    }
    if !cuts.is_empty() {
        column.insert("cuts".into(), json!(cuts));
    }

    let sampler = radar_core::sampling::VolumeSampler::new(volume, Moment::Reflectivity);
    let x_km = range_km * bearing.to_radians().sin();
    let y_km = range_km * bearing.to_radians().cos();
    let mut profile: Vec<(u32, f32)> = Vec::new();
    for kft in [1u32, 3, 5, 8, 12, 16, 20, 25, 30] {
        let z_km = kft as f32 * 0.0003048;
        if let Some(v) = sampler.sample(x_km, y_km, z_km).filter(|v| *v >= 0.0) {
            profile.push((kft, round(v)));
        }
    }
    if !profile.is_empty() {
        column.insert(
            "dbz_profile".into(),
            json!(
                profile
                    .into_iter()
                    .map(|(kft, dbz)| json!([kft, dbz]))
                    .collect::<Vec<_>>()
            ),
        );
    }

    // Velocity extremes through the column: the couplet signature.
    let mut inbound = 0.0f32;
    let mut outbound = 0.0f32;
    let mut found = false;
    for sweep in &volume.sweeps {
        let Some(md) = sweep.moment(Moment::Velocity) else {
            continue;
        };
        let Some((v, _)) = gate_on_sweep(md, sweep, bearing, range_km) else {
            continue;
        };
        found = true;
        inbound = inbound.min(v);
        outbound = outbound.max(v);
    }
    if found {
        column.insert(
            "velocity_extremes_mph".into(),
            json!({
                "inbound": round(ms_to_mph(inbound)),
                "outbound": round(ms_to_mph(outbound)),
            }),
        );
    }

    root.insert("column".into(), Value::Object(column));
    Value::Object(root)
}

/// The value of `moment` at a marker: the first (lowest) cut carrying it.
fn gate(volume: &Volume, moment: Moment, bearing: f32, range_km: f32) -> Option<(f32, f32)> {
    for sweep in &volume.sweeps {
        let Some(md) = sweep.moment(moment) else {
            continue;
        };
        if let Some(v) = gate_on_sweep(md, sweep, bearing, range_km) {
            return Some(v);
        }
    }
    None
}

/// One sweep's gate at the marker's azimuth/range, with its beam height (kft).
fn gate_on_sweep(
    md: &radar_core::MomentData,
    sweep: &radar_core::Sweep,
    bearing: f32,
    range_km: f32,
) -> Option<(f32, f32)> {
    let elevation =
        if sweep.elevation_measured_deg.is_finite() && sweep.elevation_measured_deg != 0.0 {
            sweep.elevation_measured_deg
        } else {
            sweep.elevation_deg
        };
    let height_km = radar_core::sampling::beam_height_km(range_km, elevation);
    let slant = range_km.hypot(height_km);
    let ray = radar_core::sampling::nearest_ray_index(&sweep.azimuths_deg, bearing)?;
    let gate = radar_core::sampling::gate_index_at_slant(md, slant)?;
    md.value(ray, gate)
        .filter(|v| v.is_finite())
        .map(|v| (v, height_km * 3.280_84))
}

/// The GOES readout at a point: the layer/time and the rendered brightness
/// (the app's IR alpha ramp turns luma ≥ 140 into cloud, so the number is a
/// cloud-top proxy rather than a calibrated temperature).
fn goes_at(
    images: Option<&[std::sync::Arc<radar_data::goes::GoesImage>]>,
    lat: f64,
    lon: f64,
) -> Option<Value> {
    for image in images?.iter().rev() {
        if lon < image.west || lon > image.east || lat < image.south || lat > image.north {
            continue;
        }
        let fx = (lon - image.west) / (image.east - image.west) * (image.width as f64 - 1.0);
        let fy = (image.north - lat) / (image.north - image.south) * (image.height as f64 - 1.0);
        let x = (fx.round() as isize).clamp(0, image.width as isize - 1) as usize;
        let y = (fy.round() as isize).clamp(0, image.height as isize - 1) as usize;
        let i = (y * image.width + x) * 4;
        let Some(px) = image.rgba.get(i..i + 4) else {
            continue;
        };
        let luma = (u32::from(px[0]) * 3 + u32::from(px[1]) * 4 + u32::from(px[2])) / 8;
        return Some(json!({
            "layer": image.layer.label(),
            "satellite": if image.west_satellite { "GOES-West" } else { "GOES-East" },
            "time_utc": image.time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "brightness_0_255": luma,
            "cloud": luma > 140,
            "note": "rendered brightness; brighter (higher) in the IR ramp means colder cloud tops",
        }));
    }
    None
}

/// Sample the MRMS composite windows, preferring the sharp radar window.
fn mrms_at(
    mosaics: Option<&[std::sync::Arc<radar_data::mrms::Mosaic>]>,
    lat: f64,
    lon: f64,
) -> Option<(f32, String)> {
    for mosaic in mosaics?.iter().rev() {
        if lon < mosaic.west || lon > mosaic.east || lat < mosaic.south || lat > mosaic.north {
            continue;
        }
        let fx = (lon - mosaic.west) / (mosaic.east - mosaic.west) * (mosaic.width as f64 - 1.0);
        let fy =
            (mosaic.north - lat) / (mosaic.north - mosaic.south) * (mosaic.height as f64 - 1.0);
        let x = (fx.round() as isize).clamp(0, mosaic.width as isize - 1) as usize;
        let y = (fy.round() as isize).clamp(0, mosaic.height as isize - 1) as usize;
        let Some(&value) = mosaic.values.get(y * mosaic.width + x) else {
            continue;
        };
        if !value.is_finite() || !(-30.0..=100.0).contains(&value) {
            continue;
        }
        return Some((
            (value * 10.0).round() / 10.0,
            mosaic
                .time
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ));
    }
    None
}

/// Pull "moving east at 30 mph" out of NWS text. Radar tracks are the
/// authority; this is the fallback when a site has only one scan loaded and a
/// warning/MCD still carries an explicit motion.
fn motion_from_text(text: &str) -> Value {
    let lower = text.to_ascii_lowercase();
    const DIRECTIONS: [&str; 8] = [
        "northeast",
        "northwest",
        "southeast",
        "southwest",
        "north",
        "south",
        "east",
        "west",
    ];
    for direction in DIRECTIONS {
        let Some(pos) = lower.find(&format!("moving {direction}")) else {
            continue;
        };
        let tail = &lower[pos + "moving ".len() + direction.len()..];
        let Some(at) = tail.find(" at ") else {
            continue;
        };
        let after = tail[at + 4..].trim_start();
        let mut words = after.split_whitespace();
        let number = words
            .next()
            .map(|w| w.trim_end_matches(','))
            .and_then(|w| w.parse::<f64>().ok());
        let Some(speed) = number else {
            continue;
        };
        let unit = words.next().unwrap_or("");
        let speed_mph = if unit.starts_with("kt") || unit.starts_with("knot") {
            speed * 1.150_78
        } else {
            speed
        };
        return json!({
            "direction": direction,
            "speed_mph": round1(speed_mph),
            "source": "NWS text",
        });
    }
    Value::Null
}

/// Highest SPC category containing the point, plus the nearest area and its
/// distance when the point is outside all of them.
fn spc_for_point(
    outlook: &radar_core::outlooks::Outlook,
    lat: f64,
    lon: f64,
) -> (
    Option<&radar_core::outlooks::OutlookFeature>,
    Option<(&radar_core::outlooks::OutlookFeature, f64)>,
) {
    let mut inside: Option<&radar_core::outlooks::OutlookFeature> = None;
    let mut nearest: Option<(&radar_core::outlooks::OutlookFeature, f64)> = None;
    for feature in &outlook.features {
        let mut contained = false;
        let mut min_mi = f64::MAX;
        for polygon in &feature.polygons {
            for ring in polygon {
                if ring.len() < 2 {
                    continue;
                }
                if ring_contains(ring, lon, lat) {
                    contained = true;
                }
                min_mi = min_mi.min(ring_distance_mi(ring, lon, lat));
            }
        }
        if contained && inside.is_none_or(|best| feature.dn > best.dn) {
            inside = Some(feature);
        }
        if min_mi.is_finite() && nearest.is_none_or(|(_, d)| min_mi < d) {
            nearest = Some((feature, min_mi));
        }
    }
    (inside, nearest)
}

/// Distance from a point to the nearest edge of a ring, miles (equirectangular
/// approximation — these are summaries, not geodesy).
fn ring_distance_mi(ring: &[(f64, f64)], lon: f64, lat: f64) -> f64 {
    let cos = lat.to_radians().cos().max(0.2);
    let project = |(rlon, rlat): (f64, f64)| ((rlon - lon) * 111.32 * cos, (rlat - lat) * 110.574);
    let mut best = f64::MAX;
    for pair in ring.windows(2) {
        let (ax, ay) = project(pair[0]);
        let (bx, by) = project(pair[1]);
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = dx * dx + dy * dy;
        let t = if len2 <= f64::EPSILON {
            0.0
        } else {
            (-(ax * dx + ay * dy) / len2).clamp(0.0, 1.0)
        };
        let (px, py) = (ax + dx * t, ay + dy * t);
        best = best.min(px.hypot(py));
    }
    if ring.len() > 2 {
        let (ax, ay) = project(*ring.last().expect("ring"));
        let (bx, by) = project(ring[0]);
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = dx * dx + dy * dy;
        let t = if len2 <= f64::EPSILON {
            0.0
        } else {
            (-(ax * dx + ay * dy) / len2).clamp(0.0, 1.0)
        };
        best = best.min((ax + dx * t).hypot(ay + dy * t));
    }
    // `best` is kilometres (project uses km/degree); one mile is 1.609 km.
    best / 1.609_344
}

/// Rough polygon area (shoelace in the local tangent plane), square miles.
fn outlook_area_sq_mi(feature: &radar_core::outlooks::OutlookFeature) -> f64 {
    let mut total = 0.0;
    for polygon in &feature.polygons {
        let Some(ring) = polygon.first() else {
            continue;
        };
        if ring.len() < 3 {
            continue;
        }
        let lat0 = ring.iter().map(|p| p.1).sum::<f64>() / ring.len() as f64;
        let cos = lat0.to_radians().cos().max(0.2);
        let mut area = 0.0;
        for pair in ring.windows(2) {
            let (x0, y0) = (pair[0].0 * 111.32 * cos, pair[0].1 * 110.574);
            let (x1, y1) = (pair[1].0 * 111.32 * cos, pair[1].1 * 110.574);
            area += x0 * y1 - x1 * y0;
        }
        total += area.abs() * 0.5 * 0.386_102; // km² -> mi²
    }
    total
}

fn c_to_f(c: f32) -> f32 {
    c * 9.0 / 5.0 + 32.0
}

fn kmh_to_mph(kmh: f32) -> f32 {
    kmh / 1.609_344
}

fn ms_to_mph(ms: f32) -> f32 {
    ms * 2.236_936
}

fn round(v: f32) -> f32 {
    (v * 10.0).round() / 10.0
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn round1f(v: f32) -> f32 {
    (v * 10.0).round() / 10.0
}

fn round2(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

fn clip(s: &str, n: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= n {
        trimmed.to_string()
    } else {
        let mut out: String = trimmed.chars().take(n).collect();
        out.push('…');
        out
    }
}

/// 16-wind compass point for a heading in degrees.
fn compass16(deg: f64) -> &'static str {
    const POINTS: [&str; 16] = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW",
        "NW", "NNW",
    ];
    POINTS[(((deg.rem_euclid(360.0) + 11.25) / 22.5) as usize) % 16]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A volume with a 40 dBZ column over the marker, one warning polygon
    /// around it, an SPC risk area and an MRMS pixel — enough to prove the
    /// snapshot carries each data family the prompt promises.
    #[test]
    fn snapshot_carries_radar_warnings_outlook_and_mrms() {
        use radar_core::{MomentCalibration, MomentData, Sweep, Volume, VolumeMeta};
        let calibration = MomentCalibration {
            scale: 1.0,
            offset: 2.0,
            threshold: 0.0,
        };
        let mk = |elev: f32| {
            let rays: Vec<f32> = (0..360).map(|i| i as f32).collect();
            let mut data = vec![0i16; 360 * 200];
            for r in 0..360 {
                for g in 0..200 {
                    let range_km = (g as f32 + 0.5) * 0.25;
                    let dbz = if range_km > 20.0 && range_km < 40.0 {
                        40.0
                    } else {
                        5.0
                    };
                    data[r * 200 + g] = (dbz + 2.0) as i16;
                }
            }
            Sweep {
                elevation_deg: elev,
                elevation_measured_deg: elev,
                azimuth_width_deg: 1.0,
                azimuths_deg: rays,
                moments: vec![MomentData {
                    kind: Moment::Reflectivity,
                    calibration,
                    range_start_m: 0.0,
                    gate_size_m: 250.0,
                    gate_count: 200,
                    ray_count: 360,
                    data,
                }],
            }
        };
        let mut volume = Volume {
            meta: VolumeMeta {
                site_id: "KTWX".into(),
                volume_start: chrono::Utc::now(),
                vcp: Some(212),
                sequence_number: None,
                sweep_count: 2,
                moments: vec![Moment::Reflectivity],
            },
            sweeps: vec![mk(0.5), mk(1.5)],
        };
        radar_core::derived::attach_derived(&mut volume);

        let site = radar_core::sites::site("KTWX").expect("site");
        let marker = MapMarker {
            lat: site.latitude + 30.0 / 111.0,
            lon: site.longitude,
            label: "Home".into(),
        };
        let ring = vec![
            (marker.lon - 0.5, marker.lat - 0.5),
            (marker.lon + 0.5, marker.lat - 0.5),
            (marker.lon + 0.5, marker.lat + 0.5),
            (marker.lon - 0.5, marker.lat + 0.5),
            (marker.lon - 0.5, marker.lat - 0.5),
        ];
        let warning = radar_core::warnings::Warning {
            id: "W1".into(),
            event: "Severe Thunderstorm Warning".into(),
            severity: "Severe".into(),
            urgency: "Immediate".into(),
            expires: chrono::Utc::now() + chrono::Duration::minutes(30),
            office: "OAX".into(),
            headline: "60 mph wind and quarter hail".into(),
            description: "A severe thunderstorm was located near Home.".into(),
            instruction: "Seek shelter.".into(),
            area: "Lancaster, NE".into(),
            polygon: Some(vec![ring.clone()]),
        };
        let feature = radar_core::outlooks::OutlookFeature {
            dn: 4,
            label: "SLGT".into(),
            name: "Slight Risk".into(),
            stroke: [0, 0, 0],
            fill: [0, 0, 0],
            polygons: vec![vec![ring]],
        };
        let outlook = radar_core::outlooks::Outlook {
            day: 1,
            ty: radar_core::outlooks::OutlookType::Categorical,
            issue: None,
            valid: None,
            expire: None,
            forecaster: "SPC".into(),
            features: vec![feature],
        };
        let mosaic = std::sync::Arc::new(radar_data::mrms::Mosaic {
            product: radar_data::mrms::MrmsProduct::Reflectivity,
            time: chrono::Utc::now(),
            width: 10,
            height: 10,
            north: marker.lat + 1.0,
            west: marker.lon - 1.0,
            south: marker.lat - 1.0,
            east: marker.lon + 1.0,
            values: vec![35.0; 100],
        });
        let track = radar_render::StormTrack {
            lat: site.latitude + 0.5,
            lon: site.longitude + 0.3,
            vel_e: 15.0,
            vel_n: 20.0,
            speed_kmh: 90.0,
            bearing_deg: 36.9,
            max_dbz: 48.0,
        };
        let brief = build(&BriefInputs {
            site,
            mode: "live",
            volume: &volume,
            min_dbz: 10.0,
            markers: &[marker],
            marker_etas: &[Some(MarkerEta {
                minutes: 14.0,
                speed_mph: 35.0,
                compass: "E",
            })],
            tracks: &[track],
            alerts: &[warning],
            outlook: Some(&outlook),
            mcds: &[],
            lsrs: &[],
            forecast: None,
            forecast_point: None,
            gps_fix: None,
            forecast_age_min: None,
            observation: None,
            totals: &[],
            mrms: Some(std::slice::from_ref(&mosaic)),
            view_moment: Moment::Reflectivity,
            view_tilt_deg: 0.5,
            view_center: (site.latitude, site.longitude),
            view_zoom: 8.0,
            view3d_open: false,
            show_melting_layer: true,
            look_points: &[LookPointSample {
                label: "Wichita".into(),
                lat: site.latitude - 1.0,
                lon: site.longitude - 0.5,
                radius_mi: 30.0,
            }],
            srm_volume: None,
            goes: None,
            scan_interval_min: Some(5),
            now: chrono::Utc::now(),
        });
        assert!(
            brief.system.contains("MAP:"),
            "the model must be told how to act"
        );
        assert!(brief.label.contains("KTWX"), "{}", brief.label);
        for needle in [
            r#""name":"Home""#,
            r#""max_ref_dbz":40"#,
            r#""next_arrival_min""#,
            r#""event":"Severe Thunderstorm Warning""#,
            r#""category":"SLGT""#,
            r#""mrms""#,
            r#""dbz":35.0"#,
            r#""echo_top_kft""#,
            r#""current_view""#,
            r#""as_of_local""#,
            r#""local_timezone""#,
            r#""local_utc_offset""#,
            r#""storm_cells_all""#,
            r#""mean_motion""#,
            r#""heading":"NE""#,
            r#""requested_samples""#,
            r#""label":"Wichita""#,
            r#""echo_coverage_pct""#,
            r#""max_dbz_at""#,
        ] {
            assert!(
                brief.user.contains(needle),
                "missing {needle}: {}",
                brief.user
            );
        }
    }

    fn simple_volume() -> radar_core::Volume {
        use radar_core::{MomentCalibration, MomentData, Sweep, Volume, VolumeMeta};
        let rays: Vec<f32> = (0..360).map(|i| i as f32).collect();
        let mut data = vec![0i16; 360 * 200];
        for r in 0..360 {
            for g in 0..200 {
                let range_km = (g as f32 + 0.5) * 0.25;
                if range_km > 20.0 && range_km < 40.0 {
                    data[r * 200 + g] = 12; // 10 dBZ after the +2 offset
                }
            }
        }
        let mut volume = Volume {
            meta: VolumeMeta {
                site_id: "KTWX".into(),
                volume_start: chrono::Utc::now(),
                vcp: None,
                sequence_number: None,
                sweep_count: 1,
                moments: vec![Moment::Reflectivity],
            },
            sweeps: vec![Sweep {
                elevation_deg: 0.5,
                elevation_measured_deg: 0.5,
                azimuth_width_deg: 1.0,
                azimuths_deg: rays,
                moments: vec![MomentData {
                    kind: Moment::Reflectivity,
                    calibration: MomentCalibration {
                        scale: 1.0,
                        offset: 2.0,
                        threshold: 0.0,
                    },
                    range_start_m: 0.0,
                    gate_size_m: 250.0,
                    gate_count: 200,
                    ray_count: 360,
                    data,
                }],
            }],
        };
        radar_core::derived::attach_derived(&mut volume);
        volume
    }

    /// A warm surface observation suppresses the radar's winter classifiers
    /// and the GPS fix is sampled even with no markers (the "wet snow at
    /// 63 F" regression).
    #[test]
    fn warm_surface_forbids_snow_and_gps_is_always_sampled() {
        let site = radar_core::sites::site("KTWX").expect("site");
        let volume = simple_volume();
        let observation = radar_core::forecast::Observation {
            station_id: "KTOP".into(),
            station_name: "Topeka".into(),
            timestamp: Some(chrono::Utc::now()),
            text: "Mostly Cloudy".into(),
            temperature_c: Some(17.2), // 63 F
            dewpoint_c: Some(10.0),
            wind_speed_kmh: None,
            wind_direction_deg: None,
            wind_gust_kmh: None,
            humidity_pct: None,
            pressure_pa: None,
            sea_level_pressure_pa: None,
            visibility_m: None,
            precip_last_hour_mm: None,
            precip_3h_mm: None,
            precip_6h_mm: None,
            sky: "Overcast 8000 ft".into(),
            cloud_cover_pct: Some(100.0),
            cloud_bases_ft: vec![8000.0],
            heat_index_c: None,
            wind_chill_c: None,
        };
        let gps = (site.latitude + 0.1, site.longitude + 0.1);
        let brief = build(&BriefInputs {
            site,
            mode: "live",
            volume: &volume,
            min_dbz: 20.0,
            markers: &[],
            marker_etas: &[],
            tracks: &[],
            alerts: &[],
            outlook: None,
            mcds: &[],
            lsrs: &[],
            forecast: None,
            forecast_point: Some(gps),
            gps_fix: Some(gps),
            forecast_age_min: None,
            observation: Some(&observation),
            totals: &[],
            mrms: None,
            view_moment: Moment::Reflectivity,
            view_tilt_deg: 0.5,
            view_center: gps,
            view_zoom: 8.0,
            view3d_open: false,
            show_melting_layer: false,
            look_points: &[],
            srm_volume: None,
            goes: None,
            scan_interval_min: None,
            now: chrono::Utc::now(),
        });
        for needle in [
            r#""current_location""#,
            r#""name":"My location (GPS)""#,
            r#""source":"device GPS fix""#,
            r#""precip_type_guidance""#,
            "snow and ice are NOT possible",
        ] {
            assert!(
                brief.user.contains(needle),
                "missing {needle}: {}",
                brief.user
            );
        }
        assert!(
            !brief.user.contains(r#""snow_rate_in_hr""#),
            "snow rate must be omitted at 63 F: {}",
            brief.user
        );
        assert!(
            !brief.user.contains(r#""melting_level_kft""#),
            "melting level must be omitted at 63 F: {}",
            brief.user
        );
        assert!(
            brief.label.contains("GPS") && !brief.label.contains("marker"),
            "{}",
            brief.label
        );
        assert!(
            brief.system.contains("Surface temperature outranks"),
            "the prompt must rank the surface observation first"
        );
    }

    #[test]
    fn echo_regions_find_a_line_south_of_the_radar() {
        use radar_core::{MomentCalibration, MomentData, Sweep, Volume, VolumeMeta};
        let calibration = MomentCalibration {
            scale: 1.0,
            offset: 2.0,
            threshold: 0.0,
        };
        let rays: Vec<f32> = (0..360).map(|i| i as f32).collect();
        let mut data = vec![0i16; 360 * 700];
        for r in 0..360 {
            // Only the southern rays carry the line.
            if !(165.0..=195.0).contains(&rays[r]) {
                continue;
            }
            for g in 400..600 {
                data[r * 700 + g] = 47;
            }
        }
        let sweep = Sweep {
            elevation_deg: 0.5,
            elevation_measured_deg: 0.5,
            azimuth_width_deg: 1.0,
            azimuths_deg: rays,
            moments: vec![MomentData {
                kind: Moment::Reflectivity,
                calibration,
                range_start_m: 0.0,
                gate_size_m: 250.0,
                gate_count: 700,
                ray_count: 360,
                data,
            }],
        };
        let volume = Volume {
            meta: VolumeMeta {
                site_id: "KTWX".into(),
                volume_start: chrono::Utc::now(),
                vcp: None,
                sequence_number: None,
                sweep_count: 1,
                moments: vec![Moment::Reflectivity],
            },
            sweeps: vec![sweep],
        };
        let site = radar_core::sites::site("KTWX").expect("site");
        let regions = echo_regions(&volume, site, &[], &[]);
        assert!(!regions.is_empty(), "the southern band must be found");
        let region = &regions[0];
        assert!(region["max_dbz"].as_f64().unwrap() >= 40.0, "{region}");
        assert_eq!(region["bearing_from_radar"], "S", "{region}");
        assert!(
            region["distance_from_radar_mi"].as_f64().unwrap() > 50.0,
            "{region}"
        );
        let strongest = &region["strongest_at"];
        assert!(strongest["lat"].as_f64().unwrap() < site.latitude);
    }

    #[test]
    fn warning_text_yields_motion_but_plain_text_does_not() {
        let text = "At 4:23 PM CDT, a severe thunderstorm was located 7 miles \
                    northwest of Hutchinson, moving east at 30 mph.";
        let motion = motion_from_text(text);
        assert_eq!(motion["direction"], "east");
        assert_eq!(motion["speed_mph"], 30.0);
        let kt = motion_from_text("storms moving northeast at 40 kt");
        assert_eq!(kt["direction"], "northeast");
        assert!((kt["speed_mph"].as_f64().unwrap() - 46.0).abs() < 0.1);
        assert!(motion_from_text("no motion mentioned here").is_null());
    }

    #[test]
    fn ring_distance_is_in_miles_not_km_over_1609() {
        // A one-degree-long ring edge at Home's latitude is ~53 mi away.
        // The old code divided kilometres by 1609.344 and returned 0.5 mi.
        let ring = [(0.0, 38.0), (1.0, 38.0), (1.0, 39.0), (0.0, 39.0)];
        let d = ring_distance_mi(&ring, 2.0, 39.0);
        assert!((d - 53.4).abs() < 2.0, "expected ~53 mi, got {d}");
    }

    #[test]
    fn compass_has_sixteen_points() {
        assert_eq!(compass16(0.0), "N");
        assert_eq!(compass16(90.0), "E");
        assert_eq!(compass16(225.0), "SW");
        assert_eq!(compass16(359.0), "N");
    }

    #[test]
    fn clipping_never_splits_utf8() {
        let s = "snow — heavy at times";
        let out = clip(s, 6);
        assert_eq!(out.chars().count(), 7);
        assert!(out.ends_with('…'));
    }
}
