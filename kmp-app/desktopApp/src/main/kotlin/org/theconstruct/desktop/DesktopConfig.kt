package org.theconstruct.desktop

import org.theconstruct.ServerOption
import org.theconstruct.ServerUrls
import java.io.File
import java.util.prefs.Preferences

/** The staging pod (deploy/, Fly.io), the default server (D29). */
internal const val DEFAULT_BASE_URL = "https://the-construct-pod.fly.dev"

/** The app shown at launch unless -Dconstruct.workspace / CONSTRUCT_WORKSPACE names another. */
internal const val DEFAULT_WORKSPACE = "pixelfed_home"

/** The menu's Server presets on desktop (a local pod is on the host's loopback; the emulator alias makes no sense here). */
internal val DESKTOP_SERVERS = listOf(
    ServerOption("staging", "Staging", DEFAULT_BASE_URL),
    ServerOption("local", "Local", "http://127.0.0.1:8080"),
)

/** System property wins over env var; null when neither is set. */
internal fun override(property: String, env: String): String? =
    System.getProperty(property)?.takeIf { it.isNotBlank() }
        ?: System.getenv(env)?.takeIf { it.isNotBlank() }

/**
 * The server at launch: the override (-Dconstruct.baseUrl / CONSTRUCT_BASE_URL), else the menu's saved choice (if still
 * a valid `http(s)://host[:port]`), else the staging pod.
 */
internal fun resolveBaseUrl(override: String?, saved: String?): String =
    (override ?: saved?.let(ServerUrls::parse) ?: DEFAULT_BASE_URL).trimEnd('/')

/**
 * Engine storage per server (as on Android, D29): the staging pod keeps [root] (`~/.construct`), any other server gets
 * `root/servers/<sanitized url>`, so one server's cache and pending outbox rows are never shown for, or flushed to,
 * another. All apps of one server share its directory (cache.db keys workspaces by id).
 */
internal fun storageDirFor(root: File, baseUrl: String): File {
    if (baseUrl.trimEnd('/') == DEFAULT_BASE_URL) return root
    return File(File(root, "servers"), baseUrl.trimEnd('/').replace(Regex("[^A-Za-z0-9.-]"), "_"))
}

/** The menu's server choice, kept in java.util.prefs (node `org/theconstruct/desktop`, key `baseUrl`). */
internal object DesktopServerPrefs {
    private const val KEY = "baseUrl"
    private val node: Preferences? get() = try {
        Preferences.userRoot().node("org/theconstruct/desktop")
    } catch (t: Throwable) {
        println("[desktop] preferences unavailable: $t")
        null
    }

    fun saved(): String? = try {
        node?.get(KEY, null)
    } catch (t: Throwable) {
        null
    }

    /** Choosing the default (staging) clears the key. */
    fun save(url: String) {
        try {
            val n = node ?: return
            if (url.trimEnd('/') == DEFAULT_BASE_URL) n.remove(KEY) else n.put(KEY, url.trimEnd('/'))
            n.flush()
        } catch (t: Throwable) {
            println("[desktop] cannot save the server choice: $t")
        }
    }
}
