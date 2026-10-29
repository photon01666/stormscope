//! Sounding window: model, RAP or observed RAOB profile plus derived
//! thermodynamic parameters, a Skew-T and a hodograph.

use crate::app::StormApp;

pub fn sounding_window(ctx: &egui::Context, app: &mut StormApp, area: egui::Rect) {
    if !app.state.sounding_open {
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(250));

    let target = app
        .state
        .sounding_point
        .or_else(|| radar_core::sites::site(&app.state.site_id).map(|s| (s.latitude, s.longitude)));
    if let Some((lat, lon)) = target {
        let kind = app.state.sounding_kind;
        let (loaded_matches, pending, errored) = match kind {
            crate::state::SoundingKind::Model => (
                app.state.sounding.as_ref().is_some_and(|s| {
                    (s.latitude - lat).abs() < 0.001 && (s.longitude - lon).abs() < 0.001
                }),
                app.state.sounding_requested == Some((lat, lon)),
                app.state.sounding_error.is_some(),
            ),
            crate::state::SoundingKind::Rap => (
                app.state.rap_sounding.as_ref().is_some_and(|s| {
                    (s.latitude - lat).abs() < 0.001 && (s.longitude - lon).abs() < 0.001
                }),
                app.state.rap_sounding_requested == Some((lat, lon)),
                app.state.rap_sounding_error.is_some(),
            ),
            crate::state::SoundingKind::Observed => (
                app.state.observed_sounding.is_some()
                    && app.state.observed_sounding_requested == Some((lat, lon)),
                app.state.observed_sounding_requested == Some((lat, lon)),
                app.state.observed_sounding_error.is_some(),
            ),
        };
        if !loaded_matches && !pending && !errored {
            match kind {
                crate::state::SoundingKind::Model => {
                    app.state.sounding_requested = Some((lat, lon));
                    app.state.sounding_fetched_at = None;
                    app.state.sounding_error = None;
                    let _ = app.tx.send(crate::app::DataCmd::LoadSounding(lat, lon));
                }
                crate::state::SoundingKind::Rap => {
                    app.state.rap_sounding_requested = Some((lat, lon));
                    app.state.rap_sounding_fetched_at = None;
                    app.state.rap_sounding_error = None;
                    let _ = app.tx.send(crate::app::DataCmd::LoadRapSounding(lat, lon));
                }
                crate::state::SoundingKind::Observed => {
                    app.state.observed_sounding_requested = Some((lat, lon));
                    app.state.observed_sounding_fetched_at = None;
                    app.state.observed_sounding_error = None;
                    let _ = app
                        .tx
                        .send(crate::app::DataCmd::LoadObservedSounding(lat, lon));
                }
            }
        }
    }

    let fill = cfg!(target_os = "android");
    let scale = super::big_text_scale(app);
    let screen = ctx.content_rect().size();
    let size = super::popup_size(ctx, scale, 760.0, 560.0);
    let max_h = super::popup_max_height(ctx, scale);
    let min_size = egui::vec2(
        (420.0 * scale).min((screen.x - 24.0).max(200.0)),
        (260.0 * scale).min((screen.y - 90.0).max(180.0)),
    );
    let mut open = app.state.sounding_open;
    let mut pick_from_map = false;
    let mut clear_point = false;
    let mut refresh = false;

    let mut window = egui::Window::new("Sounding")
        .id(egui::Id::new("stormscope.sounding"))
        .default_pos(super::centered_popup_pos(ctx, size))
        .max_width((screen.x - 16.0).max(200.0))
        .min_size(min_size);
    #[cfg(target_os = "android")]
    if fill {
        window = window.title_bar(false);
    }
    window = if fill {
        let inset = area.shrink2(egui::vec2(4.0, 8.0));
        window
            .fixed_rect(inset)
            .movable(false)
            .resizable(false)
            .constrain_to(inset)
    } else {
        window.default_size(size).max_height(max_h).resizable(true)
    };
    let window = window.open(&mut open);
    #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
    let mut close_requested = false;
    window.show(ctx, |ui| {
        #[cfg(target_os = "android")]
        if fill {
            close_requested |= super::phone_header(ui, "Sounding");
        }
        super::apply_text_scale(ui, scale);

        ui.horizontal_wrapped(|ui| {
            let following_station = app.state.sounding_point.is_none();
            if ui
                .selectable_label(following_station, "station")
                .on_hover_text("Use the selected radar site's coordinates")
                .clicked()
                && !following_station
            {
                clear_point = true;
                refresh = true;
            }
            let picking = app.state.sounding_pick_mode;
            if ui
                .selectable_label(picking, "Pick on map…")
                .on_hover_text("Click anywhere on the map to sound that point")
                .clicked()
            {
                pick_from_map = !picking;
            }
            if let Some((lat, lon)) = app.state.sounding_point.or(target) {
                let source = if app.state.sounding_point.is_some() {
                    "picked"
                } else {
                    "site"
                };
                ui.label(
                    egui::RichText::new(format!("· {source} {lat:.3}, {lon:.3}"))
                        .weak()
                        .small(),
                );
            }
            if ui.small_button("Refresh").clicked() {
                refresh = true;
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Profile").strong().small());
            for (kind, label, tip) in [
                (
                    crate::state::SoundingKind::Model,
                    "Model",
                    "Open-Meteo pressure-level model profile",
                ),
                (
                    crate::state::SoundingKind::Rap,
                    "RAP",
                    "RAP 13 km analysis profile from NOMADS",
                ),
                (
                    crate::state::SoundingKind::Observed,
                    "Observed RAOB",
                    "Nearest observed radiosonde launch from the Wyoming archive",
                ),
            ] {
                if ui
                    .selectable_label(app.state.sounding_kind == kind, label)
                    .on_hover_text(tip)
                    .clicked()
                    && app.state.sounding_kind != kind
                {
                    app.state.sounding_kind = kind;
                    match kind {
                        crate::state::SoundingKind::Model => {
                            app.state.sounding_error = None;
                        }
                        crate::state::SoundingKind::Rap => {
                            app.state.rap_sounding_error = None;
                        }
                        crate::state::SoundingKind::Observed => {
                            app.state.observed_sounding_error = None;
                        }
                    }
                }
            }
        });
        if app.state.sounding_pick_mode {
            ui.label(
                egui::RichText::new("Click anywhere on the map to sound that point.")
                    .strong()
                    .small(),
            );
        }
        ui.separator();

        let (active, pending, error, suffix) = match app.state.sounding_kind {
            crate::state::SoundingKind::Model => (
                app.state.sounding.clone(),
                app.state.sounding_requested.is_some(),
                app.state.sounding_error.clone(),
                "approx. model profile",
            ),
            crate::state::SoundingKind::Rap => (
                app.state.rap_sounding.clone(),
                app.state.rap_sounding_requested.is_some(),
                app.state.rap_sounding_error.clone(),
                "RAP analysis · not observed",
            ),
            crate::state::SoundingKind::Observed => (
                app.state.observed_sounding.clone(),
                app.state.observed_sounding_requested.is_some(),
                app.state.observed_sounding_error.clone(),
                "observed radiosonde",
            ),
        };
        if pending && active.is_none() && error.is_none() {
            let label = match app.state.sounding_kind {
                crate::state::SoundingKind::Model => "Loading model sounding…",
                crate::state::SoundingKind::Rap => "Loading RAP analysis…",
                crate::state::SoundingKind::Observed => "Finding nearest observed RAOB…",
            };
            ui.label(egui::RichText::new(label).weak());
        }
        if let Some(err) = &error {
            if active.is_some() {
                ui.label(
                    egui::RichText::new(format!(
                        "Last refresh failed: {err} — showing the previous profile."
                    ))
                    .weak()
                    .small(),
                );
            } else {
                ui.label(egui::RichText::new(format!("Sounding failed: {err}")).weak());
            }
        }

        if let Some(s) = active {
            ui.label(
                egui::RichText::new(format!(
                    "{} · {} levels · {suffix}",
                    s.source,
                    s.levels.len()
                ))
                .weak()
                .small(),
            );
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .id_salt("stormscope.sounding.scroll")
                .max_height((max_h - 80.0 * scale).max(220.0))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if ui.available_width() < 620.0 * scale {
                        skewt_heading(ui);
                        skewt(
                            ui,
                            &s,
                            egui::vec2(ui.available_width().max(220.0), 300.0 * scale),
                        );
                        ui.add_space(8.0);
                        hodograph_heading(ui);
                        hodograph(ui, &s, egui::vec2(220.0 * scale, 220.0 * scale));
                        ui.add_space(6.0);
                        params(ui, &s);
                    } else {
                        ui.columns(2, |cols| {
                            cols[0].vertical(|ui| {
                                skewt_heading(ui);
                                skewt(ui, &s, egui::vec2(300.0 * scale, 340.0 * scale));
                            });
                            cols[1].vertical(|ui| {
                                hodograph_heading(ui);
                                hodograph(ui, &s, egui::vec2(220.0 * scale, 220.0 * scale));
                                ui.add_space(6.0);
                                params(ui, &s);
                            });
                        });
                    }
                });
        }
    });

    if close_requested {
        open = false;
    }
    if pick_from_map {
        app.state.sounding_pick_mode = true;
        app.state.sounding_reopen_after_pick = true;
        app.state.sounding_open = false;
    }
    if clear_point {
        app.state.sounding_point = None;
        app.state.sounding = None;
        app.state.sounding_requested = None;
        app.state.sounding_error = None;
        app.state.rap_sounding = None;
        app.state.rap_sounding_requested = None;
        app.state.rap_sounding_error = None;
        app.state.observed_sounding = None;
        app.state.observed_sounding_requested = None;
        app.state.observed_sounding_station = None;
        app.state.observed_sounding_error = None;
    }
    if refresh {
        let target = app.state.sounding_point.or_else(|| {
            radar_core::sites::site(&app.state.site_id).map(|s| (s.latitude, s.longitude))
        });
        if let Some((lat, lon)) = target {
            match app.state.sounding_kind {
                crate::state::SoundingKind::Model => {
                    app.state.sounding = None;
                    app.state.sounding_requested = Some((lat, lon));
                    app.state.sounding_error = None;
                    let _ = app.tx.send(crate::app::DataCmd::LoadSounding(lat, lon));
                }
                crate::state::SoundingKind::Rap => {
                    app.state.rap_sounding = None;
                    app.state.rap_sounding_requested = Some((lat, lon));
                    app.state.rap_sounding_error = None;
                    let _ = app.tx.send(crate::app::DataCmd::LoadRapSounding(lat, lon));
                }
                crate::state::SoundingKind::Observed => {
                    app.state.observed_sounding = None;
                    app.state.observed_sounding_requested = Some((lat, lon));
                    app.state.observed_sounding_error = None;
                    let _ = app
                        .tx
                        .send(crate::app::DataCmd::LoadObservedSounding(lat, lon));
                }
            }
        }
    }
    app.state.sounding_open = open;
}

