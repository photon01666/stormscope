//! Android soft-keyboard (Gboard) → egui bridge.
//!
//! winit's Android backend can show and hide the IME but never forwards what
//! the user types into it. GameActivity, however, maintains an editor state
//! that the IME writes to (`AndroidApp::text_input_state()`). Stormscope polls
//! that once per frame, diffs it against the previous frame, and replays the
//! commits as egui `Event::Text` / `Backspace` / `Enter` events at the very
//! top of the frame — before the focused `TextEdit` runs — because egui
//! rebuilds its event queue from the new frame's raw input on every frame
//! boundary.

#![cfg(target_os = "android")]

use std::sync::OnceLock;

use crate::state::AppState;

static ANDROID_APP: OnceLock<android_activity::AndroidApp> = OnceLock::new();

/// Remember the GameActivity handle (called from `android_main` before
/// `eframe::run_native`).
pub fn register(app: android_activity::AndroidApp) {
    let _ = ANDROID_APP.set(app);
}

/// Hide (or restore) the system status bar for the chrome-free fullscreen
/// mode: the map then reaches the very top of the screen.
///
/// This calls the game-activity FFI directly: `AndroidApp::set_window_flags`
/// takes its internal lock for *writing*, but winit holds that lock for
/// *reading* across the whole frame — so taking the write lock here (inside
/// the frame) deadlocks the UI thread and freezes the app. The FFI itself
/// just posts the Java call to the main thread.
pub fn set_system_bars_hidden(hidden: bool) {
    const FLAG_FULLSCREEN: i32 = 0x0000_0400;
    let Some(app) = ANDROID_APP.get() else {
        return;
    };
    // `activity_as_ptr()` is the Java Activity's JNI handle, NOT the native
    // GameActivity struct the C API expects (passing it there dereferenced a
    // bogus pointer and crashed). Do the real thing: `Window.setFlags` via
    // JNI, posted to the Java main thread so the frame never blocks. (And
    // NOT `AndroidApp::set_window_flags`, whose write lock deadlocks when
    // taken from inside the frame.)
    // Raw pointers are not `Send`; carry them as integers into the callback.
    let vm_ptr = app.vm_as_ptr() as usize;
    let activity_ptr = app.activity_as_ptr() as usize;
    app.run_on_java_main_thread(Box::new(move || {
        let vm = unsafe { jni::JavaVM::from_raw(vm_ptr as *mut jni::sys::JavaVM) };
        let res = vm.attach_current_thread(|env| {
            // SAFETY: an unowned JNI global reference to the Activity, valid
            // for the lifetime of the AndroidApp.
            let activity =
                unsafe { jni::objects::JObject::from_raw(env, activity_ptr as jni::sys::jobject) };
            let window = env
                .call_method(
                    &activity,
                    jni::jni_str!("getWindow"),
                    jni::jni_sig!("()Landroid/view/Window;"),
                    &[],
                )?
                .l()?;
            env.call_method(
                &window,
                jni::jni_str!("setFlags"),
                jni::jni_sig!("(II)V"),
                &[
                    jni::objects::JValue::Int(if hidden { FLAG_FULLSCREEN } else { 0 }),
                    jni::objects::JValue::Int(FLAG_FULLSCREEN),
                ],
            )?;
            Ok::<(), jni::errors::Error>(())
        });
        if let Err(e) = res {
            log::warn!("system bar toggle failed: {e}");
        }
    }));
}

/// One short haptic tick for timeline scrubbing. The Java method performs the
/// platform haptic so it respects the user's haptic settings.
pub fn haptic_tick() {
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
                jni::jni_str!("stormscopeHapticTick"),
                jni::jni_sig!("()V"),
                &[],
            )?;
            Ok::<(), jni::errors::Error>(())
        });
        if let Err(e) = res {
            log::debug!("haptic tick failed: {e}");
        }
    }));
}

