//! Android foreground/background lifecycle signals for the native UI.
//!
//! GameActivity keeps the Rust event loop alive while the app is paused. The
//! live poller may have been frozen or disconnected in the background, so the
//! first frame after `onResume` asks the data thread to catch the radar up
//! instead of leaving the spotter on an old scan.

#![cfg(target_os = "android")]

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use android_activity::AndroidApp;

static ANDROID_APP: OnceLock<AndroidApp> = OnceLock::new();

/// Whether the activity is currently visible. The data thread checks this
/// before draining the live poller; while the app is backgrounded the poller
/// is dropped so it cannot keep downloading radar scans.
static FOREGROUND: AtomicBool = AtomicBool::new(true);

/// Remember the GameActivity handle (called from `android_main`).
pub fn register(app: AndroidApp) {
    let _ = ANDROID_APP.set(app);
}

/// Called by `MainActivity` from `onPause`/`onResume` through JNI.
pub fn set_foreground(foreground: bool) {
    FOREGROUND.store(foreground, Ordering::Relaxed);
}

/// True while the activity is resumed and visible.
pub fn is_foreground() -> bool {
    FOREGROUND.load(Ordering::Relaxed)
}

/// Does Android allow this app to request package installs?
pub fn can_install_packages() -> bool {
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
            jni::jni_str!("stormscopeCanInstallPackages"),
            jni::jni_sig!("()Z"),
            &[],
        )?;
        value.z()
    });
    res.unwrap_or(false)
}

/// Open the system "install unknown apps" page for Stormscope.
pub fn open_install_settings() {
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
                jni::jni_str!("stormscopeOpenInstallSettings"),
                jni::jni_sig!("()V"),
                &[],
            )?;
            Ok::<(), jni::errors::Error>(())
        });
        if let Err(error) = res {
            log::warn!("opening install settings failed: {error}");
        }
    }));
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
