package org.theconstruct.android;

import android.content.Context;

/** The Rust side of {@link PushReceiver} (crates/construct-render/src/push.rs, `Java_org_theconstruct_android_NativePush_*`). */
final class NativePush {
    private NativePush() {}

    static native void endpoint(Context context, String endpoint, String instance);

    static native void message(Context context, byte[] message, String instance);

    static native void unregistered(Context context, String instance);

    static native void failed(Context context, String instance);
}
