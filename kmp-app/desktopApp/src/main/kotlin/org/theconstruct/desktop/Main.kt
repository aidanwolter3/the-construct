package org.theconstruct.desktop

import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.painter.BitmapPainter
import androidx.compose.ui.graphics.toComposeImageBitmap
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import androidx.compose.ui.window.rememberWindowState
import org.theconstruct.ConstructApp
import org.theconstruct.ConstructController
import org.theconstruct.ConstructEngineBridge
import org.theconstruct.ServerChoice
import org.theconstruct.engine.UniffiEngineBridge
import uniffi.client_ffi.UrlOpener
import java.awt.Desktop
import java.awt.Taskbar
import java.awt.image.BufferedImage
import java.io.File
import java.net.URI
import javax.imageio.ImageIO

/** One server's engines: [controller] shows the current app and opens others (same server and storage). */
private class Session(val baseUrl: String, val controller: ConstructController)

// Top-level state: the session's controller holds the current bridge (which owns the engine handle and the JNA
// callback observers), so it stays strongly reachable for the whole life of the JVM process.
private var session: Session? by mutableStateOf(null)

/**
 * A controller for [workspace] on [baseUrl], with an engine factory so the menu can open the server's other apps.
 * Sign-in (DECISIONS D19): no secure store (the engine uses the macOS Keychain), the system browser, and a loopback
 * redirect (null redirect URI) that the engine listens for itself.
 */
private fun openSession(root: File, baseUrl: String, workspace: String): Session {
    val storage = storageDirFor(root, baseUrl).apply { mkdirs() }
    println("[desktop] starting base_url=$baseUrl workspace=$workspace storage=$storage")
    fun open(appId: String): ConstructEngineBridge {
        println("[desktop] opening engine for workspace=$appId")
        return UniffiEngineBridge.create(storage.absolutePath, baseUrl, appId, urlOpener = BrowserOpener)
    }
    return Session(baseUrl, ConstructController(open(workspace), workspace, ::open))
}

/**
 * Server precedence: -Dconstruct.baseUrl / CONSTRUCT_BASE_URL, else the menu's saved choice ([DesktopServerPrefs]),
 * else the staging pod. An override only decides the launch server: picking a server in the menu still switches this
 * run (and is saved), but the next launch with the override set starts on the override again.
 */
fun main() {
    val baseUrlOverride = override("construct.baseUrl", "CONSTRUCT_BASE_URL")
    val baseUrl = resolveBaseUrl(baseUrlOverride, DesktopServerPrefs.saved())
    val workspace = override("construct.workspace", "CONSTRUCT_WORKSPACE") ?: DEFAULT_WORKSPACE
    if (baseUrlOverride != null) println("[desktop] server from construct.baseUrl / CONSTRUCT_BASE_URL (wins over the menu's saved choice at launch)")
    val root = File(System.getProperty("user.home"), ".construct").apply { mkdirs() }
    session = openSession(root, baseUrl, workspace)

    // App icon "the construct" (assets/icon, scripts/gen_icons.sh): window icon from the full-bleed tile, dock icon
    // from the macOS-shaped variant (margin + shadow, like other dock icons).
    val windowIcon = loadIcon("/icon/construct-512.png")
    loadIcon("/icon/construct-macos-1024.png")?.let(::setDockIcon)

    application {
        Window(
            onCloseRequest = {
                session?.controller?.close()
                exitApplication()
            },
            title = "The Construct",
            icon = windowIcon?.let { BitmapPainter(it.toComposeImageBitmap()) },
            state = rememberWindowState(width = 480.dp, height = 720.dp),
        ) {
            val current = session ?: return@Window
            val server = ServerChoice(current.baseUrl, DESKTOP_SERVERS) { url -> switchServer(root, current, url, workspace) }
            key(current) { ConstructApp(current.controller, server = server) }
        }
    }
}

/**
 * The menu's server picker: replaces [current] with a session on [url] (same app), then saves the choice and closes the
 * old engine. If the new engine can't be created, nothing changes and nothing is saved.
 */
private fun switchServer(root: File, current: Session, url: String, defaultWorkspace: String) {
    if (url.trimEnd('/') == current.baseUrl) {
        current.controller.menu.closeMenu()
        return
    }
    val next = try {
        openSession(root, url.trimEnd('/'), current.controller.currentAppId() ?: defaultWorkspace)
    } catch (t: Throwable) {
        println("[desktop] cannot start an engine for $url: $t")
        return
    }
    DesktopServerPrefs.save(url)
    session = next
    try {
        current.controller.close()
    } catch (t: Throwable) {
        println("[desktop] closing the previous engine failed: $t")
    }
}

private fun loadIcon(resource: String): BufferedImage? = try {
    object {}.javaClass.getResourceAsStream(resource)?.use { ImageIO.read(it) }
} catch (t: Throwable) {
    println("[desktop] cannot load icon $resource: $t")
    null
}

/** Sets the macOS dock (taskbar) icon when the platform supports it; a no-op elsewhere. */
private fun setDockIcon(image: BufferedImage) {
    try {
        if (Taskbar.isTaskbarSupported() && Taskbar.getTaskbar().isSupported(Taskbar.Feature.ICON_IMAGE)) {
            Taskbar.getTaskbar().iconImage = image
        }
    } catch (t: Throwable) {
        println("[desktop] cannot set the dock icon: $t")
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
