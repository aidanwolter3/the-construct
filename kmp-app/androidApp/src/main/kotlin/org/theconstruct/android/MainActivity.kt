package org.theconstruct.android

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.graphics.Color as AndroidColor
import android.net.Uri
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import org.theconstruct.ConstructApp
import org.theconstruct.ConstructController
import org.theconstruct.ConstructEngineBridge
import org.theconstruct.ServerChoice
import org.theconstruct.ServerOption
import org.theconstruct.ServerUrls
import org.theconstruct.engine.UniffiEngineBridge
import uniffi.client_ffi.UrlOpener
import java.io.File
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

private const val TAG = "ConstructEngine"

/** Intent extras that override the BuildConfig defaults, e.g. `adb shell am start ... --es workspace pixelfed_demo`. */
private const val EXTRA_BASE_URL = "baseUrl"
private const val EXTRA_WORKSPACE = "workspace"

/** OAuth redirect URI (custom scheme); the manifest's intent filter routes it back to [MainActivity]. */
internal const val AUTH_REDIRECT_URI = "construct://oauth"

/**
 * `launchMode="singleTask"` (manifest): the browser runs in its own task (ACTION_VIEW + NEW_TASK), so the
 * `construct://oauth` redirect must come back to *this* instance (and its ViewModel's engine, which holds the
 * pending sign-in) via [onNewIntent]; singleTop would start a second activity in the browser's task.
 */
class MainActivity : ComponentActivity() {
    private lateinit var vm: ConstructViewModel

    override fun onCreate(savedInstanceState: Bundle?) {
        // The shared UI uses Material3's light scheme, so ask for dark system-bar icons regardless of the
        // device's night mode; the root below pads by the safe-drawing insets so nothing sits under the bars.
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.light(AndroidColor.TRANSPARENT, AndroidColor.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.light(AndroidColor.TRANSPARENT, AndroidColor.TRANSPARENT),
        )
        super.onCreate(savedInstanceState)

        val storageDir = filesDir
        val appContext = applicationContext
        vm = ViewModelProvider(
            this,
            viewModelFactory {
                initializer {
                    ConstructViewModel(EngineConfig.from(intent, storageDir, ServerPrefs.saved(appContext)), appContext)
                }
            },
        )[ConstructViewModel::class.java]
        // Normally the redirect arrives through onNewIntent; this covers a cold start by the redirect (the
        // pending flow is gone then, so the engine just ignores it). Not on recreation (rotation): the intent
        // was already handled.
        if (savedInstanceState == null) handleAuthRedirect(intent)

        setContent {
            val session = vm.session
            // Cleartext (http://) custom URLs only to the hosts network_security_config.xml allows, or images break.
            val server = ServerChoice(session.baseUrl, ServerPrefs.options(), ServerUrls.ANDROID_CLEARTEXT_HOSTS, ::switchServer)
            // The shared UI's menu button sits at the top-left of this padded box, clear of the system bars.
            Box(Modifier.fillMaxSize().background(Color.White).safeDrawingPadding()) {
                key(session) { ConstructApp(session.controller, server = server) }
            }
        }
    }

    /**
     * The menu's server picker (all builds): the ViewModel starts an engine on [url] (that server's own storage, same
     * app) and only then saves the choice and closes the old engine. A launch `baseUrl` extra is dropped so a
     * re-created activity doesn't go back to it.
     */
    private fun switchServer(url: String) {
        if (vm.switchServer(url)) intent?.removeExtra(EXTRA_BASE_URL)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleAuthRedirect(intent)
    }

    private fun handleAuthRedirect(intent: Intent?) {
        val uri = intent?.data ?: return
        if (intent.action != Intent.ACTION_VIEW || uri.scheme != "construct" || uri.host != "oauth") return
        vm.handleAuthRedirect(uri.toString())
    }
}

/**
 * Opens the authorize URL in the browser (ACTION_VIEW, NEW_TASK; Custom Tabs not needed). Called on the
 * engine's auth thread: posts to the main thread and waits up to 5 s for the result. False when no activity
 * can handle the URL, so the engine reports "no browser available".
 */
