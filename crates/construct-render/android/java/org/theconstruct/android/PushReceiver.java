package org.theconstruct.android;

import android.content.Context;
import org.unifiedpush.android.connector.FailedReason;
import org.unifiedpush.android.connector.MessagingReceiver;
import org.unifiedpush.android.connector.data.PushEndpoint;
import org.unifiedpush.android.connector.data.PushMessage;
import org.unifiedpush.android.connector.keys.KeyManager;

/**
 * What the UnifiedPush distributor (ntfy, ..., or the embedded FCM one) tells the app, also while it is closed: a new
 * endpoint, a push message, the end of a registration. The connector library's receiver checks the token and
 * acknowledges; the Web Push keys and decryption are Rust's ({@link PushKeys}). This class holds no app logic: each
 * callback goes straight to Rust ({@link NativePush}, `src/push.rs`).
 */
public final class PushReceiver extends MessagingReceiver {
    static {
        System.loadLibrary("construct_render");
    }

    @Override
    public KeyManager getKeyManager(Context context) {
        return new PushKeys(context.getApplicationContext());
    }

    @Override
    public void onNewEndpoint(Context context, PushEndpoint endpoint, String instance) {
        NativePush.endpoint(context.getApplicationContext(), endpoint.getUrl(), instance);
    }

    @Override
    public void onMessage(Context context, PushMessage message, String instance) {
        NativePush.message(context.getApplicationContext(), message.getContent(), message.getDecrypted(), instance);
    }

    @Override
    public void onUnregistered(Context context, String instance) {
        NativePush.unregistered(context.getApplicationContext(), instance);
    }

    @Override
    public void onRegistrationFailed(Context context, FailedReason reason, String instance) {
        NativePush.failed(context.getApplicationContext(), reason.name(), instance);
    }
}
