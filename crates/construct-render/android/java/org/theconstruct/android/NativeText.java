package org.theconstruct.android;

/** The Rust side of the keyboard's input connection (crates/construct-render/src/ime.rs, `Java_org_theconstruct_android_NativeText_*`). */
final class NativeText {
    private NativeText() {}

    /** Input type, IME options, selection start and end, for {@code EditorInfo}. */
    static native int[] editorInfo();

    static native String textBeforeCursor(int n);

    static native String textAfterCursor(int n);

    static native String selectedText();

    static native int capsMode(int reqModes);

    static native boolean commitText(CharSequence text, int newCursorPosition);

    static native boolean setComposingText(CharSequence text, int newCursorPosition);

    static native boolean setComposingRegion(int start, int end);

    static native boolean finishComposingText();

    static native boolean deleteSurrounding(int before, int after, boolean codePoints);

    static native boolean setSelection(int start, int end);

    static native boolean editorAction(int action);

    static native boolean contextMenuAction(int id);

    static native boolean key(int action, int keyCode, int unicodeChar);
}
