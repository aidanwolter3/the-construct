package org.theconstruct.android;

import android.app.NativeActivity;
import android.content.ClipData;
import android.content.Intent;
import android.graphics.Color;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.provider.MediaStore;
import android.view.View;
import android.view.Window;
import android.view.WindowInsetsController;

/**
 * The bridge's activity: NativeActivity (the engine draws everything, from Rust), plus the Android callbacks a
 * NativeActivity doesn't pass on. It holds no app logic: each callback goes straight to Rust ({@link Native}).
 */
public final class ConstructActivity extends NativeActivity {
    static {
        System.loadLibrary("construct_render");
    }

    private static final int PICK_IMAGES = 1;

    /** What the keyboard connects to (read from Rust, src/ime.rs). */
    public volatile TextInputView textInput;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        edgeToEdge();
        textInput = TextInputView.attach(this);
        link(getIntent());
    }

    /** A link opened the app while it runs (the sign-in redirect, a notification). */
    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        link(intent);
    }

    /**
     * The engine draws edge to edge: under transparent status and navigation bars, with no scrim, so whatever a
     * layout paints there shows (it pads its content by the window insets itself, android.rs). The bars' icon colour
     * follows what is drawn under them ({@link #setLightBars}, from Rust), not the night mode.
     */
    private void edgeToEdge() {
        Window window = getWindow();
        window.setStatusBarColor(Color.TRANSPARENT);
        window.setNavigationBarColor(Color.TRANSPARENT);
        if (Build.VERSION.SDK_INT >= 29) {
            window.setStatusBarContrastEnforced(false);
            window.setNavigationBarContrastEnforced(false);
        }
        if (Build.VERSION.SDK_INT >= 30) {
            window.setDecorFitsSystemWindows(false);
        } else {
            View decor = window.getDecorView();
            decor.setSystemUiVisibility(decor.getSystemUiVisibility()
                    | View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                    | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                    | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION);
        }
    }

    /** Dark icons on a bar over a light colour, light icons over a dark one (called from Rust). */
    public void setLightBars(boolean lightStatus, boolean lightNav) {
        runOnUiThread(() -> {
            Window window = getWindow();
            if (Build.VERSION.SDK_INT >= 30) {
                WindowInsetsController bars = window.getInsetsController();
                if (bars == null) return;
                int status = WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS;
                int nav = WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS;
                bars.setSystemBarsAppearance((lightStatus ? status : 0) | (lightNav ? nav : 0), status | nav);
            } else {
                View decor = window.getDecorView();
                int flags = decor.getSystemUiVisibility() & ~(View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR
                        | View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR);
                if (lightStatus) flags |= View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR;
                if (lightNav) flags |= View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR;
                decor.setSystemUiVisibility(flags);
            }
        });
    }

    private static void link(Intent intent) {
        Uri data = intent == null ? null : intent.getData();
        if (data != null) Native.link(data.toString());
    }

    /** Opens Android's photo picker (called from Rust); the photos come back in onActivityResult. */
    public void pickImages(int max) {
        runOnUiThread(() -> {
            Intent intent;
            if (Build.VERSION.SDK_INT >= 33) {
                intent = new Intent(MediaStore.ACTION_PICK_IMAGES);
                if (max > 1) intent.putExtra(MediaStore.EXTRA_PICK_IMAGES_MAX, Math.min(max, MediaStore.getPickImagesMaxLimit()));
            } else {
                intent = new Intent(Intent.ACTION_GET_CONTENT).setType("image/*");
                intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, max > 1);
            }
            startActivityForResult(intent, PICK_IMAGES);
        });
    }

    /** Asks for a permission on the main thread (called from Rust). */
    public void askPermission(String permission, int request) {
        runOnUiThread(() -> requestPermissions(new String[] {permission}, request));
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request != PICK_IMAGES) return;
        String[] uris = new String[0];
        if (result == RESULT_OK && data != null) {
            ClipData clip = data.getClipData();
            if (clip != null) {
                uris = new String[clip.getItemCount()];
                for (int i = 0; i < uris.length; i++) uris[i] = clip.getItemAt(i).getUri().toString();
            } else if (data.getData() != null) {
                uris = new String[] {data.getData().toString()};
            }
        }
        Native.picked(this, uris);
    }
}