internal class AndroidUrlOpener(private val context: Context) : UrlOpener {
    override fun open(url: String): Boolean {
        val intent = Intent(Intent.ACTION_VIEW, Uri.parse(url)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        // Already on the main thread: start directly. Posting and then waiting here would block the main
        // looper for the whole timeout and report false although the browser opens right after.
        if (Looper.myLooper() == Looper.getMainLooper()) return start(intent)
        val ok = AtomicBoolean(false)
        val done = CountDownLatch(1)
        Handler(Looper.getMainLooper()).post {
            try {
                ok.set(start(intent))
            } finally {
                done.countDown()
            }
        }
        return done.await(5, TimeUnit.SECONDS) && ok.get()
    }

    private fun start(intent: Intent): Boolean = try {
        context.startActivity(intent)
        true
    } catch (e: ActivityNotFoundException) {
        Log.w(TAG, "no activity can open the sign-in URL")
        false
    } catch (e: Exception) {
        Log.w(TAG, "cannot open the sign-in URL: $e")
        false
    }
}

internal data class EngineConfig(val storageDir: File, val baseUrl: String, val workspace: String) {
    companion object {
        /**
         * Base URL: intent extra (debug builds only), else the server saved by the menu's picker ([ServerPrefs]), else
         * the BuildConfig default (-PconstructBaseUrl at build time; default the staging pod). Workspace: intent extra
         * (debug only), else BuildConfig (-PconstructWorkspace; default `pixelfed_home`). Extras are honored only in
         * debug builds: the activity is exported (launcher), so in a release build any other app could otherwise point
         * the engine at an arbitrary server.
         */
        fun from(intent: Intent?, storageDir: File, savedBaseUrl: String? = null): EngineConfig {
            val extras = if (BuildConfig.DEBUG) intent else null
            val baseUrl = (
                extras?.getStringExtra(EXTRA_BASE_URL)?.takeIf { it.isNotBlank() }
                    ?: savedBaseUrl
                    ?: BuildConfig.CONSTRUCT_BASE_URL
                ).trimEnd('/')
            return EngineConfig(
                storageDir = storageDirFor(storageDir, baseUrl),
                baseUrl = baseUrl,
                workspace = extras?.getStringExtra(EXTRA_WORKSPACE)?.takeIf { it.isNotBlank() } ?: BuildConfig.CONSTRUCT_WORKSPACE,
            )
        }

        /**
         * The build-default server keeps `filesDir`; any other server gets its own `filesDir/servers/<sanitized url>`
         * so one server's cached entities and PENDING outbox rows are never shown for, or flushed to, another (D29).
         * It also means a picker switch never opens the old engine's `cache.db` while that engine is still closing.
         * All apps of one server share its directory (cache.db keys workspaces by id).
         */
        internal fun storageDirFor(filesDir: File, baseUrl: String): File {
            if (baseUrl == BuildConfig.CONSTRUCT_BASE_URL.trimEnd('/')) return filesDir
            val name = baseUrl.replace(Regex("[^A-Za-z0-9.-]"), "_")  // e.g. http___10.0.2.2_8080
            return File(File(filesDir, "servers"), name)
        }
    }
}

/**
 * The server chosen in the menu (all builds), kept in SharedPreferences `server` / key `baseUrl`. Choosing the build
 * default clears it. Precedence: see [EngineConfig.from]. The pre-menu debug picker's choice (`debug_server`) is
 * still read once nothing newer is saved.
 */
internal object ServerPrefs {
    private const val PREFS = "server"
    private const val LEGACY_PREFS = "debug_server"
    private const val KEY = "baseUrl"

    fun options(): List<ServerOption> {
        val presets = listOf(
            ServerOption("staging", "Staging", BuildConfig.STAGING_BASE_URL),
            ServerOption("emulator", "Local emulator", "http://10.0.2.2:8080"),
            ServerOption("usb", "Local USB", "http://127.0.0.1:8080"),
        )
        val default = BuildConfig.CONSTRUCT_BASE_URL.trimEnd('/')
        return if (presets.any { it.url == default }) presets else presets + ServerOption("default", "Build default", default)
    }

