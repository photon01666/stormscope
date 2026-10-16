//! Rain & snow totals window: what nearby NWS stations actually measured,
//! as an event-facing view of the observation *series* — the rolling
//! buckets each station reports plus an hourly-summed accumulation over the
//! loaded window (which is how "after the rain" totals are reconstructed
//! from hourly observations without double-counting denser cadences).
//!
//! Data comes from `radar_data::ForecastSource::station_totals` (the same
//! `/points` machinery the forecast window uses); this module is the
//! presentation and the refresh bookkeeping.

use crate::app::StormApp;

/// The window (no-op while closed).
pub fn totals_window(ctx: &egui::Context, app: &mut StormApp) {
    if !app.state.totals_open {
        return;
    }
    // Keep the staleness timer alive while nothing else animates, like the
    // forecast window does.
    ctx.request_repaint_after(std::time::Duration::from_secs(30));
    request_totals_if_stale(app);

    let scale = app.sidebar_scale.max(1.0);
    let size = super::popup_size(ctx, scale, 560.0, 500.0);
    let max_h = super::popup_max_height(ctx, scale);
    let mut open = app.state.totals_open;
    let site_id = app.state.site_id.clone();

    egui::Window::new("Rain & snow totals")
        .id(egui::Id::new("stormscope.totals"))
        .open(&mut open)
        .default_size(size)
        .max_height(max_h)
        .min_size(egui::vec2(420.0 * scale, 220.0 * scale))
        // Centered by its top-left corner (see the other popups: a pivot
        // fights the resize drag).
        .default_pos(super::centered_popup_pos(ctx, size))
        .resizable(true)
        .show(ctx, |ui| {
            super::apply_text_scale(ui, scale);
            ui.label(
                egui::RichText::new(format!(
                    "NWS station observations reporting around {site_id}"
                ))
                .weak()
                .small(),
            );

            if app.state.totals.is_empty() {
                let msg = if app.state.totals_fetched_at.is_none() {
                    "loading station totals…"
                } else {
                    "no observation series available for the nearest stations"
                };
                ui.label(egui::RichText::new(msg).weak());
            } else {
                let scroll_h = (ui.available_height() - 34.0 * scale).max(120.0);
                egui::ScrollArea::vertical()
                    .max_height(scroll_h)
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                    .show(ui, |ui| {
                        for t in &app.state.totals {
                            station_block(ui, t, scale);
                        }
                    });
            }
            ui.label(
                egui::RichText::new(
                    "source: api.weather.gov — NWS station observations (public data)",
                )
                .weak()
                .small(),
            );
            super::fill_popup_height(ui);
        });
    app.state.totals_open = open;
}

/// One station: name/id, observation time, the rolling buckets, then the
/// event-facing hourly sums.
fn station_block(ui: &mut egui::Ui, t: &radar_data::StationTotals, scale: f32) {
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(&t.station_name).strong());
        ui.label(
            egui::RichText::new(if t.station_name == t.station_id {
                String::new()
            } else {
                t.station_id.clone()
            })
            .weak()
            .small(),
        );
    });
    ui.label(
        egui::RichText::new(match t.last_obs {
            Some(ts) => format!(
                "observed {} · {} observations loaded",
                crate::timefmt::fmt_central(&ts),
                t.obs_count
            ),
            None => "no observations loaded".to_string(),
        })
        .weak()
        .small(),
    );
    if t.buckets.is_empty() && t.hourly_sums.is_empty() {
        ui.label(
            egui::RichText::new("this station's series carries no rain or snow fields")
                .weak()
                .small(),
        );
    }
    for (label, inches) in &t.buckets {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(format!("{inches:.2} in")).monospace());
            });
        });
    }
    for (label, inches) in &t.hourly_sums {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{label} · {:.1} h window", t.window_hours)).strong(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!("{inches:.2} in"))
                        .monospace()
                        .strong(),
                );
            });
        });
    }
    ui.add_space(4.0 * scale);
    ui.separator();
}

/// Send a totals request unless one already covers this target — with a
/// 60-second retry window, so a failed fetch retries on the next visit
/// instead of either sticking forever or hammering the API.
pub(super) fn request_totals_if_stale(app: &mut StormApp) {
    let Some(site) = radar_core::sites::site(&app.state.site_id) else {
        return;
    };
    let (lat, lon) = (site.latitude, site.longitude);
    let same_target = |(a, b, _): (f64, f64, std::time::Instant)| {
        (a - lat).abs() < 1e-6 && (b - lon).abs() < 1e-6
    };
    if let Some(req) = app.state.totals_requested {
        if same_target(req) && req.2.elapsed().as_secs_f64() < 60.0 {
            return; // in flight (or recently failed and due a retry)
        }
    }
    let age = app
        .state
        .totals_fetched_at
        .map(|t| t.elapsed().as_secs_f64());
    let target_matches = app
        .state
        .totals_target
        .is_some_and(|(tlat, tlon)| (tlat - lat).abs() < 1e-6 && (tlon - lon).abs() < 1e-6);
    // Same freshness rule the forecast window uses: fetch when nothing has
    // loaded, the target moved, or the loaded data aged past the cadence.
    if !super::forecast::forecast_needs_refresh(
        age,
        target_matches,
        app.state.totals_fetched_at.is_some(),
    ) {
        return;
    }
    app.state.totals_requested = Some((lat, lon, std::time::Instant::now()));
    let _ = app.tx.send(crate::app::DataCmd::LoadTotals(lat, lon));
}

#[cfg(test)]
mod tests {
    /// Freshness follows the same shape as the forecast window: fetch when
    /// nothing is loaded, the target moved, or the data aged out; the
    /// in-flight window suppresses repeat requests in between.
    #[test]
    fn totals_refresh_needs_follows_target_and_clock() {
        // Nothing loaded yet.
        assert!(super::super::forecast::forecast_needs_refresh(
            None, true, false
        ));
        // Fresh for this target.
        assert!(!super::super::forecast::forecast_needs_refresh(
            Some(10.0),
            true,
            true
        ));
        // Aged past the cadence.
        assert!(super::super::forecast::forecast_needs_refresh(
            Some(super::super::forecast::FORECAST_REFRESH_SECS + 1.0),
            true,
            true
        ));
        // Different target always refetches.
        assert!(super::super::forecast::forecast_needs_refresh(
            Some(1.0),
            false,
            true
        ));
    }
}
