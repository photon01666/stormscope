package com.stormscope.app;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.util.Log;

/**
 * Restarts Storm Watch after a reboot or an in-app update when the user has
 * left the watcher enabled. The service itself is a foreground service, so
 * this receiver only has to hand it back to the system; it does not do any
 * polling or network work.
 */
public class BootReceiver extends BroadcastReceiver {
    private static final String TAG = "stormscope";

    @Override
    public void onReceive(Context context, Intent intent) {
        String action = intent == null ? null : intent.getAction();
        if (!Intent.ACTION_BOOT_COMPLETED.equals(action)
                && !Intent.ACTION_MY_PACKAGE_REPLACED.equals(action)) {
            return;
        }
        SharedPreferences prefs =
                context.getSharedPreferences("stormscope", Context.MODE_PRIVATE);
        if (!prefs.getBoolean("storm_watch_enabled", false)) {
            return;
        }
        try {
            StormWatchService.start(context);
            Log.i(TAG, "Storm Watch restarted after " + action);
        } catch (Exception error) {
            // Android 12+ may refuse a background foreground-service start;
            // the user can still open the app to restart it.
            Log.w(TAG, "Storm Watch boot restart failed: " + error);
        }
    }
}
