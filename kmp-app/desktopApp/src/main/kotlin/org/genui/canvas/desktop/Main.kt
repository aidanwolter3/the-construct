package org.genui.canvas.desktop

import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import androidx.compose.ui.window.rememberWindowState
import org.genui.canvas.CanvasApp
import org.genui.canvas.CanvasController
import uniffi.client_ffi.EngineHandle
import java.io.File

private const val DEFAULT_BASE_URL = "http://127.0.0.1:8080"
private const val DEFAULT_WORKSPACE = "home"

/** System property wins over env var, which wins over the default. */
private fun config(property: String, env: String, default: String): String =
    System.getProperty(property)?.takeIf { it.isNotBlank() }
        ?: System.getenv(env)?.takeIf { it.isNotBlank() }
        ?: default

// Top-level so the engine handle and bridge (which owns the JNA callback observers) stay
// strongly reachable for the whole life of the JVM process.
private lateinit var engine: EngineHandle
private lateinit var bridge: EngineBridge

fun main() {
    val baseUrl = config("canvas.baseUrl", "CANVAS_BASE_URL", DEFAULT_BASE_URL)
    val workspace = config("canvas.workspace", "CANVAS_WORKSPACE", DEFAULT_WORKSPACE)
    val storageDir = File(System.getProperty("user.home"), ".canvas").apply { mkdirs() }

    engine = EngineHandle.newWithWorkspace(storageDir.absolutePath, baseUrl, workspace)
    println("[desktop] engine created base_url=$baseUrl workspace=$workspace")
    bridge = EngineBridge(engine)
    val controller = CanvasController(bridge)

    application {
        Window(
            onCloseRequest = ::exitApplication,
            title = "Project Canvas",
            state = rememberWindowState(width = 480.dp, height = 720.dp),
        ) {
            CanvasApp(controller)
        }
    }
}
