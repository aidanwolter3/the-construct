package org.theconstruct.android;

import android.content.Context;

/** The Rust side of the bridge (crates/construct-render/src/android.rs, `Java_org_theconstruct_android_Native_*`). */
final class Native {
    private Native() {}

    static native void link(String uri);

    static native void picked(Context context, String[] uris);

    static native void message(Context context, String message);
}
