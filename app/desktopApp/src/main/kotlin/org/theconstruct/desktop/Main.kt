package org.theconstruct.desktop

import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.awt.ComposeWindow
import androidx.compose.ui.graphics.painter.BitmapPainter
import androidx.compose.ui.graphics.toComposeImageBitmap
import androidx.compose.ui.platform.LocalWindowInfo
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Notification
import androidx.compose.ui.window.Tray
import androidx.compose.ui.window.TrayState
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.WindowState
import androidx.compose.ui.window.application
import androidx.compose.ui.window.rememberTrayState
import androidx.compose.ui.window.rememberWindowState
import org.theconstruct.AppNotification
import org.theconstruct.ConstructApp
import org.theconstruct.ConstructController
import org.theconstruct.ConstructEngineBridge
import org.theconstruct.ServerChoice
import org.theconstruct.engine.UniffiEngineBridge
import uniffi.client_ffi.UrlOpener
import java.awt.Desktop
import java.awt.GraphicsEnvironment
import java.awt.SystemTray
import java.awt.Taskbar
import java.awt.image.BufferedImage
import java.io.File
import java.net.InetAddress
import java.net.URI
import javax.imageio.ImageIO

/** One server's engines: [controller] shows the current app and opens others (same server and storage). */
private class Session(val baseUrl: String, val controller: ConstructController)

// Top-level state: the session's controller holds the current bridge (which owns the engine handle and the JNA
// callback observers), so it stays strongly reachable for the whole life of the JVM process.
private var session: Session? by mutableStateOf(null)

/** Why the last server switch failed (the menu's Server section shows it); cleared by the next attempt. */
private var switchError: String? by mutableStateOf(null)

/**
 * A controller for [workspace] (the root layout) on [baseUrl].
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
    // One engine for every layout on screen (decision `layouts-not-apps`).
    val controller = ConstructController(open(workspace), workspace)
    controller.setDeviceInfo("desktop", deviceName())
    return Session(baseUrl, controller)
}

/** The name the pod shows for this device: the host name, else "Desktop". */
private fun deviceName(): String = try {
    InetAddress.getLocalHost().hostName.takeIf { it.isNotBlank() && it != "localhost" } ?: "Desktop"
} catch (t: Throwable) {
    "Desktop"
}

/** Whether a tray icon (and so OS notifications) can be shown: not headless, and the platform has a system tray. */
private val traySupported: Boolean by lazy {
    try {
        !GraphicsEnvironment.isHeadless() && SystemTray.isSupported()
    } catch (t: Throwable) {
        false
    }
}

/** The window is focused: live notifications then only update the bell, else they become OS notifications. */
@Volatile
private var windowFocused = true

@Volatile
private var mainWindow: ComposeWindow? = null

/**
 * D52 on desktop, while the app runs: a live notification becomes an OS notification through the tray (only while the
 * window is not focused; when focused, the bell's badge updates). No closed-app push on desktop.
 */
private fun postNotification(tray: TrayState?, n: AppNotification) {
    if (windowFocused || tray == null) return
    try {
        tray.sendNotification(Notification(n.title, n.body.ifBlank { n.title }, Notification.Type.None))
    } catch (t: Throwable) {
        println("[desktop] cannot show a notification: $t")
    }
}

/** The dock / taskbar badge: the unread count (none at 0) where the platform supports it; a no-op elsewhere. */
private fun setDockBadge(unread: Int) {
    try {
        if (!Taskbar.isTaskbarSupported()) return
        val taskbar = Taskbar.getTaskbar()
        if (!taskbar.isSupported(Taskbar.Feature.ICON_BADGE_TEXT)) return
        taskbar.setIconBadge(if (unread > 0) (if (unread > 99) "99+" else unread.toString()) else null)
    } catch (t: Throwable) {
        println("[desktop] cannot set the dock badge: $t")
    }
}

/** Brings the window to the front (the tray icon's action). */
private fun showWindow(state: WindowState) {
    state.isMinimized = false
    mainWindow?.let {
        it.isVisible = true
        it.toFront()
        it.requestFocus()
    }
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
        val windowState = rememberWindowState(width = 480.dp, height = 720.dp)
        val trayState = if (traySupported) rememberTrayState() else null
        if (trayState != null && windowIcon != null) {
            Tray(
                icon = BitmapPainter(windowIcon.toComposeImageBitmap()),
                state = trayState,
                tooltip = "The Construct",
                onAction = { showWindow(windowState) },
            )
        }
        Window(
            onCloseRequest = {
                session?.controller?.close()
                exitApplication()
            },
            title = "The Construct",
            icon = windowIcon?.let { BitmapPainter(it.toComposeImageBitmap()) },
            state = windowState,
        ) {
            mainWindow = window
            val windowInfo = LocalWindowInfo.current
            LaunchedEffect(windowInfo) { snapshotFlow { windowInfo.isWindowFocused }.collect { windowFocused = it } }
            val current = session ?: return@Window
            DisposableEffect(current) {
                current.controller.onNotification = { n -> postNotification(trayState, n) }
                onDispose { current.controller.onNotification = null }
            }
            val unread by current.controller.unread.collectAsState()
            LaunchedEffect(unread.total) { setDockBadge(unread.total) }
            val server = ServerChoice(current.baseUrl, DESKTOP_SERVERS, error = switchError) { url -> switchServer(root, current, url, workspace) }
            val frame = window
            val picker = remember(frame) { DesktopMediaPicker { frame } }
            key(current) { ConstructApp(current.controller, server = server, mediaPicker = picker) }
        }
    }
}

/**
 * The menu's server picker: replaces [current] with a session on [url] (same app), then saves the choice and closes the
 * old engine. If the new engine can't be created, nothing changes and nothing is saved; the Server section says why.
 */
private fun switchServer(root: File, current: Session, url: String, defaultWorkspace: String) {
    switchError = null
    if (url.trimEnd('/') == current.baseUrl) {
        return
    }
    val next = try {
        openSession(root, url.trimEnd('/'), current.controller.rootId)
    } catch (t: Throwable) {
        println("[desktop] cannot start an engine for $url: $t")
        switchError = ServerChoice.switchFailed(url, t)
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