fn skewt_heading(ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("Skew-T (approx)").strong().small())
        .on_hover_text(
            "Red = temperature, green = dew point. Pressure decreases upward; the temperature \
             scale is skewed. Lines close together mean moist air; lines far apart mean dry air.",
        );
}

fn hodograph_heading(ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("Hodograph").strong().small())
        .on_hover_text(
            "Wind vectors from the surface to the top. The rings are wind speed; up is north, \
             right is east. A long line means speed shear, a curve means directional shear.",
        );
}

pub(super) fn skewt(ui: &mut egui::Ui, s: &Sounding, size: egui::Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, egui::Color32::from_gray(10));
    let pmin = 100.0f64;
    let pmax = 1000.0f64;
    let tmin = -40.0f32;
    let tmax = 40.0f32;
    let px_per_deg = rect.width() / (tmax - tmin);
    let py = |pressure: f64| -> f32 {
        let lp = (pressure.ln() - pmin.ln()) / (pmax.ln() - pmin.ln());
        rect.top() + lp as f32 * rect.height()
    };
    // Isotherm grid.
    for t in (-40..=40).step_by(10) {
        let x0 = rect.left() + (t as f32 - tmin) * px_per_deg;
        p.line_segment(
            [egui::pos2(x0, rect.top()), egui::pos2(x0, rect.bottom())],
            egui::Stroke::new(0.5, egui::Color32::from_gray(35)),
        );
    }
    // Pressure labels.
    for &pr in &[1000.0, 850.0, 700.0, 500.0, 300.0, 200.0, 100.0] {
        let y = py(pr).clamp(rect.top() + 7.0, rect.bottom() - 7.0);
        p.text(
            egui::pos2(rect.left() + 2.0, y),
            egui::Align2::LEFT_CENTER,
            format!("{pr:.0}"),
            egui::FontId::monospace(9.0),
            egui::Color32::from_gray(120),
        );
    }
    let skew = |t: f64, p: f64| -> f32 {
        rect.left() + ((t as f32 - tmin) + (1000.0 - p as f32) * 0.05) * px_per_deg
    };
    // Temperature profile.
    let mut tpts = Vec::new();
    let mut dpts = Vec::new();
    for l in &s.levels {
        tpts.push(egui::pos2(
            skew(l.temp_c, l.pressure_hpa),
            py(l.pressure_hpa),
        ));
        dpts.push(egui::pos2(
            skew(l.dewpoint_c, l.pressure_hpa),
            py(l.pressure_hpa),
        ));
    }
    if tpts.len() > 1 {
        p.add(egui::Shape::line(
            tpts,
            egui::Stroke::new(2.0, egui::Color32::from_rgb(220, 80, 80)),
        ));
    }
    if dpts.len() > 1 {
        p.add(egui::Shape::line(
            dpts,
            egui::Stroke::new(2.0, egui::Color32::from_rgb(80, 200, 120)),
        ));
    }
    // LCL marker.
    if let (Some(p_lcl), Some(z_lcl)) = (s.lcl_pressure_hpa, s.lcl_height_m) {
        let y = py(p_lcl);
        let lcl_t = s.lcl_temp_c.unwrap_or(s.surface_temp_c);
        let x = skew(lcl_t, p_lcl);
        p.circle_filled(egui::pos2(x, y), 3.0, egui::Color32::YELLOW);
        p.text(
            egui::pos2(x + 4.0, y),
            egui::Align2::LEFT_CENTER,
            format!("LCL {:.0}hPa", p_lcl),
            egui::FontId::monospace(9.0),
            egui::Color32::YELLOW,
        );
        let _ = z_lcl;
    }
}

