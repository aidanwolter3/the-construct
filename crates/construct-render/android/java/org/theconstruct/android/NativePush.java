package org.theconstruct.android;

import android.content.Context;

/**
 * The Rust side of {@link PushReceiver} and {@link PushKeys} (crates/construct-render/src/push.rs,
 * `Java_org_theconstruct_android_NativePush_*`).
 */
final class NativePush {
    private NativePush() {}

    static native void endpoint(Context context, String endpoint, String instance);

    static native void message(Context context, byte[] message, boolean decrypted, String instance);

    static native void unregistered(Context context, String instance);

    static native void failed(Context context, String reason, String instance);

    /** The plaintext, or a thrown exception (the connector then hands the message on undecrypted). */
    static native byte[] decrypt(Context context, String instance, byte[] sealed);

    static native void generate(Context context, String instance);

    /** {p256dh, auth} (base64url), or null without keys. */
    static native String[] publicKeys(Context context, String instance);

    static native boolean exists(Context context, String instance);

    static native void delete(Context context, String instance);
}
