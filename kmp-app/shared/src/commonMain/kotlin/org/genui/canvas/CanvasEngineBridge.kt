package org.genui.canvas

/**
 * One engine state snapshot (D6 shape). [workspaceJson] is the workspace node JSON (decode with
 * [CanvasJson.decodeNode]; `"null"` before the first load), [entitiesJson] the URI-keyed entities map
 * ([CanvasJson.decodeEntities]). [seq] strictly increases per engine; consumers drop snapshots whose
 * seq is not above the last applied one. [pendingActions] counts outbox rows not yet sent.
 * [queriesJson] and [authJson] are M6 additions, [agentJson] an M7 one and [layoutJson] a D27 one, with defaults so
 * older call sites keep compiling.
 */
data class EngineSnapshot(
    val workspaceJson: String,
    val entitiesJson: String,
    val isOnline: Boolean,
    val seq: Long,
    val pendingActions: Int,
    /** Workspace queries: `{query_key: [entity uri, ...]}` (M6); `"{}"` when none. */
    val queriesJson: String = "{}",
    /** Auth status: `{"state":"signed_out"|"pending"|"signed_in"|"error","username"?,"error"?}`. */
    val authJson: String = "{\"state\":\"signed_out\"}",
    /**
     * Layout agent status (M7): `{"state":"idle"|"running"|"done"|"error","message"?,"provider"?,"layout_version"?}`;
     * parse with [AgentInfo.parse].
     */
    val agentJson: String = "{\"state\":\"idle\"}",
    /**
     * The layout on display (D27): `{"layout_id"?,"layout_version"?,"revision"?}` (`layout_version` = the active
     * layout's own version, `revision` = the workspace revision); `"{}"` while unknown. Parse with [ActiveLayout.parse].
     */
    val layoutJson: String = "{}",
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

    /**
     * Delivers an OAuth redirect (custom-scheme mode, e.g. Android's `canvas://oauth?...`) to the pending
     * sign-in. True if it was accepted. Fakes without sign-in return false.
     */
    fun handleAuthRedirect(url: String): Boolean = false

    /** Signs out (non-blocking). No-op for fakes without sign-in. */
    fun logout() {}

    // Layout catalog (DECISIONS D26/D27). All BLOCKING network calls: call them off the UI thread (e.g. on
    // Dispatchers.IO). Each returns the pod's JSON body or throws [LayoutApiException] with a readable message.
    // The defaults throw, so fakes without a catalog compile unchanged.

    /** This workspace's layouts: `{"layouts":[{id,title,description,current_version,active,active_version?,...}],...}`. */
    fun listLayouts(): String = throw catalogUnavailable()

    /**
     * One page of [layoutId]'s history, newest first: `{"versions":[{version,created_at,source,prompt?,...}],
     * "next_before"?}`; pass `next_before` as [before] for the next page. [limit] null = the pod's default (100).
     */
    fun layoutVersions(layoutId: String, before: Int? = null, limit: Int? = null): String = throw catalogUnavailable()

    /** One version's metadata plus its full `layout` (a workspace [UiNode] JSON). */
    fun layoutVersion(layoutId: String, version: Int): String = throw catalogUnavailable()

    /**
     * Makes [layoutId] the workspace's active layout ([version] non-null: restore that version first). When it changed
     * the screen, the engine has already applied the new layout when this returns.
     */
    fun activateLayout(layoutId: String, version: Int? = null): String = throw catalogUnavailable()

    /** Appends a copy of [version] as [layoutId]'s newest version (applied at once when it is the active layout). */
    fun restoreLayoutVersion(layoutId: String, version: Int): String = throw catalogUnavailable()
}

private fun catalogUnavailable() = LayoutApiException("Layouts aren't available here.")