pub(super) fn hodograph(ui: &mut egui::Ui, s: &Sounding, size: egui::Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, egui::Color32::from_gray(10));
    let center = rect.center();
    let scale = rect.width().min(rect.height()) / 80.0;
    // axes
    p.line_segment(
        [
            egui::pos2(rect.left(), center.y),
            egui::pos2(rect.right(), center.y),
        ],
        egui::Stroke::new(0.5, egui::Color32::from_gray(60)),
    );
    p.line_segment(
        [
            egui::pos2(center.x, rect.top()),
            egui::pos2(center.x, rect.bottom()),
        ],
        egui::Stroke::new(0.5, egui::Color32::from_gray(60)),
    );
    for r in [10.0, 20.0, 30.0] {
        p.circle_stroke(
            center,
            r * scale,
            egui::Stroke::new(0.5, egui::Color32::from_gray(40)),
        );
    }
    let mut pts = Vec::new();
    for l in &s.levels {
        let x = center.x + l.u_ms as f32 * scale;
        let y = center.y - l.v_ms as f32 * scale;
        pts.push(egui::pos2(x, y));
    }
    if pts.len() > 1 {
        p.add(egui::Shape::line(
            pts.clone(),
            egui::Stroke::new(2.0, egui::Color32::from_rgb(120, 180, 255)),
        ));
    }
    for (i, pt) in pts.iter().enumerate() {
        let col = if i < s.levels.len() / 2 {
            egui::Color32::from_rgb(120, 220, 120)
        } else {
            egui::Color32::from_rgb(220, 120, 120)
        };
        p.circle_filled(*pt, 2.5, col);
    }
}

