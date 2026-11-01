package org.genui.canvas.desktop

import org.genui.canvas.CanvasEngineBridge
import uniffi.client_ffi.EngineHandle
import uniffi.client_ffi.FfiStateSnapshot
import uniffi.client_ffi.StateObserver

/**
 * [CanvasEngineBridge] backed by the Rust engine through the UniFFI/JNA bindings.
 *
 * Holds strong references to both the [EngineHandle] and every registered [StateObserver]:
 * the Rust side only keeps a handle into UniFFI's callback map, so the Kotlin observer object
 * must stay reachable for as long as the engine may call it.
 */
class EngineBridge(private val handle: EngineHandle) : CanvasEngineBridge {
    private val observers = mutableListOf<StateObserver>()

    override fun subscribe(onState: (workspaceJson: String, entitiesJson: String, isOnline: Boolean) -> Unit) {
        val observer = object : StateObserver {
            override fun onStateUpdated(snapshot: FfiStateSnapshot) {
                onState(snapshot.workspaceJson, snapshot.entitiesJson, snapshot.isOnline)
            }
        }
        synchronized(observers) { observers += observer }
        handle.subscribe(observer)
    }

    override fun dispatch(intentJson: String) {
        println("[desktop] dispatch: $intentJson")
        handle.dispatch(intentJson)
    }
}
