package com.stormscope.app;

import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioRecord;
import android.media.AudioTrack;
import android.media.MediaRecorder;
import android.media.audiofx.AcousticEchoCanceler;
import android.media.audiofx.NoiseSuppressor;
import android.util.Log;

import java.util.ArrayDeque;

/**
 * Microphone capture and speaker playback for the Gemini Live session.
 *
 * <p>Gemini expects 16 kHz mono PCM16 input and returns 24 kHz mono PCM16.
 * The mic uses the voice-communication source with hardware echo cancellation
 * and noise suppression when available, which is what keeps the model from
 * hearing its own answer through the speaker.
 *
 * <p>The Rust session thread drains {@link #readMic()} and feeds
 * {@link #play(byte[])}; both only touch the thread-safe queues and are safe
 * to call from a JNI-attached thread.
 */
final class GeminiLiveAudio {
    private static final String TAG = "stormscope";
    private static final int RATE_IN = 16000;
    private static final int RATE_OUT = 24000;
    private static final int MIC_CHUNK_BYTES = RATE_IN / 10 * 2; // 100 ms
    private static final int MIC_QUEUE_MAX = 12;
    private static final int PLAY_QUEUE_MAX = 100;

    private static final Object MIC_LOCK = new Object();
    private static final Object PLAY_LOCK = new Object();
    private static final ArrayDeque<byte[]> sMicQueue = new ArrayDeque<>();
    private static final ArrayDeque<byte[]> sPlayQueue = new ArrayDeque<>();

    private static volatile boolean sRunning;
    private static volatile boolean sCapturePaused;
    private static volatile float sLevel;
    private static volatile String sLastError;
    /** Last time model audio was queued or played, for the echo gate. */
    private static volatile long sLastPlaybackNs;
    private static volatile AudioRecord sRecord;
    private static volatile AudioTrack sTrack;
    private static AcousticEchoCanceler sAec;
    private static NoiseSuppressor sNs;
    private static Thread sCaptureThread;
    private static Thread sPlayThread;

    private GeminiLiveAudio() {
    }

    static synchronized void start() {
        if (sRunning) {
            return;
        }
        sLastError = null;
        try {
            sRecord = buildRecord();
            sTrack = buildTrack();
            if (sRecord == null || sRecord.getState() != AudioRecord.STATE_INITIALIZED) {
                throw new IllegalStateException("AudioRecord could not initialize");
            }
            if (sTrack == null || sTrack.getState() != AudioTrack.STATE_INITIALIZED) {
                throw new IllegalStateException("AudioTrack could not initialize");
            }
            enableEffects();
            synchronized (MIC_LOCK) {
                sMicQueue.clear();
            }
            synchronized (PLAY_LOCK) {
                sPlayQueue.clear();
            }
            sLevel = 0f;
            sLastPlaybackNs = 0L;
            sCapturePaused = false;
            sRunning = true;
            sRecord.startRecording();
            sTrack.play();
            sCaptureThread = new Thread(GeminiLiveAudio::captureLoop, "gemini-mic");
            sPlayThread = new Thread(GeminiLiveAudio::playLoop, "gemini-speaker");
            sCaptureThread.start();
            sPlayThread.start();
            Log.i(TAG, "Gemini Live audio started (16 kHz in / 24 kHz out)");
        } catch (Exception e) {
            sLastError = e.getMessage() == null ? e.toString() : e.getMessage();
            Log.w(TAG, "Gemini Live audio start failed: " + e);
            stop();
        }
    }

    /**
     * Speaker-only mode for one-shot Gemini Live alerts: no microphone is
     * opened, so this works without RECORD_AUDIO permission.
     */
    static synchronized void startPlaybackOnly() {
        if (sRunning) {
            return;
        }
        sLastError = null;
        try {
            sTrack = buildTrack();
            if (sTrack == null || sTrack.getState() != AudioTrack.STATE_INITIALIZED) {
                throw new IllegalStateException("AudioTrack could not initialize");
            }
            synchronized (PLAY_LOCK) {
                sPlayQueue.clear();
            }
            sLastPlaybackNs = 0L;
            sRunning = true;
            sTrack.play();
            sPlayThread = new Thread(GeminiLiveAudio::playLoop, "gemini-alert-speaker");
            sPlayThread.start();
            Log.i(TAG, "Gemini Live playback started (24 kHz out)");
        } catch (Exception e) {
            sLastError = e.getMessage() == null ? e.toString() : e.getMessage();
            Log.w(TAG, "Gemini Live playback start failed: " + e);
            stop();
        }
    }

