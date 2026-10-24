//! Android opt-in background Storm Watch bridge.
//!
//! The foreground service and the persistent notification live in
//! `StormWatchService`; this module only reads/writes its SharedPreferences
//! toggle from the Rust settings UI. Calls that start/stop the service are
//! posted to the Java main thread.

#![cfg(target_os = "android")]

use std::sync::OnceLock;

use android_activity::AndroidApp;

static ANDROID_APP: OnceLock<AndroidApp> = OnceLock::new();

pub fn register(app: AndroidApp) {
    let _ = ANDROID_APP.set(app);
}

/// The persisted Storm Watch toggle. Called from the UI thread.
pub fn enabled() -> bool {
    let Some(app) = ANDROID_APP.get() else {
        return false;
    };
    let vm_ptr = app.vm_as_ptr() as usize;
    let activity_ptr = app.activity_as_ptr() as usize;
    let vm = unsafe { jni::JavaVM::from_raw(vm_ptr as *mut jni::sys::JavaVM) };
    let res = vm.attach_current_thread(|env| {
        let activity =
            unsafe { jni::objects::JObject::from_raw(env, activity_ptr as jni::sys::jobject) };
        let value = env.call_method(
            &activity,
            jni::jni_str!("stormscopeStormWatchEnabled"),
            jni::jni_sig!("()Z"),
            &[],
        )?;
        value.z()
    });
    res.unwrap_or(false)
}

/// Start or stop the background service; persists the preference in Java.
pub fn set_enabled(enabled: bool) {
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
            env.call_method(
                &activity,
                jni::jni_str!("stormscopeSetStormWatch"),
                jni::jni_sig!("(Z)V"),
                &[jni::objects::JValue::Bool(enabled)],
            )?;
            Ok::<(), jni::errors::Error>(())
        });
        if let Err(e) = res {
            log::warn!("Storm Watch JNI toggle failed: {e}");
        }
    }));
}