fn potential_bar(ui: &mut egui::Ui, name: &str, score: f64) {
    let color = match score {
        s if s < 20.0 => egui::Color32::from_rgb(70, 150, 90),
        s if s < 40.0 => egui::Color32::from_rgb(170, 160, 60),
        s if s < 60.0 => egui::Color32::from_rgb(200, 130, 40),
        s if s < 80.0 => egui::Color32::from_rgb(210, 70, 50),
        _ => egui::Color32::from_rgb(235, 45, 60),
    };
    let label = Sounding::potential_label(score);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(name).strong().small());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!("{score:.0}/100 · {label}"))
                    .strong()
                    .small(),
            );
        });
    });
    ui.add(
        egui::ProgressBar::new((score / 100.0) as f32)
            .desired_width(ui.available_width())
            .fill(color),
    );
}

pub(super) fn params(ui: &mut egui::Ui, s: &Sounding) {
    ui.label(egui::RichText::new("Environment potential").strong())
        .on_hover_text(
            "A heuristic 0–100 favorability score, not an official forecast or \n\
             a substitute for SPC outlooks and warnings. It blends CAPE, shear, \n\
             SRH and LCL height, and is gated to near zero when surface-based \n\
             CAPE is negligible, CIN is strong, or the LFC is higher than 500 hPa.",
        );
    potential_bar(ui, "Severe storms", s.severe_potential());
    potential_bar(ui, "Tornadoes", s.tornado_potential());
    potential_bar(ui, "Winter precipitation", s.winter_potential());
    let winter_type = s.winter_type();
    let winter_note = match winter_type {
        "snow" => "Type: snow — cold column",
        "mix" => "Type: mix — warm layer aloft over a cold surface",
        "ice" => "Type: ice — warm nose aloft over a sub-freezing surface",
        "rain" => "Type: rain — surface wet-bulb is above freezing",
        _ => "Type: unknown",
    };
    ui.label(egui::RichText::new(winter_note).small().weak())
        .on_hover_text(
            "Winter precipitation type inferred from the thermal profile. \n\
         Snow needs a cold column; ice/freezing rain needs a warm layer above \
         a sub-freezing surface. Rain means the surface is too warm.",
        );
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("0 minimal").small().weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new("100 very high").small().weak());
        });
    });
    ui.label(
        egui::RichText::new("Heuristic only — not a forecast; check SPC and NWS warnings.")
            .weak()
            .small(),
    );
    ui.add_space(6.0);
    ui.label(egui::RichText::new(format!("CAPE (SB) ~{:.0} J/kg", s.sb_cape_jkg)).strong())
        .on_hover_text("Surface-based convective available potential energy. More CAPE = more energy for updrafts.");
    ui.label(egui::RichText::new(format!("CIN (SB) {:.0} J/kg", s.sb_cin_jkg)).weak())
        .on_hover_text("Convective inhibition. Negative values mean a cap is present; stronger negative values require more forcing to break it.");
    ui.label(format!("LCL {:.0} hPa  {:.0} m", s.lcl_pressure_hpa.unwrap_or(0.0), s.lcl_height_m.unwrap_or(0.0)))
        .on_hover_text("Lifting condensation level: the height where a lifted surface parcel becomes saturated. Near-surface LCLs favor low cloud bases and tornado potential.");
    if let Some(p) = s.lfc_pressure_hpa {
        ui.label(format!("LFC {:.0} hPa", p))
            .on_hover_text("Level of free convection: where the parcel becomes warmer than the environment and rises freely.");
    }
    if let Some(p) = s.el_pressure_hpa {
        ui.label(format!("EL {:.0} hPa", p)).on_hover_text(
            "Equilibrium level: the top of the positive-buoyancy layer, near the anvil level.",
        );
    }
    ui.label(format!("PWAT {:.1} mm", s.pwat_mm))
        .on_hover_text("Precipitable water: total moisture in a vertical column, expressed as millimetres of liquid water.");
    if let Some(z) = s.freezing_level_m {
        ui.label(format!("0C level {:.0} m", z))
            .on_hover_text("Height where the environmental temperature reaches 0 °C. Relevant to snow level and hail growth.");
    }
    if let Some(z) = s.wet_bulb_zero_m {
        ui.label(format!("WBZ {:.0} m", z))
            .on_hover_text("Wet-bulb zero level: where the wet-bulb temperature reaches 0 °C. Useful for snow level and evaporative cooling.");
    }
    ui.label(format!("Shear 0-1/0-3/0-6: {:.0}/{:.0}/{:.0} m/s", s.shear_0_1km_ms, s.shear_0_3km_ms, s.shear_0_6km_ms))
        .on_hover_text("Bulk wind shear across the lowest 1, 3 and 6 km. Stronger shear supports storm organization and supercells.");
    ui.label(format!("SRH 0-1/0-3: {:.0}/{:.0} m²/s²", s.srh_0_1km, s.srh_0_3km))
        .on_hover_text("Storm-relative helicity: the amount of streamwise vorticity available to a storm. Positive values favor right-moving supercells; negative values favor left-moving storms.");
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("Hover any parameter for a definition.")
            .weak()
            .small(),
    );
    egui::CollapsingHeader::new("What these mean")
        .default_open(false)
        .show(ui, |ui| {
            for (name, text) in [
                ("CAPE", "Energy available to a rising air parcel. Higher values support stronger updrafts."),
                ("CIN", "Inhibition or cap. Negative values are normal; more negative means more forcing is needed to start storms."),
                ("LCL", "Cloud-base level for a surface parcel. Low LCLs mean moist low levels."),
                ("LFC", "Level where a parcel starts rising freely. No LFC means the parcel never becomes buoyant."),
                ("EL", "Top of the buoyant layer, close to the storm top / anvil."),
                ("PWAT", "Total column moisture, in millimetres if condensed."),
                ("0C level", "Freezing level. Important for snow level and hail."),
                ("WBZ", "Wet-bulb zero level. A better snow-level guide than the dry freezing level."),
                ("Shear", "Change in wind speed/direction with height. Strong shear organizes storms."),
                ("SRH", "Storm-relative helicity. Positive values favor right-moving supercells."),
                ("Winter", "Winter-precipitation favorability from the cold wet-bulb surface, column moisture, saturation and the snow-growth layer. The type (snow/mix/ice/rain) is shown in the bar label."),
            ] {
                ui.label(egui::RichText::new(format!("{name} — {text}")).small());
            }
        });
    ui.label(
        egui::RichText::new("Approximate model profile; not an observed sounding.")
            .weak()
            .small(),
    );
}

use radar_core::sounding::Sounding;