    static synchronized void stop() {
        if (!sRunning && sRecord == null && sTrack == null) {
            return;
        }
        sRunning = false;
        sCapturePaused = false;
        synchronized (MIC_LOCK) {
            sMicQueue.clear();
        }
        synchronized (PLAY_LOCK) {
            sPlayQueue.clear();
            PLAY_LOCK.notifyAll();
        }
        AudioRecord record = sRecord;
        AudioTrack track = sTrack;
        sRecord = null;
        sTrack = null;
        if (record != null) {
            try {
                record.stop();
            } catch (Exception ignored) {
            }
        }
        if (track != null) {
            try {
                track.pause();
                track.flush();
                track.stop();
            } catch (Exception ignored) {
            }
        }
        Thread mic = sCaptureThread;
        Thread play = sPlayThread;
        sCaptureThread = null;
        sPlayThread = null;
        if (mic != null) {
            try {
                mic.join(600);
            } catch (InterruptedException ignored) {
            }
        }
        if (play != null) {
            try {
                play.join(600);
            } catch (InterruptedException ignored) {
            }
        }
        if (record != null) {
            try {
                record.release();
            } catch (Exception ignored) {
            }
        }
        if (track != null) {
            try {
                track.release();
            } catch (Exception ignored) {
            }
        }
        if (sAec != null) {
            try {
                sAec.release();
            } catch (Exception ignored) {
            }
            sAec = null;
        }
        if (sNs != null) {
            try {
                sNs.release();
            } catch (Exception ignored) {
            }
            sNs = null;
        }
        sLevel = 0f;
        sLastPlaybackNs = 0L;
        Log.i(TAG, "Gemini Live audio stopped");
    }

    static void pauseCapture() {
        if (!sRunning) {
            return;
        }
        sCapturePaused = true;
        AudioRecord record = sRecord;
        if (record != null) {
            try {
                record.stop();
            } catch (Exception ignored) {
            }
        }
        flushMic();
        sLevel = 0f;
    }

    static void resumeCapture() {
        if (!sRunning) {
            return;
        }
        AudioRecord record = sRecord;
        if (record != null) {
            try {
                record.startRecording();
            } catch (Exception e) {
                sLastError = "Microphone could not resume: " + e.getMessage();
                return;
            }
        }
        sCapturePaused = false;
    }

    static boolean isRunning() {
        return sRunning;
    }

    static String lastError() {
        return sLastError;
    }

    static byte[] readMic() {
        synchronized (MIC_LOCK) {
            return sMicQueue.pollFirst();
        }
    }

    static void play(byte[] pcm) {
        if (pcm == null || pcm.length == 0) {
            return;
        }
        synchronized (PLAY_LOCK) {
            while (sPlayQueue.size() >= PLAY_QUEUE_MAX) {
                sPlayQueue.removeFirst();
            }
            sPlayQueue.addLast(pcm);
            sLastPlaybackNs = System.nanoTime();
            PLAY_LOCK.notifyAll();
        }
    }

    static void flushMic() {
        synchronized (MIC_LOCK) {
            sMicQueue.clear();
        }
    }

    /** Barge-in: empty the speaker queue and cut the currently playing audio. */
    static void flushPlayback() {
        synchronized (PLAY_LOCK) {
            sPlayQueue.clear();
            PLAY_LOCK.notifyAll();
        }
        sLastPlaybackNs = 0L;
        AudioTrack track = sTrack;
        if (track != null) {
            try {
                track.pause();
                track.flush();
                track.play();
            } catch (Exception ignored) {
            }
        }
    }

    static float level() {
        return sLevel;
    }

    private static AudioRecord buildRecord() {
        int min = AudioRecord.getMinBufferSize(
                RATE_IN,
                AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT);
        if (min <= 0) {
            min = MIC_CHUNK_BYTES * 4;
        }
        return new AudioRecord.Builder()
                .setAudioSource(MediaRecorder.AudioSource.VOICE_COMMUNICATION)
                .setAudioFormat(new AudioFormat.Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setSampleRate(RATE_IN)
                        .setChannelMask(AudioFormat.CHANNEL_IN_MONO)
                        .build())
                .setBufferSizeInBytes(Math.max(min, MIC_CHUNK_BYTES * 4))
                .build();
    }

