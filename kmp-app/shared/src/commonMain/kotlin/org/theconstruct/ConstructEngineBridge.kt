package org.theconstruct

/**
 * One engine state snapshot (D6 shape). [workspaceJson] is the workspace node JSON (decode with
 * [ConstructJson.decodeNode]; `"null"` before the first load), [entitiesJson] the URI-keyed entities map
 * ([ConstructJson.decodeEntities]). [seq] strictly increases per engine; consumers drop snapshots whose
 * seq is not above the last applied one. [pendingActions] counts outbox rows not yet sent.
 * [queriesJson] and [authJson] are M6 additions and [layoutJson] a D27 one, with defaults so older call sites keep
 * compiling.
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
     * The layout on display: `{"app_id","version"?}` (`version` = the app's latest version, as applied from
     * `X-Construct-Layout-Version`; omitted on an unversioned screen such as the signed-out one); `"{}"` while unknown.
     * Parse with [ActiveLayout.parse].
     */
    val layoutJson: String = "{}",
    /** Data path loads (DECISIONS D34): `{path: {"state":"loading"|"ready"|"error","error"?}}`; `"{}"` when none. */
    val loadsJson: String = "{}",
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

    /** Forgets the session on this device only ("Leave this pod", D46). No-op for fakes without sign-in. */
    fun forgetSession() {}

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

    // Attachments (schema 8, D47): photos picked by `core://MediaPicker`.

    /**
     * Stages the image file at [path] for a later create's `attachments` (BLOCKING: decodes, scales and re-encodes it;
     * call it off the UI thread). Its local entity `construct://attachments/<id>` then appears in the snapshots. The
     * failure is an [AttachmentException] with a readable reason. Default: not available.
     */
    fun stageAttachment(path: String): Result<AttachmentInfo> =
        Result.failure(AttachmentException("Photos can't be added here."))

    /** Forgets a staged attachment no create took (its file and entity go). Default: nothing to forget. */
    fun discardAttachment(id: String) {}
}

private fun appsUnavailable() = LayoutApiException("Apps aren't available here.")
