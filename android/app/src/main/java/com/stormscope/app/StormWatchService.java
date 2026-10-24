package com.stormscope.app;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.media.AudioAttributes;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.speech.tts.TextToSpeech;
import android.util.Log;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.BufferedReader;
import java.io.File;
import java.io.FileInputStream;
import java.io.InputStreamReader;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.time.OffsetDateTime;
import java.time.ZoneId;
import java.time.format.DateTimeFormatter;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Comparator;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import java.util.Set;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.TimeUnit;

/**
 * Opt-in Storm Watch: a foreground service that keeps polling the last GPS
 * point while Stormscope is closed and speaks new warnings/watches with its
 * own TTS engine. The foreground notification is the only persistent UI.
 */
public class StormWatchService extends Service {
    static final String ACTION_START = "com.stormscope.app.STORM_WATCH_START";
    static final String ACTION_STOP = "com.stormscope.app.STORM_WATCH_STOP";
    private static final String TAG = "stormscope";
    private static final String PREFS = "stormscope";
    private static final String CHANNEL = "storm_watch";
    private static final int NOTIFICATION_ID = 0x5701;
    private static final long POLL_SECONDS = 60;
    private static final double RADIUS_KM = 50.0 * 1.609344;
    private static final int MAX_SPOKEN = 3;

    private final DateTimeFormatter timeFmt =
            DateTimeFormatter.ofPattern("h:mm a", Locale.US);
    private final ArrayDeque<String> pendingSpeech = new ArrayDeque<>();

    private SharedPreferences prefs;
    private Handler main;
    private ScheduledExecutorService poller;
    private volatile boolean polling;
    private volatile boolean ttsReady;
    private volatile TextToSpeech tts;

    public static void start(Context context) {
        Intent intent = new Intent(context, StormWatchService.class).setAction(ACTION_START);
        context.startForegroundService(intent);
    }

    public static void stop(Context context) {
        context.stopService(new Intent(context, StormWatchService.class));
    }

