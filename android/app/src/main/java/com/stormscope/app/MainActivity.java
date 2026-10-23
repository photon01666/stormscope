package com.stormscope.app;

import android.Manifest;
import android.content.Context;
import android.content.pm.PackageManager;
import android.location.Location;
import android.location.LocationListener;
import android.location.LocationManager;
import android.media.AudioAttributes;
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

    /** Latest fix as {lat, lon}; volatile because Rust reads it on its own thread. */
    private static volatile double[] sLocation;

    private static LocationManager sManager;
    private static LocationListener sListener;
    private static boolean sPermissionAsked;

    /** Soft-keyboard height in px, straight from GameActivity's IME insets. */
    private static volatile int sImeInset;

    private static final String TAG = "stormscope";
    private static final int TTS_PENDING_MAX = 8;
    /** Android's system TTS engine; announcements are queued until it is ready. */
    private static volatile TextToSpeech sTts;
    private static volatile boolean sTtsReady;
    private static final ArrayDeque<String> sTtsPending = new ArrayDeque<>();

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        startLocation(false);
        initTts();
    }

    @Override
    protected void onDestroy() {
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

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode == LOCATION_REQUEST) {
            startLocation(false);
        }
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
