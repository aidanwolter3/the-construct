package org.genui.canvas.engine

import org.genui.canvas.CanvasEngineBridge
import org.genui.canvas.EngineSnapshot
import org.genui.canvas.LayoutApiException
import uniffi.client_ffi.EngineHandle
import uniffi.client_ffi.FfiStateSnapshot
import uniffi.client_ffi.SecureStore
import uniffi.client_ffi.StateObserver
import uniffi.client_ffi.UrlOpener

/**
 * [CanvasEngineBridge] backed by the Rust engine through the UniFFI/JNA bindings. Shared by the
 * desktop JVM and Android targets (`jvmCommonMain`, DECISIONS D5); platform differences (logging)
 * go through [engineLog].
 *
 * Holds strong references to both the [EngineHandle] and every registered [StateObserver]. (UniFFI's
 * generated callback handle map already pins each observer until Rust frees it; the list here is
 * defensive.) Callers must keep the bridge reachable for the life of the UI: if it and the handle
 * become unreachable, UniFFI's cleaner frees the Rust engine.
 */
class UniffiEngineBridge(val handle: EngineHandle) : CanvasEngineBridge {
    private val observers = mutableListOf<StateObserver>()

    override fun subscribe(onState: (EngineSnapshot) -> Unit) {
        val observer = object : StateObserver {
            override fun onStateUpdated(snapshot: FfiStateSnapshot) {
                onState(snapshot.toEngineSnapshot())
            }
        }
        synchronized(observers) { observers += observer }
        handle.subscribe(observer)
    }

    override fun dispatch(intentJson: String) {
        engineLog("dispatch: $intentJson")
        handle.dispatch(intentJson)
    }

    override fun handleAuthRedirect(url: String): Boolean {
        val accepted = handle.handleAuthRedirect(url)
        // Log only the scheme/host part: the query carries the one-time code.
        engineLog("auth redirect ${url.substringBefore('?')} accepted=$accepted")
        return accepted
    }

    override fun logout() {
        engineLog("logout")
        handle.logout()
    }

    override fun listLayouts(): String = catalog("list layouts") { handle.listLayouts() }

    override fun layoutVersions(layoutId: String, before: Int?, limit: Int?): String =
        catalog("versions of $layoutId") { handle.layoutVersions(layoutId, before?.toUIntArg(), limit?.toUIntArg()) }

    override fun layoutVersion(layoutId: String, version: Int): String =
        catalog("$layoutId v$version") { handle.layoutVersion(layoutId, version.toUIntArg()) }

    override fun activateLayout(layoutId: String, version: Int?): String =
        catalog("activate $layoutId${version?.let { " v$it" } ?: ""}") {
            handle.activateLayout(layoutId, version?.toUIntArg())
        }

    override fun restoreLayoutVersion(layoutId: String, version: Int): String =
        catalog("restore $layoutId v$version") { handle.restoreLayoutVersion(layoutId, version.toUIntArg()) }

    /** Runs a blocking catalog call, mapping the UniFFI error to a [LayoutApiException] with the engine's sentence. */
    private inline fun catalog(what: String, call: () -> String): String = try {
        call()
    } catch (e: uniffi.client_ffi.LayoutApiException) {
        val mapped = e.toLayoutApiException()
        engineLog("layouts: $what failed: ${mapped.kind} ${mapped.message}")
        throw mapped
    }

    companion object {
        /**
         * Creates the engine (which starts loading [workspace] from [baseUrl] in the background,
         * caching under [storageDir]) and wraps it. [storageDir] must exist and be writable.
         *
         * Sign-in (DECISIONS D19): [secureStore] null = the platform default (macOS Keychain, in-memory
         * elsewhere); [urlOpener] null = sign-in reports "no browser available"; [redirectUri] null = loopback
         * redirect (desktop), else a custom scheme (Android `canvas://oauth`) whose redirects the app passes
         * to [handleAuthRedirect].
         */
        fun create(
            storageDir: String,
            baseUrl: String,
            workspace: String,
            secureStore: SecureStore? = null,
            urlOpener: UrlOpener? = null,
            redirectUri: String? = null,
        ): UniffiEngineBridge {
            val handle = EngineHandle.newWithOptions(storageDir, baseUrl, workspace, secureStore, urlOpener, redirectUri)
            engineLog(
                "engine created base_url=$baseUrl workspace=$workspace redirect=${redirectUri ?: "loopback"} " +
                    "store=${if (secureStore == null) "default" else "app"} opener=${urlOpener != null}",
            )
            return UniffiEngineBridge(handle)
        }
    }
}

/**
 * u64 seq -> Long (clamped: a seq past Long.MAX_VALUE is unreachable in practice) and u32 pending count
 * -> Int (clamped likewise).
 */
internal fun FfiStateSnapshot.toEngineSnapshot(): EngineSnapshot = EngineSnapshot(
    workspaceJson = workspaceJson,
    entitiesJson = entitiesJson,
    isOnline = isOnline,
    seq = if (seq > Long.MAX_VALUE.toULong()) Long.MAX_VALUE else seq.toLong(),
    pendingActions = if (pendingActions > Int.MAX_VALUE.toUInt()) Int.MAX_VALUE else pendingActions.toInt(),
    queriesJson = queriesJson,
    authJson = authJson,
    agentJson = agentJson,
    layoutJson = layoutJson,
)

/** A negative version / limit can't be meant; 0 makes the pod answer 400 or 404 with a readable error. */
private fun Int.toUIntArg(): UInt = if (this < 0) 0u else toUInt()

internal fun uniffi.client_ffi.LayoutApiException.toLayoutApiException(): LayoutApiException = when (this) {
    is uniffi.client_ffi.LayoutApiException.Offline -> LayoutApiException(reason, LayoutApiException.Kind.Offline)
    is uniffi.client_ffi.LayoutApiException.Unauthorized ->
        LayoutApiException(reason, LayoutApiException.Kind.Unauthorized)
    is uniffi.client_ffi.LayoutApiException.NotFound -> LayoutApiException(reason, LayoutApiException.Kind.NotFound)
    is uniffi.client_ffi.LayoutApiException.Server ->
        LayoutApiException(reason, LayoutApiException.Kind.Server, status.toInt())
}

/** Platform log line: stdout with a `[desktop]` prefix on desktop, logcat tag `CanvasEngine` on Android. */
internal expect fun engineLog(message: String)
