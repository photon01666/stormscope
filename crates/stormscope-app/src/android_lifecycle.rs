//! Android foreground/background lifecycle signals for the native UI.
//!
//! GameActivity keeps the Rust event loop alive while the app is paused. The
//! live poller may have been frozen or disconnected in the background, so the
//! first frame after `onResume` asks the data thread to catch the radar up
//! instead of leaving the spotter on an old scan.

#![cfg(target_os = "android")]

use std::sync::OnceLock;

use android_activity::AndroidApp;

static ANDROID_APP: OnceLock<AndroidApp> = OnceLock::new();

/// Remember the GameActivity handle (called from `android_main`).
pub fn register(app: AndroidApp) {
    let _ = ANDROID_APP.set(app);
}

/// Hand an APK to Android's package installer.
pub fn install_apk(path: &std::path::Path) {
    let Some(app) = ANDROID_APP.get() else {
        return;
    };
    let path = path.to_owned();
    let vm_ptr = app.vm_as_ptr() as usize;
    let activity_ptr = app.activity_as_ptr() as usize;
    app.run_on_java_main_thread(Box::new(move || {
        let vm = unsafe { jni::JavaVM::from_raw(vm_ptr as *mut jni::sys::JavaVM) };
        let res = vm.attach_current_thread(|env| {
            let activity =
                unsafe { jni::objects::JObject::from_raw(env, activity_ptr as jni::sys::jobject) };
            let jpath = env.new_string(path.to_string_lossy().as_ref())?;
            env.call_method(
                &activity,
                jni::jni_str!("stormscopeInstallApk"),
                jni::jni_sig!("(Ljava/lang/String;)V"),
                &[jni::objects::JValue::Object(&jpath)],
            )?;
            Ok::<(), jni::errors::Error>(())
        });
        if let Err(error) = res {
            log::warn!("APK install failed to start: {error}");
        }
    }));
}

/// One-shot: the app returned to the foreground after a pause.
pub fn take_resume_refresh() -> bool {
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
            jni::jni_str!("stormscopeTakeResumeRefresh"),
            jni::jni_sig!("()Z"),
            &[],
        )?;
        value.z()
    });
    res.unwrap_or(false)
}
