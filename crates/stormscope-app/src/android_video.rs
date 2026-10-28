//! Android video handoff.
//!
//! YouTube rejects its embedded player in Android WebViews with a
//! "Sign in to confirm you're not a bot" page. The installed YouTube app is
//! the reliable player, so the feed opens the video there (falling back to the
//! browser when YouTube is not installed).

#![cfg(target_os = "android")]

use std::sync::OnceLock;

use android_activity::AndroidApp;

static ANDROID_APP: OnceLock<AndroidApp> = OnceLock::new();

pub fn register(app: AndroidApp) {
    let _ = ANDROID_APP.set(app);
}

/// Open a YouTube video by id in the YouTube app or the default browser.
pub fn open(video_id: &str) -> Result<(), String> {
    if !(6..=24).contains(&video_id.len())
        || !video_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err("YouTube returned an invalid video id".into());
    }
    let Some(app) = ANDROID_APP.get() else {
        return Err("Android video bridge is not ready".into());
    };
    let video_id = video_id.to_string();
    let vm_ptr = app.vm_as_ptr() as usize;
    let activity_ptr = app.activity_as_ptr() as usize;
    app.run_on_java_main_thread(Box::new(move || {
        let vm = unsafe { jni::JavaVM::from_raw(vm_ptr as *mut jni::sys::JavaVM) };
        let result = vm.attach_current_thread(|env| {
            let activity =
                unsafe { jni::objects::JObject::from_raw(env, activity_ptr as jni::sys::jobject) };
            let video_id = env.new_string(&video_id)?;
            env.call_method(
                &activity,
                jni::jni_str!("stormscopeOpenYouTube"),
                jni::jni_sig!("(Ljava/lang/String;)V"),
                &[jni::objects::JValue::Object(&video_id)],
            )?;
            Ok::<(), jni::errors::Error>(())
        });
        if let Err(error) = result {
            log::warn!("opening YouTube failed: {error}");
        }
    }));
    Ok(())
}