    private static AudioTrack buildTrack() {
        int min = AudioTrack.getMinBufferSize(
                RATE_OUT,
                AudioFormat.CHANNEL_OUT_MONO,
                AudioFormat.ENCODING_PCM_16BIT);
        if (min <= 0) {
            min = RATE_OUT;
        }
        return new AudioTrack.Builder()
                .setAudioAttributes(new AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                        .build())
                .setAudioFormat(new AudioFormat.Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setSampleRate(RATE_OUT)
                        .setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
                        .build())
                .setBufferSizeInBytes(Math.max(min, RATE_OUT))
                .setTransferMode(AudioTrack.MODE_STREAM)
                .build();
    }

    private static void enableEffects() {
        AudioRecord record = sRecord;
        if (record == null) {
            return;
        }
        try {
            if (AcousticEchoCanceler.isAvailable()) {
                sAec = AcousticEchoCanceler.create(record.getAudioSessionId());
                if (sAec != null) {
                    sAec.setEnabled(true);
                }
            }
        } catch (Exception e) {
            Log.w(TAG, "echo canceller unavailable: " + e);
        }
        try {
            if (NoiseSuppressor.isAvailable()) {
                sNs = NoiseSuppressor.create(record.getAudioSessionId());
                if (sNs != null) {
                    sNs.setEnabled(true);
                }
            }
        } catch (Exception e) {
            Log.w(TAG, "noise suppressor unavailable: " + e);
        }
    }

    private static void captureLoop() {
        byte[] buffer = new byte[MIC_CHUNK_BYTES];
        AudioRecord record = sRecord;
        while (sRunning && record != null) {
            if (sCapturePaused) {
                sleep(50);
                continue;
            }
            int read;
            try {
                read = record.read(buffer, 0, buffer.length);
            } catch (Exception e) {
                if (sRunning) {
                    sLastError = "Microphone read failed: " + e.getMessage();
                }
                sleep(20);
                continue;
            }
            if (read <= 0) {
                sleep(10);
                continue;
            }
            // The speaker is the loudest thing this microphone can hear.
            // While model audio is queued or playing (plus a short tail),
            // discard captured chunks so the server never mistakes its own
            // voice for the user and cuts the answer off. The audio is still
            // read, so the record buffer stays live for the next turn.
            if (System.nanoTime() - sLastPlaybackNs < 600_000_000L) {
                sLevel = 0f;
                continue;
            }
            byte[] chunk = new byte[read];
            System.arraycopy(buffer, 0, chunk, 0, read);
            synchronized (MIC_LOCK) {
                while (sMicQueue.size() >= MIC_QUEUE_MAX) {
                    sMicQueue.removeFirst();
                }
                sMicQueue.addLast(chunk);
            }
            sLevel = levelOf(chunk);
        }
        sLevel = 0f;
    }

    private static void playLoop() {
        while (sRunning) {
            byte[] chunk;
            synchronized (PLAY_LOCK) {
                while (sRunning && sPlayQueue.isEmpty()) {
                    try {
                        PLAY_LOCK.wait(250);
                    } catch (InterruptedException e) {
                        return;
                    }
                }
                if (!sRunning) {
                    return;
                }
                chunk = sPlayQueue.removeFirst();
            }
            AudioTrack track = sTrack;
            if (track == null) {
                return;
            }
            try {
                track.write(chunk, 0, chunk.length);
                sLastPlaybackNs = System.nanoTime();
            } catch (Exception e) {
                if (sRunning) {
                    sLastError = "Speaker write failed: " + e.getMessage();
                }
                return;
            }
        }
    }

    private static float levelOf(byte[] pcm) {
        int samples = pcm.length / 2;
        if (samples == 0) {
            return 0f;
        }
        long sum = 0;
        for (int i = 0; i + 1 < pcm.length; i += 2) {
            int sample = (pcm[i] & 0xff) | (pcm[i + 1] << 8);
            sum += (long) sample * sample;
        }
        float rms = (float) Math.sqrt((double) sum / samples) / 32768f;
        return Math.min(1f, rms * 5f);
    }

    private static void sleep(long millis) {
        try {
            Thread.sleep(millis);
        } catch (InterruptedException ignored) {
        }
    }
}