    /** The saved choice if it is still a valid `http(s)://host[:port]` URL. */
    fun saved(context: Context): String? = sequenceOf(PREFS, LEGACY_PREFS)
        .firstNotNullOfOrNull { context.getSharedPreferences(it, Context.MODE_PRIVATE).getString(KEY, null) }
        ?.let(ServerUrls::parse)

    fun save(context: Context, url: String) {
        val default = url.trimEnd('/') == BuildConfig.CONSTRUCT_BASE_URL.trimEnd('/')
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().apply {
            if (default) remove(KEY) else putString(KEY, url.trimEnd('/'))
        }.commit()
        context.getSharedPreferences(LEGACY_PREFS, Context.MODE_PRIVATE).edit().clear().commit()
    }
}

/**
 * Owns the engines for the life of the activity *task entry* (DECISIONS D11): a ViewModel survives configuration
 * changes such as rotation, so the engine is created once per launch, not once per onCreate. The config is read from
 * the intent that created it. The controller gets an engine factory, so picking another app in the menu creates that
 * app's engine (same server and storage) and closes the previous one; picking another server replaces the whole
 * session in place ([switchServer]). When the activity finishes for good (back / task removed), [onCleared] closes the
 * current engine; dropping the Rust engine also ends its background fetch thread. The controller holds the current
 * bridge (and its JNA callback observers) strongly while the UI is alive.
 */
internal class ConstructViewModel(config: EngineConfig, private val appContext: Context) : ViewModel() {
    /** One server's engines: [controller] shows the current app and opens others (same server and storage). */
    class Session(val baseUrl: String, val controller: ConstructController)

    /** The current server's session; the menu's server picker replaces it ([switchServer]). */
    var session: Session by mutableStateOf(openSession(config))
        private set

    val controller: ConstructController get() = session.controller

    private fun openSession(config: EngineConfig): Session {
        Log.i(TAG, "starting base_url=${config.baseUrl} workspace=${config.workspace} storage=${config.storageDir}")
        config.storageDir.mkdirs()
        val open = { workspace: String -> openEngine(config, workspace) }
        return Session(config.baseUrl, ConstructController(open(config.workspace), config.workspace, open))
    }

    /** Sign-in (DECISIONS D19): Keystore-backed session store, the system browser, `construct://oauth` redirect. */
    private fun openEngine(config: EngineConfig, workspace: String): ConstructEngineBridge {
        Log.i(TAG, "opening engine for workspace=$workspace")
        return UniffiEngineBridge.create(
            config.storageDir.absolutePath,
            config.baseUrl,
            workspace,
            secureStore = KeystoreSecureStore(appContext),
            urlOpener = AndroidUrlOpener(appContext),
            redirectUri = AUTH_REDIRECT_URI,
        )
    }

    /**
     * Switches to [url]: a new session (engine on that server's own storage, same app), then the choice is saved and
     * the old engine closed. If the engine can't be created nothing changes (and nothing is saved). True if switched.
     */
    fun switchServer(url: String): Boolean {
        val current = session
        val baseUrl = url.trimEnd('/')
        if (baseUrl == current.baseUrl) {
            current.controller.menu.closeMenu()
            return false
        }
        val filesDir = appContext.filesDir
        val config = EngineConfig(
            EngineConfig.storageDirFor(filesDir, baseUrl), baseUrl,
            current.controller.currentAppId() ?: BuildConfig.CONSTRUCT_WORKSPACE,
        )
        val next = try {
            openSession(config)
        } catch (t: Throwable) {
            Log.w(TAG, "cannot start an engine for $baseUrl: $t")
            return false
        }
        ServerPrefs.save(appContext, baseUrl)
        Log.i(TAG, "server picker: switched to $baseUrl")
        session = next
        try {
            current.controller.close()
        } catch (t: Throwable) {
            Log.w(TAG, "closing the previous engine failed: $t")
        }
        return true
    }

    /** Forwards a `construct://oauth?...` redirect to the current engine's pending sign-in. */
    fun handleAuthRedirect(url: String): Boolean = controller.handleAuthRedirect(url)

    override fun onCleared() {
        Log.i(TAG, "closing engine")
        controller.close()
    }
}
