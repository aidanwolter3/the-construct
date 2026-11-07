package org.theconstruct.android;

import android.os.Bundle;
import android.os.Handler;
import android.view.KeyEvent;
import android.view.inputmethod.CompletionInfo;
import android.view.inputmethod.CorrectionInfo;
import android.view.inputmethod.ExtractedText;
import android.view.inputmethod.ExtractedTextRequest;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputContentInfo;

/**
 * The keyboard's input connection to the focused field: each call goes straight to Rust ({@link NativeText}), which
 * edits and reads the field. Calls Rust has no use for answer as a plain view would (nothing extracted, no rich
 * content, no cursor updates).
 */
final class TextInput implements InputConnection {
    @Override
    public CharSequence getTextBeforeCursor(int n, int flags) {
        return NativeText.textBeforeCursor(n);
    }

    @Override
    public CharSequence getTextAfterCursor(int n, int flags) {
        return NativeText.textAfterCursor(n);
    }

    @Override
    public CharSequence getSelectedText(int flags) {
        return NativeText.selectedText();
    }

    @Override
    public int getCursorCapsMode(int reqModes) {
        return NativeText.capsMode(reqModes);
    }

    @Override
    public ExtractedText getExtractedText(ExtractedTextRequest request, int flags) {
        return null;
    }

    @Override
    public boolean deleteSurroundingText(int beforeLength, int afterLength) {
        return NativeText.deleteSurrounding(beforeLength, afterLength, false);
    }

    @Override
    public boolean deleteSurroundingTextInCodePoints(int beforeLength, int afterLength) {
        return NativeText.deleteSurrounding(beforeLength, afterLength, true);
    }

    @Override
    public boolean setComposingText(CharSequence text, int newCursorPosition) {
        return NativeText.setComposingText(text, newCursorPosition);
    }

    @Override
    public boolean setComposingRegion(int start, int end) {
        return NativeText.setComposingRegion(start, end);
    }

    @Override
    public boolean finishComposingText() {
        return NativeText.finishComposingText();
    }

    @Override
    public boolean commitText(CharSequence text, int newCursorPosition) {
        return NativeText.commitText(text, newCursorPosition);
    }

    @Override
    public boolean commitCompletion(CompletionInfo text) {
        return false;
    }

    @Override
    public boolean commitCorrection(CorrectionInfo correctionInfo) {
        return true;
    }

    @Override
    public boolean setSelection(int start, int end) {
        return NativeText.setSelection(start, end);
    }

    @Override
    public boolean performEditorAction(int editorAction) {
        return NativeText.editorAction(editorAction);
    }

    @Override
    public boolean performContextMenuAction(int id) {
        return NativeText.contextMenuAction(id);
    }

    @Override
    public boolean beginBatchEdit() {
        return true;
    }

    @Override
    public boolean endBatchEdit() {
        return true;
    }

    @Override
    public boolean sendKeyEvent(KeyEvent event) {
        return NativeText.key(event.getAction(), event.getKeyCode(), event.getUnicodeChar());
    }

    @Override
    public boolean clearMetaKeyStates(int states) {
        return true;
    }

    @Override
    public boolean reportFullscreenMode(boolean enabled) {
        return true;
    }

    @Override
    public boolean performPrivateCommand(String action, Bundle data) {
        return false;
    }

    @Override
    public boolean requestCursorUpdates(int cursorUpdateMode) {
        return false;
    }

    @Override
    public Handler getHandler() {
        return null;
    }

    @Override
    public void closeConnection() {}

    @Override
    public boolean commitContent(InputContentInfo inputContentInfo, int flags, Bundle opts) {
        return false;
    }
}
