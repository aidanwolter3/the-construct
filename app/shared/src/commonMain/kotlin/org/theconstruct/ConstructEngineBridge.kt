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
    /** Null until the engine has heard from the server or failed to (its cached snapshots don't mean offline). */
    val isOnline: Boolean?,
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
    /** Unread notifications (D52): `{"unread_total":N,"unread_by_app":{"app":n}}`. Parse with [NotificationsJson.parseUnread]. */
    val notificationsJson: String = "{\"unread_total\":0,\"unread_by_app\":{}}",
    /** Other layouts (decision `layouts-not-apps`): `{id: {"workspace","version","gone"}}`. Parse with [LayoutSnapshot.parseAll]. */
    val layoutsJson: String = "{}",
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

    /** Forgets the session on this device only (Settings' "Leave pod", D46). No-op for fakes without sign-in. */
    fun forgetSession() {}

    /** Releases the engine (called when the UI goes away for good). Default: nothing to release. */
    fun close() {}

    /**
     * Loads another layout into this engine (decision `layouts-not-apps`): its cached version at once, then fetched in
     * the background; it appears in [EngineSnapshot.layoutsJson]. Never blocks. Default: nothing (fakes with one layout).
     */
    fun openLayout(id: String) {}

    /** Fetches the root layout again (BLOCKING). Default: nothing. */
    fun refresh() {}

    // Apps and their version history (the hamburger menu's Apps section and version picker). All BLOCKING network
    // calls: call them off the UI thread (e.g. on Dispatchers.IO). Each returns the pod's JSON body unchanged or throws
    // [LayoutApiException] with a readable message. The defaults throw, so fakes without apps compile unchanged.

    /**
     * `GET /v1/apps?all=1`: `{"apps":[{"id","title","icon","kind","hidden","latest_version","updated_at"}]}`, hidden
     * apps included (`kind`/`hidden` are missing from older pods).
     */
    fun listApps(): String = throw appsUnavailable()

    /**
     * `GET /v1/apps/{appId}/versions`: one page, newest first: `{"app_id","latest_version","versions":[{"version",
     * "created_at","source","note"?}],"next_before"?}`; pass `next_before` as [before] for the next (older) page.
     * [limit] null = the pod's default (20).
     */
    fun appVersions(appId: String, before: Long? = null, limit: Int? = null): String = throw appsUnavailable()

    /** `GET /v1/apps/{appId}/versions/{version}`: the version's metadata plus its full `layout` (a workspace [UiNode]). */
    fun appVersion(appId: String, version: Long): String = throw appsUnavailable()

    /**
     * `GET /v1/auth/sign-ins` (Settings): `{"sign_ins":[{"id","title","server"?,"username"?,"apps":[{"id",
     * "title"}]}], "providers":[{"id","title","signed_in","intent"?,"server_placeholder"?}]}`; `{"sign_ins":[]}`
     * without a session. BLOCKING, like the apps calls. Default: none.
     */
    fun signIns(): String = """{"sign_ins":[]}"""

    /** `DELETE /v1/custom-apps/{appId}`: deletes the pod user's own app on every device. BLOCKING. */
    fun deleteCustomApp(appId: String): Unit = throw appsUnavailable()

    /** `POST /v1/custom-apps/{appId}/dismiss-update`: no more "has updates" notice up to [version]. BLOCKING. */
    fun dismissUpdate(appId: String, version: Long): Unit = throw appsUnavailable()

    /** `PUT /v1/apps/{appId}/hidden`: hides the app from every device's list, or shows it again. BLOCKING. */
    fun setAppHidden(appId: String, hidden: Boolean): Unit = throw appsUnavailable()

    // Notifications from apps (D52, docs/notifications.md). The blocking calls (list, mark read, channels, push key) are
    // like the apps calls: off the UI thread, pod JSON in, [LayoutApiException] out. The defaults make a fake without
    // notifications compile: listing throws "not available", the rest do nothing.

    /**
     * Sets (replaces) the listener for notifications that arrive LIVE (never for catch-up, lists or push): called on the
     * engine's events thread with the `Notification` JSON (`{id, app_id, channel, title, body, image_url?, target_uri?,
     * group_key?, created_at, read}`; parse with [NotificationsJson.parseNotification]).
     */
    fun setNotificationListener(onNotification: (String) -> Unit) {}

    /** What the pod shows for this device (`platform` e.g. "android", "desktop"; `name` e.g. the model). Non-blocking. */
    fun setDeviceInfo(platform: String, name: String) {}

    /** This installation's device id (stable across restarts); "" when unknown. */
    fun deviceId(): String = ""

    /**
     * One page of notifications, newest first: `{"notifications":[...],"unread_total","unread_by_app",next_before?}`
     * (pass `next_before` as [before] for the next page). BLOCKING. From the pod, else from the engine's cache.
     */
    fun listNotifications(before: String? = null, limit: Int = 50, appId: String? = null): String =
        throw notificationsUnavailable()

    /** Marks [ids] read (optimistic, offline-safe): `{"unread_total","unread_by_app"}`. BLOCKING. */
    fun markNotificationsRead(ids: List<String>): String = throw notificationsUnavailable()

    /** Marks every notification (of [appId], when given) read: `{"unread_total","unread_by_app"}`. BLOCKING. */
    fun markAllNotificationsRead(appId: String? = null): String = throw notificationsUnavailable()

    /**
     * `{"channels":[{app_id,id,title,description,default_enabled,importance,enabled}]}` with this device's choices.
     * BLOCKING.
     */
    fun notificationChannels(): String = throw notificationsUnavailable()

    /** Turns one of an app's channels on or off for this device. BLOCKING; throws [LayoutApiException]. */
    fun setChannelEnabled(appId: String, channel: String, enabled: Boolean) {}

    /** The pod's VAPID public key (base64url) for a Web Push subscription. BLOCKING; NotFound when it sends no push. */
    fun pushPublicKey(): String = throw notificationsUnavailable()

    /** Sends this device's Web Push subscription to the pod (remembered by the engine). BLOCKING. */
    fun registerPush(endpoint: String, p256dh: String, auth: String) {}

    /** Stops Web Push to this device. BLOCKING. */
    fun unregisterPush() {}

    /**
     * A decrypted Web Push message body: cached and counted; returns the `Notification` JSON, or null when the body is
     * not a notification. Never calls the [setNotificationListener] listener.
     */
    fun handlePushMessage(payloadJson: String): String? = null

    // Attachments (schema 9, D49): photos picked by `core://MediaPicker`.

    /**
     * Stages the image file at [path] for a later create's `attachments` (BLOCKING: decodes, scales and re-encodes it;
     * call it off the UI thread). Its local entity `construct://attachments/<id>` then appears in the snapshots. The
     * failure is an [AttachmentException] with a readable reason. Default: not available.
     */
    fun stageAttachment(path: String): Result<AttachmentInfo> =
        Result.failure(AttachmentException("Photos can't be added here."))

    /** Forgets a staged attachment no create took (its file and entity go). Default: nothing to forget. */
    fun discardAttachment(id: String) {}

    // Remembered form values (schema 10, docs/decisions/remembered-form-values.md): kept on this device per app (the
    // engine's) and form scope ([ROOT_FORM_SCOPE] or a screen id). Local storage only, quick enough for the UI thread.

    /** The remembered values of form [scope]: field -> value. Default: none. */
    fun rememberedFormValues(scope: String): Map<String, String> = emptyMap()

    /** Remembers one value of form [scope] (null forgets it). Default: not kept. */
    fun rememberFormValue(scope: String, field: String, value: String?) {}

    // Pinned versions (`action.device` `pin_version`): kept on this device per app (the engine's) and layout, so a pin
    // survives a restart; the engine forgets them when the device leaves the pod. Local storage only.

    /** The versions pinned on this device: one per layout, with the pinned layout's JSON. Default: none. */
    fun pinnedVersions(): List<StoredPin> = emptyList()

    /** Keeps [version] of [layoutId] pinned ([specJson]: its layout), or with null version forgets the pin. */
    fun setPinnedVersion(layoutId: String, version: Long?, specJson: String?) {}
}

/** A pinned version as the engine keeps it ([ConstructEngineBridge.pinnedVersions]). */
data class StoredPin(val layoutId: String, val version: Long, val specJson: String)

private fun appsUnavailable() = LayoutApiException("Apps aren't available here.")

private fun notificationsUnavailable() = LayoutApiException("Notifications aren't available here.")
