package org.genui.canvas.android

import android.content.Intent
import android.graphics.Color as AndroidColor
import android.os.Bundle
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import org.genui.canvas.CanvasApp
import org.genui.canvas.CanvasController
import org.genui.canvas.engine.UniffiEngineBridge
import java.io.File

private const val TAG = "CanvasEngine"

/** Intent extras that override the BuildConfig defaults, e.g. `adb shell am start ... --es workspace pixelfed_demo`. */
private const val EXTRA_BASE_URL = "baseUrl"
private const val EXTRA_WORKSPACE = "workspace"

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        // The shared UI uses Material3's light scheme, so ask for dark system-bar icons regardless of the
        // device's night mode; the root below pads by the safe-drawing insets so nothing sits under the bars.
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.light(AndroidColor.TRANSPARENT, AndroidColor.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.light(AndroidColor.TRANSPARENT, AndroidColor.TRANSPARENT),
        )
        super.onCreate(savedInstanceState)

        val storageDir = filesDir
        val vm = ViewModelProvider(
            this,
            viewModelFactory { initializer { CanvasViewModel(EngineConfig.from(intent, storageDir)) } },
        )[CanvasViewModel::class.java]

        setContent {
            Box(Modifier.fillMaxSize().background(Color.White).safeDrawingPadding()) {
                CanvasApp(vm.controller)
            }
        }
    }
}

internal data class EngineConfig(val storageDir: File, val baseUrl: String, val workspace: String) {
    companion object {
        /** Intent extra, else the BuildConfig default (-PcanvasBaseUrl / -PcanvasWorkspace at build time). */
        fun from(intent: Intent?, storageDir: File) = EngineConfig(
            storageDir = storageDir,
            baseUrl = intent?.getStringExtra(EXTRA_BASE_URL)?.takeIf { it.isNotBlank() } ?: BuildConfig.CANVAS_BASE_URL,
            workspace = intent?.getStringExtra(EXTRA_WORKSPACE)?.takeIf { it.isNotBlank() } ?: BuildConfig.CANVAS_WORKSPACE,
        )
    }
}

/**
 * Owns the engine for the life of the activity *task entry* (DECISIONS D11): a ViewModel survives
 * configuration changes such as rotation, so the engine is created once per launch, not once per
 * onCreate. The config is read from the intent that created it. When the activity finishes for good
 * (back / task removed), [onCleared] closes the [uniffi.client_ffi.EngineHandle]; dropping the Rust
 * engine also ends its background fetch thread. Holding the bridge here keeps it (and its JNA callback
 * observers) strongly reachable while the UI is alive.
 */
internal class CanvasViewModel(config: EngineConfig) : ViewModel() {
    private val bridge: UniffiEngineBridge
    val controller: CanvasController

    init {
        Log.i(TAG, "starting base_url=${config.baseUrl} workspace=${config.workspace} storage=${config.storageDir}")
        config.storageDir.mkdirs()
        bridge = UniffiEngineBridge.create(config.storageDir.absolutePath, config.baseUrl, config.workspace)
        controller = CanvasController(bridge)
    }

    override fun onCleared() {
        Log.i(TAG, "closing engine")
        bridge.handle.close()
    }
}
