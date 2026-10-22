package com.stormscope.app;

import com.google.androidgamesdk.GameActivity;

/**
 * GameActivity owns the window, surface, lifecycle and IME plumbing.
 *
 * The Rust glue (libstormscope_android.so, from crates/stormscope-android)
 * registers itself here and runs android_main on its own thread.
 */
public class MainActivity extends GameActivity {
    static {
        System.loadLibrary("stormscope_android");
    }
}
