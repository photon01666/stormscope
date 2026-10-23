//! Android text-to-speech bridge.
//!
//! `MainActivity` owns the system `TextToSpeech` engine and its ready/pending
//! queue. This module posts announcements to the Java main thread, because
//! TTS calls (and their callbacks) are safest there and the Rust voice worker
//! is not a Looper thread.

#![cfg(target_os = "android")]

use std::sync::OnceLock;

static ANDROID_APP: OnceLock<android_activity::AndroidApp> = OnceLock::new();

/// Remember the GameActivity handle (called from `android_main`).
pub fn register(app: android_activity::AndroidApp) {
    let _ = ANDROID_APP.set(app);
}

/// Queue one announcement with the Android system TTS engine.
pub fn speak(text: &str) {
    let Some(app) = ANDROID_APP.get() else {
        return;
    };
    let text = text.to_string();
    let vm_ptr = app.vm_as_ptr() as usize;
    let activity_ptr = app.activity_as_ptr() as usize;
    app.run_on_java_main_thread(Box::new(move || {
        let vm = unsafe { jni::JavaVM::from_raw(vm_ptr as *mut jni::sys::JavaVM) };
        let res = vm.attach_current_thread(|env| {
            // SAFETY: an unowned global reference to the Activity, valid for
            // the lifetime of the AndroidApp (same pattern as android_ime).
            let activity =
                unsafe { jni::objects::JObject::from_raw(env, activity_ptr as jni::sys::jobject) };
            let text = env.new_string(&text)?;
            env.call_method(
                &activity,
                jni::jni_str!("stormscopeSpeak"),
                jni::jni_sig!("(Ljava/lang/String;)V"),
                &[jni::objects::JValue::Object(&text)],
            )?;
            Ok::<(), jni::errors::Error>(())
        });
        if let Err(e) = res {
            log::warn!("TTS JNI call failed: {e}");
        }
    }));
}
