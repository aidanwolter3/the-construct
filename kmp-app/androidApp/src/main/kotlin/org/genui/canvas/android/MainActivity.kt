package org.genui.canvas.android

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
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import org.genui.canvas.CanvasApp
import org.genui.canvas.CanvasController
import org.genui.canvas.engine.UniffiEngineBridge
import uniffi.client_ffi.UrlOpener
import java.io.File
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

private const val TAG = "CanvasEngine"

/** Intent extras that override the BuildConfig defaults, e.g. `adb shell am start ... --es workspace pixelfed_demo`. */
private const val EXTRA_BASE_URL = "baseUrl"
private const val EXTRA_WORKSPACE = "workspace"

/** OAuth redirect URI (custom scheme); the manifest's intent filter routes it back to [MainActivity]. */
internal const val AUTH_REDIRECT_URI = "canvas://oauth"

/**
 * `launchMode="singleTask"` (manifest): the browser runs in its own task (ACTION_VIEW + NEW_TASK), so the
 * `canvas://oauth` redirect must come back to *this* instance (and its ViewModel's engine, which holds the
 * pending sign-in) via [onNewIntent]; singleTop would start a second activity in the browser's task.
 */
class MainActivity : ComponentActivity() {
    private lateinit var vm: CanvasViewModel

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
            viewModelFactory { initializer { CanvasViewModel(EngineConfig.from(intent, storageDir), appContext) } },
        )[CanvasViewModel::class.java]
        // Normally the redirect arrives through onNewIntent; this covers a cold start by the redirect (the
        // pending flow is gone then, so the engine just ignores it). Not on recreation (rotation): the intent
        // was already handled.
        if (savedInstanceState == null) handleAuthRedirect(intent)

        setContent {
            Box(Modifier.fillMaxSize().background(Color.White).safeDrawingPadding()) {
                CanvasApp(vm.controller)
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleAuthRedirect(intent)
    }

    private fun handleAuthRedirect(intent: Intent?) {
        val uri = intent?.data ?: return
        if (intent.action != Intent.ACTION_VIEW || uri.scheme != "canvas" || uri.host != "oauth") return
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
        val ok = AtomicBoolean(false)
        val done = CountDownLatch(1)
        Handler(Looper.getMainLooper()).post {
            try {
                context.startActivity(intent)
                ok.set(true)
            } catch (e: ActivityNotFoundException) {
                Log.w(TAG, "no activity can open the sign-in URL")
            } catch (e: Exception) {
                Log.w(TAG, "cannot open the sign-in URL: $e")
            } finally {
                done.countDown()
            }
        }
        return done.await(5, TimeUnit.SECONDS) && ok.get()
    }
}

internal data class EngineConfig(val storageDir: File, val baseUrl: String, val workspace: String) {
    companion object {
        /**
         * Intent extra, else the BuildConfig default (-PcanvasBaseUrl / -PcanvasWorkspace at build time).
         * Extras are honored only in debug builds: the activity is exported (launcher), so in a release build
         * any other app could otherwise point the engine at an arbitrary server.
         */
        fun from(intent: Intent?, storageDir: File): EngineConfig {
            val extras = if (BuildConfig.DEBUG) intent else null
            return EngineConfig(
                storageDir = storageDir,
                baseUrl = extras?.getStringExtra(EXTRA_BASE_URL)?.takeIf { it.isNotBlank() } ?: BuildConfig.CANVAS_BASE_URL,
                workspace = extras?.getStringExtra(EXTRA_WORKSPACE)?.takeIf { it.isNotBlank() } ?: BuildConfig.CANVAS_WORKSPACE,
            )
        }
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
internal class CanvasViewModel(config: EngineConfig, appContext: Context) : ViewModel() {
    private val bridge: UniffiEngineBridge
    val controller: CanvasController

    init {
        Log.i(TAG, "starting base_url=${config.baseUrl} workspace=${config.workspace} storage=${config.storageDir}")
        config.storageDir.mkdirs()
        // Sign-in (DECISIONS D19): Keystore-backed session store, the system browser, `canvas://oauth` redirect.
        bridge = UniffiEngineBridge.create(
            config.storageDir.absolutePath,
            config.baseUrl,
            config.workspace,
            secureStore = KeystoreSecureStore(appContext),
            urlOpener = AndroidUrlOpener(appContext),
            redirectUri = AUTH_REDIRECT_URI,
        )
        controller = CanvasController(bridge)
    }

    /** Forwards a `canvas://oauth?...` redirect to the engine's pending sign-in. */
    fun handleAuthRedirect(url: String): Boolean = bridge.handleAuthRedirect(url)

    override fun onCleared() {
        Log.i(TAG, "closing engine")
        bridge.handle.close()
    }
}
