package org.genui.canvas.desktop

import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import androidx.compose.ui.window.rememberWindowState
import org.genui.canvas.CanvasApp
import org.genui.canvas.CanvasController
import org.genui.canvas.engine.UniffiEngineBridge
import uniffi.client_ffi.UrlOpener
import java.awt.Desktop
import java.io.File
import java.net.URI

private const val DEFAULT_BASE_URL = "http://127.0.0.1:8080"
private const val DEFAULT_WORKSPACE = "home"

/** System property wins over env var, which wins over the default. */
private fun config(property: String, env: String, default: String): String =
    System.getProperty(property)?.takeIf { it.isNotBlank() }
        ?: System.getenv(env)?.takeIf { it.isNotBlank() }
        ?: default

// Top-level so the bridge (which owns the engine handle and the JNA callback observers) stays
// strongly reachable for the whole life of the JVM process.
private lateinit var bridge: UniffiEngineBridge

fun main() {
    val baseUrl = config("canvas.baseUrl", "CANVAS_BASE_URL", DEFAULT_BASE_URL)
    val workspace = config("canvas.workspace", "CANVAS_WORKSPACE", DEFAULT_WORKSPACE)
    val storageDir = File(System.getProperty("user.home"), ".canvas").apply { mkdirs() }

    println("[desktop] starting base_url=$baseUrl workspace=$workspace storage=$storageDir")
    // Sign-in (DECISIONS D19): no secure store (the engine uses the macOS Keychain), the system browser, and a
    // loopback redirect (null redirect URI) that the engine listens for itself.
    bridge = UniffiEngineBridge.create(storageDir.absolutePath, baseUrl, workspace, urlOpener = BrowserOpener)
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

/**
 * Opens the sign-in page in the default browser: `java.awt.Desktop.browse` when supported, else macOS `open`.
 * Called on the engine's auth thread. False (the engine then logs the URL) if neither works.
 */
internal object BrowserOpener : UrlOpener {
    override fun open(url: String): Boolean {
        try {
            if (Desktop.isDesktopSupported() && Desktop.getDesktop().isSupported(Desktop.Action.BROWSE)) {
                Desktop.getDesktop().browse(URI(url))
                return true
            }
        } catch (t: Throwable) {
            println("[desktop] Desktop.browse failed: $t; trying `open`")
        }
        return try {
            ProcessBuilder("open", url).inheritIO().start().waitFor() == 0
        } catch (t: Throwable) {
            println("[desktop] cannot open a browser: $t")
            false
        }
    }
}
