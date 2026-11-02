package org.theconstruct

/**
 * One engine state snapshot (D6 shape). [workspaceJson] is the workspace node JSON (decode with
 * [ConstructJson.decodeNode]; `"null"` before the first load), [entitiesJson] the URI-keyed entities map
 * ([ConstructJson.decodeEntities]). [seq] strictly increases per engine; consumers drop snapshots whose
 * seq is not above the last applied one. [pendingActions] counts outbox rows not yet sent.
 * [queriesJson] and [authJson] are M6 additions and [layoutJson] a D27 one, with defaults so older call sites keep
 * compiling. [agentJson] is ignored (the AI client UI is gone; the field goes with the FFI's agent status).
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
    /** Former layout agent status (M7); ignored. */
    val agentJson: String = "{\"state\":\"idle\"}",
    /**
     * The layout on display: `{"app_id","version"?}` (`version` = the app's latest version, as applied from
     * `X-Construct-Layout-Version`; omitted on an unversioned screen such as the signed-out one); `"{}"` while unknown.
     * Parse with [ActiveLayout.parse] (which also reads the older D27 shape).
     */
    val layoutJson: String = "{}",
)

/**
 * Platform-neutral seam to the Rust engine (client-core via client-ffi). On desktop and Android it
 * is implemented by `org.theconstruct.engine.UniffiEngineBridge` (jvmCommonMain) over the UniFFI/JNA
 * bindings; tests and previews can supply fakes.
 */
interface ConstructEngineBridge {
    /** Registers a listener for engine state. [onState] may be called from any thread. */
    fun subscribe(onState: (EngineSnapshot) -> Unit)

    /** Sends an intent JSON produced by [ConstructJson.encodeIntent]. */
    fun dispatch(intentJson: String)

    /**
     * Delivers an OAuth redirect (custom-scheme mode, e.g. Android's `construct://oauth?...`) to the pending
     * sign-in. True if it was accepted. Fakes without sign-in return false.
     */
    fun handleAuthRedirect(url: String): Boolean = false

    /** Signs out (non-blocking). No-op for fakes without sign-in. */
    fun logout() {}

    /**
     * Releases the engine (called by [ConstructController.openApp] on the bridge it replaces, and by the platform when
     * the UI goes away for good). Default: nothing to release.
     */
    fun close() {}

    // Apps and their version history (the hamburger menu's Apps section and version picker). All BLOCKING network
    // calls: call them off the UI thread (e.g. on Dispatchers.IO). Each returns the pod's JSON body unchanged or throws
    // [LayoutApiException] with a readable message. The defaults throw, so fakes without apps compile unchanged.

    /** `GET /v1/apps`: `{"apps":[{"id","title","icon","latest_version","updated_at"}]}`. */
    fun listApps(): String = throw appsUnavailable()

    /**
     * `GET /v1/apps/{appId}/versions`: one page, newest first: `{"app_id","latest_version","versions":[{"version",
     * "created_at","source","note"?}],"next_before"?}`; pass `next_before` as [before] for the next (older) page.
     * [limit] null = the pod's default (20).
     */
    fun appVersions(appId: String, before: Long? = null, limit: Int? = null): String = throw appsUnavailable()

    /** `GET /v1/apps/{appId}/versions/{version}`: the version's metadata plus its full `layout` (a workspace [UiNode]). */
    fun appVersion(appId: String, version: Long): String = throw appsUnavailable()

    // Old per-layout catalog (D26/D27). No longer used by the UI; kept until the FFI drops it.

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

private fun appsUnavailable() = LayoutApiException("Apps aren't available here.")
