package org.theconstruct.android;

import android.content.Context;
import org.unifiedpush.android.connector.MessagingReceiver;

/**
 * What the UnifiedPush distributor (ntfy, ...) tells the app, also while it is closed: a new endpoint, a push message
 * (still encrypted), the end of a registration. The connector library's receiver checks the token and acknowledges;
 * this class holds no app logic: each callback goes straight to Rust ({@link NativePush}, `src/push.rs`).
 */
public final class PushReceiver extends MessagingReceiver {
    static {
        System.loadLibrary("construct_render");
    }

    @Override
    public void onNewEndpoint(Context context, String endpoint, String instance) {
        NativePush.endpoint(context.getApplicationContext(), endpoint, instance);
    }

    @Override
    public void onMessage(Context context, byte[] message, String instance) {
        NativePush.message(context.getApplicationContext(), message, instance);
    }

    @Override
    public void onUnregistered(Context context, String instance) {
        NativePush.unregistered(context.getApplicationContext(), instance);
    }

    @Override
    public void onRegistrationFailed(Context context, String instance) {
        NativePush.failed(context.getApplicationContext(), instance);
    }
}
