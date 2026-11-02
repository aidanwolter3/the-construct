package org.genui.canvas.engine

import org.genui.canvas.CanvasEngineBridge
import org.genui.canvas.EngineSnapshot
import uniffi.client_ffi.EngineHandle
import uniffi.client_ffi.FfiStateSnapshot
import uniffi.client_ffi.StateObserver

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

    companion object {
        /**
         * Creates the engine (which starts loading [workspace] from [baseUrl] in the background,
         * caching under [storageDir]) and wraps it. [storageDir] must exist and be writable.
         */
        fun create(storageDir: String, baseUrl: String, workspace: String): UniffiEngineBridge {
            val handle = EngineHandle.newWithWorkspace(storageDir, baseUrl, workspace)
            engineLog("engine created base_url=$baseUrl workspace=$workspace")
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
)

/** Platform log line: stdout with a `[desktop]` prefix on desktop, logcat tag `CanvasEngine` on Android. */
internal expect fun engineLog(message: String)
