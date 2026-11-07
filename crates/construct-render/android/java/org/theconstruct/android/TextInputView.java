package org.theconstruct.android;

import android.app.Activity;
import android.view.View;
import android.view.ViewGroup;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;

/**
 * What the keyboard connects to: an invisible, focusable view over the NativeActivity (which has no input connection
 * of its own). Its input connection ({@link TextInput}) forwards every call to Rust ({@link NativeText},
 * crates/construct-render/src/ime.rs), which answers from the focused field. It holds no app logic: Rust asks it to
 * show or hide the keyboard and tells it when the field changed.
 */
public final class TextInputView extends View {
    private final InputMethodManager imm;

    private TextInputView(Activity activity) {
        super(activity);
        setFocusable(true);
        setFocusableInTouchMode(true);
        imm = activity.getSystemService(InputMethodManager.class);
    }

    /** Adds the view to the activity's content (1 × 1 px: a view without a size can't take focus). */
    static TextInputView attach(Activity activity) {
        TextInputView view = new TextInputView(activity);
        activity.addContentView(view, new ViewGroup.LayoutParams(1, 1));
        return view;
    }

    @Override
    public boolean onCheckIsTextEditor() {
        return true;
    }

    @Override
    public InputConnection onCreateInputConnection(EditorInfo out) {
        int[] info = NativeText.editorInfo();
        out.inputType = info[0];
        out.imeOptions = info[1];
        out.initialSelStart = info[2];
        out.initialSelEnd = info[3];
        return new TextInput();
    }

    /** Shows the keyboard, connected to this view (called from Rust). */
    public void showKeyboard() {
        post(() -> {
            requestFocus();
            imm.showSoftInput(this, 0);
        });
    }

    /** Hides the keyboard (called from Rust). */
    public void hideKeyboard() {
        post(() -> imm.hideSoftInputFromWindow(getWindowToken(), 0));
    }

    /** Another field, or its text changed under the keyboard (called from Rust). */
    public void restartInput() {
        post(() -> imm.restartInput(this));
    }

    /** The selection or the word being composed moved (called from Rust; UTF-16 offsets, -1 for none). */
    public void updateSelection(int selStart, int selEnd, int composingStart, int composingEnd) {
        post(() -> imm.updateSelection(this, selStart, selEnd, composingStart, composingEnd));
    }
}
