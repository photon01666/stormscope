package com.stormscope.app;

import android.Manifest;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.location.Location;
import android.location.LocationListener;
import android.location.LocationManager;
import android.media.AudioAttributes;
import android.os.Build;
import android.os.Bundle;
import android.speech.tts.TextToSpeech;
import android.util.Log;
import android.view.View;

import java.util.ArrayDeque;
import java.util.Locale;

import androidx.core.view.WindowInsetsCompat;

import com.google.androidgamesdk.GameActivity;

/**
 * GameActivity owns the window, surface, lifecycle and IME plumbing.
 *
 * The Rust glue (libstormscope_android.so, from crates/stormscope-android)
 * registers itself here and runs android_main on its own thread.
 *
 * We also keep the latest GPS/network fix in a static field. The Rust side
 * polls {@link #stormscopeLocation()} once the first frame is up, centres the
 * map on it, selects the nearest radar and points the forecast at it.
 */
public class MainActivity extends GameActivity {
    static {
        System.loadLibrary("stormscope_android");
    }

    private static final int LOCATION_REQUEST = 0x4750; // "GP"
    private static final int NOTIFICATION_REQUEST = 0x4751; // "GP" + 1
    private static final int AUDIO_REQUEST = 0x4752; // "GP" + 2
    /** Intent extra: the Storm Watch notification was tapped. */
    public static final String EXTRA_AI_BRIEF = "stormscope_ai_brief";
    /** Intent extra: the tapped alert notification's NWS id. */
    public static final String EXTRA_ALERT_ID = "stormscope_alert_id";

    /** One-shot flag parked by the notification, consumed by Rust's UI loop. */
    private static volatile boolean sPendingAiBrief;
    private static volatile String sPendingAlertId;
    /** Set when the app returns to the foreground after being paused. */
    private static volatile boolean sPendingResumeRefresh;
    private static boolean sHasResumed;

    /** Latest fix as {lat, lon}; volatile because Rust reads it on its own thread. */
    private static volatile double[] sLocation;

    private static LocationManager sManager;
    private static LocationListener sListener;
    private static boolean sPermissionAsked;
    private static SharedPreferences sPrefs;
    private static volatile boolean sForeground;

    /** Soft-keyboard height in px, straight from GameActivity's IME insets. */
    private static volatile int sImeInset;

    private static final String TAG = "stormscope";
    private static final int TTS_PENDING_MAX = 8;
    /** Android's system TTS engine; announcements are queued until it is ready. */
    private static volatile TextToSpeech sTts;
    private static volatile boolean sTtsReady;
    private static final ArrayDeque<String> sTtsPending = new ArrayDeque<>();

