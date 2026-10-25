//! Android entry point for Stormscope.
//!
//! This crate is only the Android shell: it initialises logging and the app's
//! private data directories, then hands control to the same `StormApp` the
//! desktop binary uses. `android-activity` calls `android_main` on its own
//! thread once the `GameActivity` has been created; the desktop `main` is not
//! used on Android.

#![cfg(target_os = "android")]

use eframe::egui;

/// GameActivity's native entry point.
///
/// Uses the Rust ABI (not `extern "C"`) as required by `android-activity`.
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    // There is no terminal on Android; send `log` to logcat (`adb logcat -s
    // stormscope`). This must happen before anything logs.
    android_logger::init_once(
        android_logger::Config::default()
            .with_tag("stormscope")
            .with_max_level(log::LevelFilter::Info),
    );

    // Android has no `$HOME` or XDG directories. Point the existing cache and
    // config code at the app's private storage before anything touches it
    // (DiskCache, AI settings, markers, the panic log). `internal_data_path`
    // is `/data/user/0/<package>/files`, per the android-activity docs.
    if let Some(data) = app.internal_data_path() {
        let cache = data.join("cache");
        let config = data.join("config");
        let _ = std::fs::create_dir_all(&cache);
        let _ = std::fs::create_dir_all(&config);
        // SAFETY: this runs first on the app's own thread, before any other
        // thread can read the environment.
        unsafe {
            std::env::set_var("XDG_CACHE_HOME", &cache);
            std::env::set_var("XDG_CONFIG_HOME", &config);
            std::env::set_var("HOME", &data);
        }
        log::info!(
            "Stormscope {} starting; data dir {}",
            env!("CARGO_PKG_VERSION"),
            data.display()
        );
    } else {
        log::warn!("no internal data path; cache/config will use the CWD");
    }

    // rustls-platform-verifier panics on Android unless it is given the JVM
    // and the Activity context up front (its `global()` is empty otherwise).
    // Doing this here, before eframe::run_native spawns the data thread, is
    // what lets S3 / NOAA / MRMS fetches verify TLS. The bundled webpki roots
    // are not enough on their own: nexrad-data pulls the platform verifier in
    // transitively through reqwest's rustls-tls feature.
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr() as *mut jni::sys::JavaVM) };
    let init_result = vm.attach_current_thread(|env| {
        let context = unsafe {
            jni::objects::JObject::from_raw(env, app.activity_as_ptr() as jni::sys::jobject)
        };
        match rustls_platform_verifier::android::init_with_env(env, context) {
            Ok(()) => log::info!("rustls platform verifier initialised"),
            Err(e) => log::warn!("rustls platform verifier init failed: {e:?} (TLS may not work)"),
        }
        Ok::<(), jni::errors::Error>(())
    });
    if let Err(e) = init_result {
        log::warn!("rustls platform verifier attach failed: {e}");
    }

    // Gboard → egui: GameActivity keeps an editor state the IME writes to
    // (`AndroidApp::text_input_state`), and winit never forwards IME text, so
    // the app polls that state and replays commits as egui events. Register
    // the activity handle for the bridge before eframe takes `app`.
    stormscope_app::android_ime::register(app.clone());
    stormscope_app::android_lifecycle::register(app.clone());
    stormscope_app::android_live_audio::register(app.clone());
    stormscope_app::android_location::register(app.clone());
    stormscope_app::android_storm_watch::register(app.clone());
    stormscope_app::android_tts::register(app.clone());

    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Stormscope")
            .with_fullscreen(true),
        android_app: Some(app),
        ..Default::default()
    };
    // Match the desktop's vsync pin; AutoVsync renegotiates on display changes.
    options.wgpu_options.surface.present_mode = eframe::egui_wgpu::wgpu::PresentMode::Fifo;

    if let Err(e) = eframe::run_native(
        "Stormscope",
        options,
        Box::new(|cc| Ok(Box::new(stormscope_app::StormApp::new(cc)))),
    ) {
        log::error!("Stormscope failed to start: {e}");
    }
}