/// Height of the soft keyboard in physical pixels (0 when it is closed).
/// GameActivity receives the IME insets; MainActivity records the latest one
/// and this reads it back for the AI window's bottom inset.
pub fn ime_height_px() -> i32 {
    let Some(app) = ANDROID_APP.get() else {
        return 0;
    };
    let vm_ptr = app.vm_as_ptr() as usize;
    let activity_ptr = app.activity_as_ptr() as usize;
    let vm = unsafe { jni::JavaVM::from_raw(vm_ptr as *mut jni::sys::JavaVM) };
    let res = vm.attach_current_thread(|env| {
        // SAFETY: an unowned global reference to the Activity, valid for the
        // lifetime of the AndroidApp (same pattern as `set_system_bars_hidden`).
        let activity =
            unsafe { jni::objects::JObject::from_raw(env, activity_ptr as jni::sys::jobject) };
        let value = env.call_method(
            &activity,
            jni::jni_str!("stormscopeImeInset"),
            jni::jni_sig!("()I"),
            &[],
        )?;
        value.i()
    });
    res.unwrap_or(0)
}

/// Replay what Gboard committed since the previous frame into egui's event
/// queue, and keep the IME visible exactly while a widget has focus.
pub fn pump(ctx: &egui::Context, state: &mut AppState) {
    let Some(app) = ANDROID_APP.get() else {
        return;
    };

    let focused = ctx.memory(|m| m.focused().is_some());
    if focused {
        // Keep polling the IME state at display rate while a field is focused:
        // egui otherwise only repaints on its ~1 Hz age/latency ticks, and a
        // Gboard commit would sit in the queue until then — that reads as
        // typing lag. This keeps the frame loop alive only while typing.
        ctx.request_repaint();
    }
    if focused != state.ime_was_focused {
        if focused {
            // Seed Gboard's editor with the field's current text when we know
            // it (the marker label and AI composer mirror theirs into
            // `text_focus_text`); other fields start from an empty buffer and
            // commits simply append at egui's cursor.
            let text = state
                .text_focus
                .then(|| state.text_focus_text.clone())
                .unwrap_or_default();
            app.set_text_input_state(android_activity::input::TextInputState {
                text: text.clone(),
                selection: android_activity::input::TextSpan {
                    start: text.len(),
                    end: text.len(),
                },
                compose_region: None,
            });
            app.show_soft_input(true);
            state.ime_last_text = Some(text);
        } else {
            app.hide_soft_input(false);
            state.ime_last_text = None;
        }
        state.ime_was_focused = focused;
        return;
    }

    if !focused {
        return;
    }

    let current = app.text_input_state();
    let new_text = current.text;
    let Some(prev) = state.ime_last_text.clone() else {
        state.ime_last_text = Some(new_text);
        return;
    };

    let mut events: Vec<egui::Event> = Vec::new();

    if new_text != prev {
        // Longest common prefix and suffix: everything in between is what the
        // IME changed — an insertion, a deletion, or a replacement.
        let p = prev
            .bytes()
            .zip(new_text.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        let max_s = prev.len().min(new_text.len()).saturating_sub(p);
        let s = prev
            .bytes()
            .rev()
            .zip(new_text.bytes().rev())
            .take_while(|(a, b)| a == b)
            .count()
            .min(max_s);
        let deleted = prev.len().saturating_sub(p + s);
        let inserted = &new_text[p..new_text.len().saturating_sub(s)];
        for _ in 0..deleted {
            events.push(egui::Event::Key {
                key: egui::Key::Backspace,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
        if !inserted.is_empty() {
            events.push(egui::Event::Text(inserted.to_string()));
        }
        state.ime_last_text = Some(new_text);
    } else if state.text_focus && state.text_focus_text != prev {
        // The field changed without the IME (Enter commit, clear all…): keep
        // Gboard's buffer in step so the next diff stays sane.
        let text = state.text_focus_text.clone();
        app.set_text_input_state(android_activity::input::TextInputState {
            text: text.clone(),
            selection: android_activity::input::TextSpan {
                start: text.len(),
                end: text.len(),
            },
            compose_region: None,
        });
        state.ime_last_text = Some(text);
    }

    // The IME's editor action key (Done / Go / …) is not observable through
    // the text state; it may arrive as a plain Enter key event via the input
    // pipeline, which egui handles on its own.

    if !events.is_empty() {
        ctx.input_mut(|i| i.events.extend(events));
    }
}
