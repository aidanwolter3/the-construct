package org.genui.canvas

/**
 * One engine state snapshot (D6 shape). [workspaceJson] is the workspace node JSON (decode with
 * [CanvasJson.decodeNode]; `"null"` before the first load), [entitiesJson] the URI-keyed entities map
 * ([CanvasJson.decodeEntities]). [seq] strictly increases per engine; consumers drop snapshots whose
 * seq is not above the last applied one. [pendingActions] counts outbox rows not yet sent.
 */
data class EngineSnapshot(
    val workspaceJson: String,
    val entitiesJson: String,
    val isOnline: Boolean,
    val seq: Long,
    val pendingActions: Int,
)

/**
 * Platform-neutral seam to the Rust engine (client-core via client-ffi). On desktop and Android it
 * is implemented by `org.genui.canvas.engine.UniffiEngineBridge` (jvmCommonMain) over the UniFFI/JNA
 * bindings; tests and previews can supply fakes.
 */
interface CanvasEngineBridge {
    /** Registers a listener for engine state. [onState] may be called from any thread. */
    fun subscribe(onState: (EngineSnapshot) -> Unit)

    /** Sends an intent JSON produced by [CanvasJson.encodeIntent]. */
    fun dispatch(intentJson: String)
}
