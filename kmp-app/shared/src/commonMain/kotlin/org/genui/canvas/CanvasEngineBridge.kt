package org.genui.canvas

/**
 * Platform-neutral seam to the Rust engine (client-core via client-ffi). On desktop and Android it
 * is implemented by `org.genui.canvas.engine.UniffiEngineBridge` (jvmCommonMain) over the UniFFI/JNA
 * bindings; tests and previews can supply fakes.
 */
interface CanvasEngineBridge {
    /**
     * Registers a listener for engine state. [onState] receives the workspace node JSON
     * (decode with [CanvasJson.decodeNode]), the entities map JSON ([CanvasJson.decodeEntities]),
     * and connectivity.
     */
    fun subscribe(onState: (workspaceJson: String, entitiesJson: String, isOnline: Boolean) -> Unit)

    /** Sends an intent JSON produced by [CanvasJson.encodeIntent]. */
    fun dispatch(intentJson: String)
}