    /** RECORD_AUDIO is being requested for the Gemini Live session. */
    private static volatile boolean sLivePermissionPending;
    /** A live session is waiting for the permission dialog to close. */
    private static volatile boolean sLivePendingStart;
    /** One-shot audio error for the Rust session thread. */
    private static volatile String sLiveError;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        sPendingAiBrief = getIntent() != null
                && getIntent().getBooleanExtra(EXTRA_AI_BRIEF, false);
        sPendingAlertId = getIntent() == null ? null : getIntent().getStringExtra(EXTRA_ALERT_ID);
        sPrefs = getSharedPreferences("stormscope", MODE_PRIVATE);
        startLocation(false);
        initTts();
        if (sPrefs.getBoolean("storm_watch_enabled", false)) {
            try {
                StormWatchService.start(this);
            } catch (Exception e) {
                Log.w(TAG, "Storm Watch could not start: " + e);
            }
        }
    }

    @Override
    protected void onResume() {
        super.onResume();
        sForeground = true;
        // The first onResume is the launch itself. Later ones mean the app was
        // paused in the background; Rust uses this to catch the live radar up.
        if (sHasResumed) {
            sPendingResumeRefresh = true;
        }
        sHasResumed = true;
        GeminiLiveAudio.resumeCapture();
    }

    @Override
    protected void onPause() {
        sForeground = false;
        // Free the microphone as soon as the app leaves the foreground; the
        // Gemini Live WebSocket can stay connected for a short background
        // spell, and onResume restarts capture.
        GeminiLiveAudio.pauseCapture();
        super.onPause();
    }

    /** singleTask: notification taps arrive here when the app is alive. */
    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        if (intent != null && intent.getBooleanExtra(EXTRA_AI_BRIEF, false)) {
            sPendingAiBrief = true;
        }
        sPendingAlertId = intent == null ? null : intent.getStringExtra(EXTRA_ALERT_ID);
    }

    /**
     * Called from Rust (JNI, UI thread): consume the tapped alert notification's
     * id once, so the app can open that alert's detail.
     */
    public String stormscopeTakeAlertId() {
        String id = sPendingAlertId;
        sPendingAlertId = null;
        return id;
    }

    /**
     * Called from Rust (JNI, UI thread): consume the persistent notification's
     * tap request exactly once and return whether it was pending. Rust opens
     * the forecast page for it.
     */
    public boolean stormscopeTakeAiBriefRequest() {
        boolean pending = sPendingAiBrief;
        sPendingAiBrief = false;
        return pending;
    }

    /**
     * Called from Rust (JNI, UI thread): consume the "returned to foreground"
     * flag exactly once. Rust then refreshes the live radar if it is stale.
     */
    public boolean stormscopeTakeResumeRefresh() {
        boolean pending = sPendingResumeRefresh;
        sPendingResumeRefresh = false;
        return pending;
    }

    /** True while the game window is resumed; the service stays silent then. */
    public static boolean isAppForeground() {
        return sForeground;
    }

    @Override
    protected void onDestroy() {
        // A configuration change recreates this Activity in the same process;
        // the native eframe thread must survive that. A real destroy (swiped
        // from Recents, Back, task removed) leaves the old native event loop
        // and its background threads alive in a process with no window, which
        // makes the next launch hang on a black screen. End the process so the
        // next open starts a fresh native loop.
        boolean recreated = isChangingConfigurations();
        sLivePendingStart = false;
        sLivePermissionPending = false;
        GeminiLiveAudio.stop();
        TextToSpeech tts = sTts;
        sTts = null;
        sTtsReady = false;
        if (tts != null) {
            try {
                tts.stop();
                tts.shutdown();
            } catch (Exception ignored) {
            }
        }
        if (!recreated) {
            // Do this before GameActivity.onDestroy: that call blocks in
            // terminateNativeCode waiting for the Rust ALooper thread, which
            // never exits on a task removal — the old code ANR'd here and the
            // next launch hung on a black screen.
            Log.i(TAG, "MainActivity destroyed; exiting process for a clean relaunch");
            android.os.Process.killProcess(android.os.Process.myPid());
            return;
        }
        super.onDestroy();
    }

    @Override
    public WindowInsetsCompat onApplyWindowInsets(View view, WindowInsetsCompat insets) {
        sImeInset = insets.getInsets(WindowInsetsCompat.Type.ime()).bottom;
        return super.onApplyWindowInsets(view, insets);
    }

    /** Called from Rust (JNI, UI thread): soft-keyboard height in px. */
    public int stormscopeImeInset() {
        return sImeInset;
    }

    /** Set up the system TTS engine once; Rust queues announcements until ready. */
    private void initTts() {
        if (sTts != null) {
            return;
        }
        sTtsReady = false;
        try {
            sTts = new TextToSpeech(this, status -> {
                if (status != TextToSpeech.SUCCESS) {
                    Log.w(TAG, "text-to-speech init failed: " + status);
                    return;
                }
                TextToSpeech tts = sTts;
                if (tts == null) {
                    return;
                }
                int language = tts.setLanguage(Locale.US);
                if (language == TextToSpeech.LANG_MISSING_DATA
                        || language == TextToSpeech.LANG_NOT_SUPPORTED) {
                    Log.w(TAG, "US English TTS voice unavailable; using engine default");
                }
                tts.setSpeechRate(1.0f);
                tts.setPitch(1.0f);
                tts.setAudioAttributes(new AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_ALARM)
                        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                        .build());
                synchronized (sTtsPending) {
                    sTtsReady = true;
                    while (!sTtsPending.isEmpty()) {
                        speakNow(sTtsPending.removeFirst());
                    }
                }
                Log.i(TAG, "text-to-speech ready");
            });
        } catch (Exception e) {
            sTts = null;
            Log.w(TAG, "text-to-speech unavailable: " + e);
        }
    }

    /**
     * Called from Rust (JNI, Java main thread): queue an announcement. Text
     * arriving before the engine finishes initializing is held in a short
     * bounded queue and flushed on init.
     */
    public void stormscopeSpeak(String text) {
        if (text == null || text.trim().isEmpty()) {
            return;
        }
        synchronized (sTtsPending) {
            if (!sTtsReady || sTts == null) {
                if (sTtsPending.size() >= TTS_PENDING_MAX) {
                    sTtsPending.removeFirst();
                }
                sTtsPending.addLast(text);
                return;
            }
            speakNow(text);
        }
    }

    private static void speakNow(String text) {
        TextToSpeech tts = sTts;
        if (tts == null) {
            return;
        }
        int result = tts.speak(
                text,
                TextToSpeech.QUEUE_ADD,
                null,
                "stormscope-" + System.nanoTime());
        if (result != TextToSpeech.SUCCESS) {
            Log.w(TAG, "text-to-speech speak failed: " + result);
        }
    }

    /**
     * Called from Rust (JNI, Java main thread): initialize the Gemini Live
     * microphone/speaker, asking for RECORD_AUDIO the first time. The Rust
     * session thread polls {@link #stormscopeLiveAudioState()} until ready.
     */
    public void stormscopeLiveAudioStart() {
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO)
                != PackageManager.PERMISSION_GRANTED) {
            sLivePendingStart = true;
            sLivePermissionPending = true;
            requestPermissions(
                    new String[]{Manifest.permission.RECORD_AUDIO},
                    AUDIO_REQUEST);
            return;
        }
        GeminiLiveAudio.start();
        if (!GeminiLiveAudio.isRunning()) {
            sLiveError = GeminiLiveAudio.lastError();
        }
    }

    /** Called from Rust (JNI): `idle`, `permission`, `ready` or `error:<msg>`. */
    public String stormscopeLiveAudioState() {
        if (sLivePermissionPending) {
            return "permission";
        }
        String error = sLiveError;
        if (error != null) {
            sLiveError = null;
            return "error:" + error;
        }
        return GeminiLiveAudio.isRunning() ? "ready" : "idle";
    }

    /** Called from Rust (JNI, Java main thread): release mic and speaker. */
    public void stormscopeLiveAudioStop() {
        sLivePendingStart = false;
        sLivePermissionPending = false;
        GeminiLiveAudio.stop();
    }

    /** Called from Rust: pop one captured 16 kHz PCM16 chunk, or null. */
    public byte[] stormscopeLiveReadMic() {
        return GeminiLiveAudio.readMic();
    }

    /** Called from Rust: queue 24 kHz PCM16 model audio for playback. */
    public void stormscopeLivePlay(byte[] pcm) {
        GeminiLiveAudio.play(pcm);
    }

    /** Called from Rust: drop captured audio waiting in the queue. */
    public void stormscopeLiveFlushMic() {
        GeminiLiveAudio.flushMic();
    }

    /** Called from Rust: stop the current answer (the user interrupted). */
    public void stormscopeLiveFlushPlayback() {
        GeminiLiveAudio.flushPlayback();
    }

    /** Called from Rust: current microphone level 0..1 for the meter. */
    public float stormscopeLiveLevel() {
        return GeminiLiveAudio.level();
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode == LOCATION_REQUEST) {
            startLocation(false);
        } else if (requestCode == NOTIFICATION_REQUEST
                && sPrefs != null
                && sPrefs.getBoolean("storm_watch_enabled", false)) {
            try {
                StormWatchService.start(this);
            } catch (Exception e) {
                Log.w(TAG, "Storm Watch could not start after notification grant: " + e);
            }
        } else if (requestCode == AUDIO_REQUEST) {
            sLivePermissionPending = false;
            boolean granted = grantResults.length > 0
                    && grantResults[0] == PackageManager.PERMISSION_GRANTED;
            if (granted && sLivePendingStart) {
                sLivePendingStart = false;
                GeminiLiveAudio.start();
                if (!GeminiLiveAudio.isRunning()) {
                    sLiveError = GeminiLiveAudio.lastError();
                }
            } else if (!granted && sLivePendingStart) {
                sLivePendingStart = false;
                sLiveError = "Microphone permission denied. Allow it in Android Settings.";
            }
        }
    }

    /**
     * Called from Rust (JNI, Java main thread): turn the opt-in background
     * watcher on or off. The service itself keeps the persistent notification
     * and owns a separate TTS engine, so it survives the Activity closing.
     */
    public void stormscopeSetStormWatch(boolean enabled) {
        if (sPrefs == null) {
            sPrefs = getSharedPreferences("stormscope", MODE_PRIVATE);
        }
        sPrefs.edit().putBoolean("storm_watch_enabled", enabled).apply();
        if (enabled) {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU
                    && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                    != PackageManager.PERMISSION_GRANTED) {
                requestPermissions(
                        new String[]{Manifest.permission.POST_NOTIFICATIONS},
                        NOTIFICATION_REQUEST);
            }
            try {
                StormWatchService.start(this);
            } catch (Exception e) {
                Log.w(TAG, "Storm Watch could not start: " + e);
            }
        } else {
            StormWatchService.stop(this);
        }
    }

    /** Called from Rust (JNI, UI thread): the persisted Storm Watch toggle. */
    public boolean stormscopeStormWatchEnabled() {
        if (sPrefs == null) {
            sPrefs = getSharedPreferences("stormscope", MODE_PRIVATE);
        }
        return sPrefs.getBoolean("storm_watch_enabled", false);
    }

    private boolean hasLocationPermission() {
        return checkSelfPermission(Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED
                || checkSelfPermission(Manifest.permission.ACCESS_COARSE_LOCATION) == PackageManager.PERMISSION_GRANTED;
    }

    /**
     * Start one burst of location updates. {@code fresh} discards the cached
     * fix first and only accepts a recent last-known location, so a manual
     * "locate me" does not snap to yesterday's position.
     */
    private void startLocation(boolean fresh) {
        if (!hasLocationPermission()) {
            // Ask once at launch; a manual request may ask again (the system
            // stops showing the dialog after "don't ask again").
            if (!sPermissionAsked || fresh) {
                sPermissionAsked = true;
                requestPermissions(new String[]{
                        Manifest.permission.ACCESS_FINE_LOCATION,
                        Manifest.permission.ACCESS_COARSE_LOCATION
                }, LOCATION_REQUEST);
            }
            return;
        }

        if (fresh) {
            sLocation = null;
        }

        LocationManager lm = (LocationManager) getSystemService(Context.LOCATION_SERVICE);
        if (lm == null) {
            return;
        }
        if (sListener == null) {
            sListener = new LocationListener() {
                @Override
                public void onLocationChanged(Location location) {
                    accept(location);
                }

                @Override
                public void onProviderEnabled(String provider) {
                }

                @Override
                public void onProviderDisabled(String provider) {
                }

                @Override
                public void onStatusChanged(String provider, int status, Bundle extras) {
                }
            };
            try {
                lm.requestLocationUpdates(LocationManager.GPS_PROVIDER, 3000L, 0f, sListener);
            } catch (Exception ignored) {
                // No GPS (or precise permission denied): the network provider
                // below may still deliver a coarse fix.
            }
            try {
                lm.requestLocationUpdates(LocationManager.NETWORK_PROVIDER, 5000L, 0f, sListener);
            } catch (Exception ignored) {
            }
            sManager = lm;
        }

        // Seed from the last known fix so the first frame already has
        // something. Ten minutes at launch, one minute for a manual refresh.
        long maxAge = fresh ? 60_000L : 10 * 60_000L;
        for (String provider : new String[]{
                LocationManager.GPS_PROVIDER,
                LocationManager.NETWORK_PROVIDER,
                LocationManager.PASSIVE_PROVIDER
        }) {
            try {
                Location last = lm.getLastKnownLocation(provider);
                if (last != null && System.currentTimeMillis() - last.getTime() < maxAge) {
                    accept(last);
                    break;
                }
            } catch (Exception ignored) {
            }
        }
    }

    private static void accept(Location location) {
        if (location == null) {
            return;
        }
        sLocation = new double[]{location.getLatitude(), location.getLongitude()};
        SharedPreferences prefs = sPrefs;
        if (prefs != null) {
            prefs.edit()
                    .putLong("last_lat_bits",
                            Double.doubleToRawLongBits(location.getLatitude()))
                    .putLong("last_lon_bits",
                            Double.doubleToRawLongBits(location.getLongitude()))
                    .putLong("last_fix_ms", System.currentTimeMillis())
                    .apply();
        }
    }

    /**
     * Called from Rust (JNI, UI thread): the latest fix as {lat, lon}, or
     * null while none has arrived.
     */
    public double[] stormscopeLocation() {
        return sLocation;
    }

    /** Called from Rust (JNI, Java main thread): ask for a fresh fix. */
    public void stormscopeRequestLocation() {
        startLocation(true);
    }

    /** Called from Rust (JNI, Java main thread): stop the update burst. */
    public void stormscopeStopLocation() {
        LocationManager lm = sManager;
        LocationListener listener = sListener;
        if (lm != null && listener != null) {
            try {
                lm.removeUpdates(listener);
            } catch (Exception ignored) {
            }
        }
        sManager = null;
        sListener = null;
    }
}
