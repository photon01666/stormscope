//! Android audio capture/playback for the Gemini Live voice session.
//!
//! The Java `GeminiLiveAudio` helper owns an `AudioRecord` (16 kHz mono
//! PCM16, voice-communication source with AEC/NS) and an `AudioTrack`
//! (24 kHz mono PCM16). This module polls captured chunks and pushes model
//! audio back; permission requests and engine lifecycle run on the Java main
//! thread, while the PCM queues themselves are thread-safe.

#![cfg(target_os = "android")]

use std::sync::OnceLock;

use android_activity::AndroidApp;

static ANDROID_APP: OnceLock<AndroidApp> = OnceLock::new();

pub fn register(app: AndroidApp) {
    let _ = ANDROID_APP.set(app);
}

/// Ask Java to initialize the mic/speaker and request RECORD_AUDIO if needed.
pub fn start() {
    post_void("stormscopeLiveAudioStart");
}

/// Start speaker-only output for a one-shot Gemini alert (no microphone).
pub fn start_playback() {
    post_void("stormscopeLiveAudioStartPlayback");
}

/// Stop and release the audio engine.
pub fn stop() {
    post_void("stormscopeLiveAudioStop");
}

/// One status snapshot: `idle`, `permission`, `ready`, or `error:<message>`.
pub fn state() -> String {
    call_string("stormscopeLiveAudioState").unwrap_or_else(|| "idle".into())
}

/// Drop any captured audio waiting in the queue (e.g. while the WebSocket
/// handshake runs).
pub fn flush_mic() {
    call_void("stormscopeLiveFlushMic");
}

/// Clear the playback queue and stop the current audio (barge-in).
pub fn flush_playback() {
    call_void("stormscopeLiveFlushPlayback");
}

/// Latest mic level, 0..1, for the footer equalizer.
pub fn level() -> f32 {
    let Some(app) = ANDROID_APP.get() else {
        return 0.0;
    };
    let vm_ptr = app.vm_as_ptr() as usize;
    let activity_ptr = app.activity_as_ptr() as usize;
    let vm = unsafe { jni::JavaVM::from_raw(vm_ptr as *mut jni::sys::JavaVM) };
    let res = vm.attach_current_thread(|env| {
        let activity =
            unsafe { jni::objects::JObject::from_raw(env, activity_ptr as jni::sys::jobject) };
        let value = env.call_method(
            &activity,
            jni::jni_str!("stormscopeLiveLevel"),
            jni::jni_sig!("()F"),
            &[],
        )?;
        value.f()
    });
    res.unwrap_or(0.0)
}

/// Pop one captured PCM chunk (or `None` when the queue is empty).
pub fn read_mic() -> Option<Vec<u8>> {
    let app = ANDROID_APP.get()?;
    let vm_ptr = app.vm_as_ptr() as usize;
    let activity_ptr = app.activity_as_ptr() as usize;
    let vm = unsafe { jni::JavaVM::from_raw(vm_ptr as *mut jni::sys::JavaVM) };
    let res = vm.attach_current_thread(|env| {
        let activity =
            unsafe { jni::objects::JObject::from_raw(env, activity_ptr as jni::sys::jobject) };
        let value = env.call_method(
            &activity,
            jni::jni_str!("stormscopeLiveReadMic"),
            jni::jni_sig!("()[B"),
            &[],
        )?;
        let obj = value.l()?;
        if obj.is_null() {
            return Ok::<Option<Vec<u8>>, jni::errors::Error>(None);
        }
        // SAFETY: the method's signature is `byte[]`; take ownership of the
        // local reference like the other bridges.
        let arr =
            unsafe { jni::objects::JByteArray::from_raw(env, obj.into_raw() as jni::sys::jarray) };
        let len = arr.len(env)?;
        let mut buf = vec![0i8; len];
        arr.get_region(env, 0, &mut buf)?;
        let bytes: Vec<u8> = buf.into_iter().map(|b| b as u8).collect();
        Ok(Some(bytes))
    });
    match res {
        Ok(bytes) => bytes,
        Err(e) => {
            log::warn!("live mic JNI failed: {e}");
            None
        }
    }
}

/// Queue 24 kHz mono PCM16 from the model for playback.
pub fn play(pcm: &[u8]) {
    let Some(app) = ANDROID_APP.get() else {
        return;
    };
    let vm_ptr = app.vm_as_ptr() as usize;
    let activity_ptr = app.activity_as_ptr() as usize;
    let pcm = pcm.to_vec();
    let vm = unsafe { jni::JavaVM::from_raw(vm_ptr as *mut jni::sys::JavaVM) };
    let res = vm.attach_current_thread(|env| {
        let activity =
            unsafe { jni::objects::JObject::from_raw(env, activity_ptr as jni::sys::jobject) };
        let array = env.byte_array_from_slice(&pcm)?;
        env.call_method(
            &activity,
            jni::jni_str!("stormscopeLivePlay"),
            jni::jni_sig!("([B)V"),
            &[jni::objects::JValue::Object(&array)],
        )?;
        Ok::<(), jni::errors::Error>(())
    });
    if let Err(e) = res {
        log::warn!("live playback JNI failed: {e}");
    }
}

fn call_string(method: &'static str) -> Option<String> {
    let app = ANDROID_APP.get()?;
    let vm_ptr = app.vm_as_ptr() as usize;
    let activity_ptr = app.activity_as_ptr() as usize;
    let vm = unsafe { jni::JavaVM::from_raw(vm_ptr as *mut jni::sys::JavaVM) };
    let res = vm.attach_current_thread(|env| {
        let activity =
            unsafe { jni::objects::JObject::from_raw(env, activity_ptr as jni::sys::jobject) };
        let method_name = jni::strings::JNIString::from(method);
        let value = env.call_method(
            &activity,
            &method_name,
            jni::jni_sig!("()Ljava/lang/String;"),
            &[],
        )?;
        let obj = value.l()?;
        if obj.is_null() {
            return Ok::<Option<String>, jni::errors::Error>(None);
        }
        let text = unsafe { jni::objects::JString::from_raw(env, obj.into_raw()) };
        Ok(Some(text.try_to_string(env)?))
    });
    match res {
        Ok(value) => value,
        Err(e) => {
            log::warn!("live audio {method} JNI failed: {e}");
            None
        }
    }
}

fn call_void(method: &'static str) {
    let Some(app) = ANDROID_APP.get() else {
        return;
    };
    let vm_ptr = app.vm_as_ptr() as usize;
    let activity_ptr = app.activity_as_ptr() as usize;
    let vm = unsafe { jni::JavaVM::from_raw(vm_ptr as *mut jni::sys::JavaVM) };
    let res = vm.attach_current_thread(|env| {
        let activity =
            unsafe { jni::objects::JObject::from_raw(env, activity_ptr as jni::sys::jobject) };
        let method_name = jni::strings::JNIString::from(method);
        env.call_method(&activity, &method_name, jni::jni_sig!("()V"), &[])?;
        Ok::<(), jni::errors::Error>(())
    });
    if let Err(e) = res {
        log::warn!("live audio {method} JNI failed: {e}");
    }
}

fn post_void(method: &'static str) {
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
            let method_name = jni::strings::JNIString::from(method);
            env.call_method(&activity, &method_name, jni::jni_sig!("()V"), &[])?;
            Ok::<(), jni::errors::Error>(())
        });
        if let Err(e) = res {
            log::warn!("live audio {method} JNI call failed: {e}");
        }
    }));
}
