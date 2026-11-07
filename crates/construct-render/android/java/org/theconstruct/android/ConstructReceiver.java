package org.theconstruct.android;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/**
 * A message for the app, also while it is closed (Android starts the process for it): what a push message does. It
 * holds no app logic: the message goes straight to Rust ({@link Native}).
 */
public final class ConstructReceiver extends BroadcastReceiver {
    static {
        System.loadLibrary("construct_render");
    }

    @Override
    public void onReceive(Context context, Intent intent) {
        Native.message(context.getApplicationContext(), intent.getStringExtra("message"));
    }
}
