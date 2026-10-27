//! Android device location (GPS / network) bridge.
//!
//! `MainActivity` owns a `LocationManager` listener and stores the latest fix
//! in a static field. This module polls that field over JNI and applies the
//! first fix: it centres the map on the user, selects the nearest radar site,
//! and points the NWS forecast at the user instead of the station. The map's
//! GPS chip can ask for a fresh fix at any time.
//!
//! JNI calls run on the UI thread; the *start/stop* calls are posted to the
//! Java main thread because `LocationManager` binds callbacks to the calling
//! thread's looper.

#![cfg(target_os = "android")]

use std::sync::OnceLock;

use crate::app::StormApp;
use crate::state::AppState;

static ANDROID_APP: OnceLock<android_activity::AndroidApp> = OnceLock::new();
static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How often to ask Java for a fix while one is outstanding.
const POLL_SECS: f64 = 2.0;

/// Remember the activity handle (called from `android_main`).
pub fn register(app: android_activity::AndroidApp) {
    let _ = ANDROID_APP.set(app);
}

/// Poll for a fix and, when one lands, apply it. Called at the top of the
/// frame on Android.
pub fn apply(ctx: &egui::Context, app: &mut StormApp) {
    if app.state.gps_applied {
        return;
    }
    let now = ctx.input(|i| i.time);
    if now - app.state.gps_polled_at < POLL_SECS {
        return;
    }
    app.state.gps_polled_at = now;
    let Some((lat, lon)) = read_fix() else {
        return;
    };
    apply_fix(app, lat, lon);
    stop_updates();
}

/// Ask for a fresh fix (the map's GPS chip). The last fix is discarded so the
/// result is a live one, not whatever was cached at launch.
pub fn request(state: &mut AppState) {
    state.gps_applied = false;
    state.gps_waiting = true;
    state.gps_polled_at = f64::NEG_INFINITY;
    call_void("request");
}

fn apply_fix(app: &mut StormApp, lat: f64, lon: f64) {
    app.state.gps_fix = Some((lat, lon));
    app.state.gps_applied = true;
    app.state.gps_waiting = false;

    if let Some(site) = radar_core::sites::nearest(lat, lon) {
        let km = radar_core::sites::haversine_km(lat, lon, site.latitude, site.longitude);
        log::info!(
            "GPS fix {lat:.4}, {lon:.4}; nearest radar {} ({} km)",
            site.id,
            km.round() as i64
        );
        // A site chosen with "Set default" outranks the automatic nearest
        // choice; without one, follow the GPS fix.
        if app.state.default_site.is_none() {
            app.select_site(site.id);
        }
    }

    // Show the user's own neighbourhood, not the radar's: the station can be
    // up to a couple hundred km away and still cover the fix.
    app.state.view.center_lat = lat;
    app.state.view.center_lon = lon;
    app.state.view.zoom = app.state.view.zoom.max(8.0);

    // The forecast follows the device unless the user has pinned a point.
    // A previous GPS point is replaced by the newer fix.
    if app.state.forecast_point.is_none() || app.state.forecast_point_gps {
        app.state.forecast_point = Some((lat, lon));
        app.state.forecast_point_gps = true;
        app.state.forecast = None;
        app.state.uv_index = None;
        app.state.observation = None;
        app.state.forecast_site.clear();
        app.state.forecast_requested = None;
        // The next frame's forecast window notices the moved target and
        // refetches; nothing to fetch while the window is closed.
    }
}

/// Read `MainActivity.stormscopeLocation()` — a `double[]` of lat/lon, or null
/// until a fix arrives.
fn read_fix() -> Option<(f64, f64)> {
    let app = ANDROID_APP.get()?;
    let vm_ptr = app.vm_as_ptr() as usize;
    let activity_ptr = app.activity_as_ptr() as usize;
    let vm = unsafe { jni::JavaVM::from_raw(vm_ptr as *mut jni::sys::JavaVM) };
    let res = vm.attach_current_thread(|env| {
        // SAFETY: an unowned global reference to the Activity, valid for the
        // lifetime of the AndroidApp (same pattern as android_ime).
        let activity =
            unsafe { jni::objects::JObject::from_raw(env, activity_ptr as jni::sys::jobject) };
        let value = env.call_method(
            &activity,
            jni::jni_str!("stormscopeLocation"),
            jni::jni_sig!("()[D"),
            &[],
        )?;
        let obj = value.l()?;
        if obj.is_null() {
            return Ok::<Option<(f64, f64)>, jni::errors::Error>(None);
        }
        // SAFETY: the method's signature is `double[]`; taking ownership of
        // the local reference from `JValueOwned`.
        let arr = unsafe {
            jni::objects::JDoubleArray::from_raw(env, obj.into_raw() as jni::sys::jarray)
        };
        if arr.len(env)? < 2 {
            return Ok(None);
        }
        let mut buf = [0.0f64; 2];
        arr.get_region(env, 0, &mut buf)?;
        Ok(Some((buf[0], buf[1])))
    });
    match res {
        Ok(fix) => fix,
        Err(e) => {
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                log::warn!("GPS JNI read failed: {e}");
            }
            None
        }
    }
}

/// Call `MainActivity.stormscopeRequestLocation()` / `…StopLocation()` on the
/// Java main thread (LocationManager needs a looper for its callbacks).
fn call_void(which: &'static str) {
    let Some(app) = ANDROID_APP.get() else {
        return;
    };
    let vm_ptr = app.vm_as_ptr() as usize;
    let activity_ptr = app.activity_as_ptr() as usize;
    app.run_on_java_main_thread(Box::new(move || {
        let vm = unsafe { jni::JavaVM::from_raw(vm_ptr as *mut jni::sys::JavaVM) };
        let res = vm.attach_current_thread(|env| {
            let activity =
                unsafe { jni::objects::JObject::from_raw(env, activity_ptr as jni::sys::jobject) };
            match which {
                "request" => {
                    env.call_method(
                        &activity,
                        jni::jni_str!("stormscopeRequestLocation"),
                        jni::jni_sig!("()V"),
                        &[],
                    )?;
                }
                _ => {
                    env.call_method(
                        &activity,
                        jni::jni_str!("stormscopeStopLocation"),
                        jni::jni_sig!("()V"),
                        &[],
                    )?;
                }
            }
            Ok::<(), jni::errors::Error>(())
        });
        if let Err(e) = res {
            log::warn!("GPS {which} call failed: {e}");
        }
    }));
}

fn stop_updates() {
    call_void("stop");
}
