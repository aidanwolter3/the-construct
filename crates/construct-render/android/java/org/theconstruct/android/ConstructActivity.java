package org.theconstruct.android;

import android.app.NativeActivity;
import android.content.ClipData;
import android.content.Intent;
import android.content.res.Configuration;
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

    /** The engine's dark page (res/values: construct_page_dark). */
    private static final int PAGE_DARK = 0xFF141318;

    /** What the keyboard connects to (read from Rust, src/ime.rs). */
    public volatile TextInputView textInput;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        applySystemBars(getResources().getConfiguration());
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
     * The activity takes uiMode changes itself (the manifest), so the theme's system bars are not re-applied when the
     * system switches between light and dark: set them here, or the status bar keeps the launch mode's icon colour.
     */
    @Override
    public void onConfigurationChanged(Configuration config) {
        super.onConfigurationChanged(config);
        applySystemBars(config);
    }

    /** Light bars with dark icons, or dark bars with light icons, as the layouts draw (the theme does the same). */
    private void applySystemBars(Configuration config) {
        boolean dark = (config.uiMode & Configuration.UI_MODE_NIGHT_MASK) == Configuration.UI_MODE_NIGHT_YES;
        Window window = getWindow();
        int color = dark ? PAGE_DARK : 0xFFFFFFFF;
        window.setStatusBarColor(color);
        window.setNavigationBarColor(color);
        if (Build.VERSION.SDK_INT >= 30) {
            WindowInsetsController bars = window.getInsetsController();
            if (bars != null) {
                int light = WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS
                        | WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS;
                bars.setSystemBarsAppearance(dark ? 0 : light, light);
            }
        } else {
            View decor = window.getDecorView();
            int light = View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR | View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR;
            int flags = decor.getSystemUiVisibility();
            decor.setSystemUiVisibility(dark ? flags & ~light : flags | light);
        }
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