    @Override
    public void onCreate() {
        super.onCreate();
        prefs = getSharedPreferences(PREFS, MODE_PRIVATE);
        main = new Handler(Looper.getMainLooper());
        createChannel();
        startForeground(NOTIFICATION_ID, notification());
        initTts();
        poller = Executors.newSingleThreadScheduledExecutor(runnable -> {
            Thread thread = new Thread(runnable, "storm-watch");
            thread.setDaemon(true);
            return thread;
        });
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent != null && ACTION_STOP.equals(intent.getAction())) {
            prefs.edit().putBoolean("storm_watch_enabled", false).apply();
            stopForeground(STOP_FOREGROUND_REMOVE);
            stopSelf();
            return START_NOT_STICKY;
        }
        if (!prefs.getBoolean("storm_watch_enabled", false)) {
            stopSelf();
            return START_NOT_STICKY;
        }
        startPolling();
        return START_STICKY;
    }

    @Override
    public void onDestroy() {
        polling = false;
        if (poller != null) {
            poller.shutdownNow();
            poller = null;
        }
        TextToSpeech engine = tts;
        tts = null;
        ttsReady = false;
        if (engine != null) {
            try {
                engine.stop();
                engine.shutdown();
            } catch (Exception ignored) {
            }
        }
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    private void startPolling() {
        if (polling) {
            return;
        }
        polling = true;
        poller.scheduleWithFixedDelay(() -> {
            try {
                poll();
            } catch (Exception e) {
                Log.w(TAG, "Storm Watch poll failed", e);
            }
        }, 0, POLL_SECONDS, TimeUnit.SECONDS);
    }

    // ------------------------------------------------------------------
    // Foreground notification

    private void createChannel() {
        NotificationManager nm = getSystemService(NotificationManager.class);
        if (nm == null || nm.getNotificationChannel(CHANNEL) != null) {
            return;
        }
        NotificationChannel channel = new NotificationChannel(
                CHANNEL, "Storm Watch", NotificationManager.IMPORTANCE_LOW);
        channel.setDescription("Keeps Stormscope watching for severe warnings in the background");
        channel.setShowBadge(false);
        nm.createNotificationChannel(channel);
    }

    private Notification notification() {
        Intent open = new Intent(this, MainActivity.class)
                .setFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP);
        PendingIntent content = PendingIntent.getActivity(
                this, 0, open,
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        Intent stop = new Intent(this, StormWatchService.class).setAction(ACTION_STOP);
        PendingIntent stopIntent = PendingIntent.getService(
                this, 1, stop,
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        return new Notification.Builder(this, CHANNEL)
                .setSmallIcon(android.R.drawable.stat_notify_sync)
                .setContentTitle("Stormscope Storm Watch")
                .setContentText("Watching for severe warnings in the background")
                .setContentIntent(content)
                .setOngoing(true)
                .setOnlyAlertOnce(true)
                .addAction(new Notification.Action.Builder(
                        android.R.drawable.ic_menu_close_clear_cancel,
                        "Stop",
                        stopIntent).build())
                .build();
    }

    // ------------------------------------------------------------------
    // TTS

    private void initTts() {
        tts = new TextToSpeech(this, status -> {
            if (status != TextToSpeech.SUCCESS) {
                Log.w(TAG, "Storm Watch TTS init failed: " + status);
                return;
            }
            TextToSpeech engine = tts;
            if (engine == null) {
                return;
            }
            int language = engine.setLanguage(Locale.US);
            if (language == TextToSpeech.LANG_MISSING_DATA
                    || language == TextToSpeech.LANG_NOT_SUPPORTED) {
                Log.w(TAG, "Storm Watch US English voice unavailable");
            }
            engine.setSpeechRate(1.0f);
            engine.setPitch(1.0f);
            engine.setAudioAttributes(new AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_ALARM)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                    .build());
            synchronized (pendingSpeech) {
                ttsReady = true;
                while (!pendingSpeech.isEmpty()) {
                    speakNow(pendingSpeech.removeFirst());
                }
            }
            Log.i(TAG, "Storm Watch TTS ready");
        });
    }

    private void speak(String text) {
        if (text == null || text.trim().isEmpty()) {
            return;
        }
        synchronized (pendingSpeech) {
            if (!ttsReady || tts == null) {
                if (pendingSpeech.size() >= 8) {
                    pendingSpeech.removeFirst();
                }
                pendingSpeech.addLast(text);
                return;
            }
            speakNow(text);
        }
    }

    private void speakNow(String text) {
        TextToSpeech engine = tts;
        if (engine == null) {
            return;
        }
        main.post(() -> {
            TextToSpeech current = tts;
            if (current != null) {
                current.speak(text, TextToSpeech.QUEUE_ADD, null,
                        "storm-watch-" + System.nanoTime());
            }
        });
    }

    // ------------------------------------------------------------------
    // Poll + filter

    private void poll() {
        if (!prefs.getBoolean("storm_watch_enabled", false)) {
            stopSelf();
            return;
        }
        if (MainActivity.isAppForeground()) {
            // The in-app voice path handles foreground announcements.
            return;
        }
        double lat = Double.longBitsToDouble(
                prefs.getLong("last_lat_bits", Double.doubleToRawLongBits(Double.NaN)));
        double lon = Double.longBitsToDouble(
                prefs.getLong("last_lon_bits", Double.doubleToRawLongBits(Double.NaN)));
        if (Double.isNaN(lat) || Double.isNaN(lon)) {
            Log.i(TAG, "Storm Watch waiting for a GPS fix");
            return;
        }

        VoicePrefs voice = readVoicePrefs();
        if (!voice.enabled) {
            return;
        }

        JSONArray features;
        try {
            features = new JSONObject(fetch(lat, lon)).optJSONArray("features");
        } catch (Exception e) {
            Log.w(TAG, "Storm Watch fetch failed: " + e);
            return;
        }
        if (features == null) {
            return;
        }

        Set<String> seen = new HashSet<>(
                prefs.getStringSet("storm_watch_seen", Collections.emptySet()));
        boolean baseline = prefs.getBoolean("storm_watch_baseline", false);
        Set<String> current = new HashSet<>();
        List<Alert> fresh = new ArrayList<>();

        for (int i = 0; i < features.length(); i++) {
            JSONObject feature = features.optJSONObject(i);
            if (feature == null) {
                continue;
            }
            JSONObject props = feature.optJSONObject("properties");
            if (props == null) {
                continue;
            }
            String event = props.optString("event", "");
            if (!announceable(event)) {
                continue;
            }
            String id = props.optString("id", "");
            if (id.isEmpty()) {
                continue;
            }
            String expires = props.optString("expires", "");
            String signature = id + "|" + event + "|" + expires;
            current.add(signature);
            if (!voice.categoryEnabled(event)) {
                continue;
            }
            if (!within50Miles(feature.optJSONObject("geometry"), lat, lon)) {
                continue;
            }
            if (!seen.contains(signature)) {
                fresh.add(new Alert(
                        event,
                        props.optString("areaDesc", ""),
                        props.optString("expires", ""),
                        props.optString("instruction", "")));
            }
        }

        prefs.edit()
                .putStringSet("storm_watch_seen", current)
                .putBoolean("storm_watch_baseline", true)
                .apply();
        if (!baseline || fresh.isEmpty()) {
            return;
        }
        if (MainActivity.isAppForeground()) {
            // The app came back while this poll was in flight; its own TTS
            // owns foreground announcements.
            return;
        }

        fresh.sort(Comparator.comparingInt(alert -> -rank(alert.event)));
        int spoken = Math.min(MAX_SPOKEN, fresh.size());
        for (int i = 0; i < spoken; i++) {
            speak(announcement(fresh.get(i)));
        }
        if (fresh.size() > MAX_SPOKEN) {
            speak("And " + (fresh.size() - MAX_SPOKEN) + " more new alerts near you.");
        }
    }

    private String fetch(double lat, double lon) throws Exception {
        String point = String.format(Locale.US, "%.4f,%.4f", lat, lon);
        URL url = new URL("https://api.weather.gov/alerts/active?status=actual&point=" + point);
        HttpURLConnection connection = (HttpURLConnection) url.openConnection();
        connection.setRequestProperty(
                "User-Agent",
                "stormscope/1.5.1 (+https://github.com/photon01666/stormscope)");
        connection.setRequestProperty("Accept", "application/geo+json");
        connection.setConnectTimeout(10_000);
        connection.setReadTimeout(15_000);
        try {
            int code = connection.getResponseCode();
            if (code < 200 || code >= 300) {
                throw new IllegalStateException("HTTP " + code);
            }
            try (BufferedReader reader = new BufferedReader(
                    new InputStreamReader(connection.getInputStream(), StandardCharsets.UTF_8))) {
                StringBuilder body = new StringBuilder();
                String line;
                while ((line = reader.readLine()) != null) {
                    body.append(line);
                }
                return body.toString();
            }
        } finally {
            connection.disconnect();
        }
    }

    private VoicePrefs readVoicePrefs() {
        VoicePrefs out = new VoicePrefs();
        File file = new File(getFilesDir(),
                "config/stormscope/voice-alerts.json");
        if (!file.isFile()) {
            return out;
        }
        try (FileInputStream in = new FileInputStream(file)) {
            byte[] bytes = new byte[(int) file.length()];
            int read = in.read(bytes);
            if (read <= 0) {
                return out;
            }
            JSONObject json = new JSONObject(new String(bytes, 0, read, StandardCharsets.UTF_8));
            out.enabled = json.optBoolean("enabled", true);
            out.tornado = json.optBoolean("tornado", true);
            out.severeThunderstorm = json.optBoolean("severe_thunderstorm", true);
            out.watches = json.optBoolean("watches", true);
            out.otherWarnings = json.optBoolean("other_warnings", true);
        } catch (Exception e) {
            Log.w(TAG, "Storm Watch cannot read voice settings: " + e);
        }
        return out;
    }

    private static boolean announceable(String event) {
        return (event.contains("Warning") || event.contains("Watch")
                || event.contains("Emergency"))
                && !event.contains("Advisory")
                && !event.contains("Statement");
    }

    private static int rank(String event) {
        if (event.contains("Tornado") && event.contains("Warning")) return 6;
        if (event.contains("Emergency")) return 5;
        if (event.contains("Severe Thunderstorm") && event.contains("Warning")) return 4;
        if (event.contains("Flash Flood") && event.contains("Warning")) return 3;
        if (event.contains("Warning")) return 2;
        return 1;
    }

    private String announcement(Alert alert) {
        StringBuilder text = new StringBuilder("New ").append(alert.event);
        String area = firstAreas(alert.area);
        if (!area.isEmpty()) {
            text.append(" for ").append(area);
        }
        String time = formatTime(alert.expires);
        if (!time.isEmpty()) {
            text.append(". Expires at ").append(time).append('.');
        } else {
            text.append('.');
        }
        String action = action(alert.event, alert.instruction);
        if (action != null) {
            text.append(' ').append(action);
        }
        return text.toString();
    }

    private static String firstAreas(String area) {
        String[] parts = area.split(";");
        StringBuilder out = new StringBuilder();
        int used = 0;
        for (String part : parts) {
            String value = part.trim();
            if (value.isEmpty()) {
                continue;
            }
            if (used == 2) {
                out.append(" and surrounding areas");
                break;
            }
            if (used > 0) {
                out.append(" and ");
            }
            out.append(value);
            used++;
        }
        String text = out.toString();
        return text.length() > 120 ? text.substring(0, 120) : text;
    }

    private String formatTime(String expires) {
        try {
            return OffsetDateTime.parse(expires)
                    .atZoneSameInstant(ZoneId.systemDefault())
                    .format(timeFmt);
        } catch (Exception ignored) {
            return "";
        }
    }

    private static String action(String event, String instruction) {
        if (event.contains("Watch")) {
            return null;
        }
        for (String sentence : instruction.split("[.!?]")) {
            String value = sentence.trim();
            if (!value.isEmpty()) {
                if (value.length() > 160) {
                    value = value.substring(0, 160);
                }
                return value + ".";
            }
        }
        if (event.contains("Tornado")) {
            return "Take cover now.";
        }
        if (event.contains("Severe Thunderstorm")) {
            return "Move indoors now.";
        }
        if (event.contains("Flash Flood")) {
            return "Move to higher ground now.";
        }
        return null;
    }

    // ------------------------------------------------------------------
    // Point-to-warning-polygon distance in the local tangent plane

    private static boolean within50Miles(JSONObject geometry, double lat, double lon) {
        if (geometry == null) {
            return false;
        }
        String type = geometry.optString("type", "");
        JSONArray coordinates = geometry.optJSONArray("coordinates");
        if (coordinates == null) {
            return false;
        }
        if ("Polygon".equals(type)) {
            return ringWithin(coordinates.optJSONArray(0), lat, lon);
        }
        if ("MultiPolygon".equals(type)) {
            for (int i = 0; i < coordinates.length(); i++) {
                JSONArray polygon = coordinates.optJSONArray(i);
                if (polygon != null && ringWithin(polygon.optJSONArray(0), lat, lon)) {
                    return true;
                }
            }
        }
        return false;
    }

    private static boolean ringWithin(JSONArray ring, double lat0, double lon0) {
        if (ring == null || ring.length() < 2) {
            return false;
        }
        int n = ring.length();
        double[] x = new double[n];
        double[] y = new double[n];
        double kmPerLon = 111.320 * Math.abs(Math.cos(Math.toRadians(lat0)));
        double kmPerLat = 110.574;
        for (int i = 0; i < n; i++) {
            JSONArray point = ring.optJSONArray(i);
            if (point == null || point.length() < 2) {
                return false;
            }
            x[i] = (point.optDouble(0) - lon0) * kmPerLon;
            y[i] = (point.optDouble(1) - lat0) * kmPerLat;
        }

        boolean inside = false;
        for (int i = 0, j = n - 1; i < n; j = i++) {
            if (((y[i] > 0) != (y[j] > 0))
                    && (0 < (x[j] - x[i]) * (0 - y[i]) / (y[j] - y[i]) + x[i])) {
                inside = !inside;
            }
        }
        if (inside) {
            return true;
        }

        double best = Double.MAX_VALUE;
        for (int i = 0, j = n - 1; i < n; j = i++) {
            double dx = x[i] - x[j];
            double dy = y[i] - y[j];
            double len2 = dx * dx + dy * dy;
            double t = len2 > 0 ? (-(x[j] * dx + y[j] * dy) / len2) : 0;
            t = Math.max(0, Math.min(1, t));
            double cx = x[j] + t * dx;
            double cy = y[j] + t * dy;
            best = Math.min(best, Math.sqrt(cx * cx + cy * cy));
        }
        return best <= RADIUS_KM;
    }

    private static final class Alert {
        final String event;
        final String area;
        final String expires;
        final String instruction;

        Alert(String event, String area, String expires, String instruction) {
            this.event = event;
            this.area = area;
            this.expires = expires;
            this.instruction = instruction;
        }
    }

    private static final class VoicePrefs {
        boolean enabled = true;
        boolean tornado = true;
        boolean severeThunderstorm = true;
        boolean watches = true;
        boolean otherWarnings = true;

        boolean categoryEnabled(String event) {
            if (event.contains("Watch")) return watches;
            if (event.contains("Tornado")) return tornado;
            if (event.contains("Severe Thunderstorm")) return severeThunderstorm;
            return otherWarnings;
        }
    }
}
