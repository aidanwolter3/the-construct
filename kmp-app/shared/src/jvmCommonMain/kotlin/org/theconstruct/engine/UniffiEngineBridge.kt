package org.theconstruct.engine

import org.theconstruct.AttachmentException
import org.theconstruct.AttachmentInfo
import org.theconstruct.ConstructEngineBridge
import org.theconstruct.EngineSnapshot
import org.theconstruct.LayoutApiException
import uniffi.client_ffi.EngineHandle
import uniffi.client_ffi.FfiStateSnapshot
import uniffi.client_ffi.NotificationListener
import uniffi.client_ffi.SecureStore
import uniffi.client_ffi.StateObserver
import uniffi.client_ffi.UrlOpener

/**
 * [ConstructEngineBridge] backed by the Rust engine through the UniFFI/JNA bindings. Shared by the
 * desktop JVM and Android targets (`jvmCommonMain`, DECISIONS D5); platform differences (logging)
 * go through [engineLog].
 *
 * Holds strong references to both the [EngineHandle] and every registered [StateObserver]. (UniFFI's
 * generated callback handle map already pins each observer until Rust frees it; the list here is
 * defensive.) Callers must keep the bridge reachable for the life of the UI: if it and the handle
 * become unreachable, UniFFI's cleaner frees the Rust engine.
 */
class UniffiEngineBridge(val handle: EngineHandle) : ConstructEngineBridge {
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
        if (closed) {
            engineLog("dispatch ignored (engine closed): $intentJson")
            return
        }
        engineLog("dispatch: $intentJson")
        handle.dispatch(intentJson)
    }

    override fun handleAuthRedirect(url: String): Boolean {
        if (closed) return false
        val accepted = handle.handleAuthRedirect(url)
        // Log only the scheme/host part: the query carries the one-time code.
        engineLog("auth redirect ${url.substringBefore('?')} accepted=$accepted")
        return accepted
    }

    override fun logout() {
        if (closed) return
        engineLog("logout")
        handle.logout()
    }

    override fun forgetSession() {
        if (closed) return
        engineLog("forget session")
        handle.forgetSession()
    }

    override fun listApps(): String = catalog("list apps") { handle.listApps() }

    override fun appVersions(appId: String, before: Long?, limit: Int?): String =
        catalog("versions of $appId") { handle.appVersions(appId, before, limit?.toUIntArg()) }

    override fun appVersion(appId: String, version: Long): String =
        catalog("$appId v$version") { handle.appVersion(appId, version) }

    override fun signIns(): String = catalog("sign-ins") { handle.signIns() }

    // Notifications (D52). The listener is held here too (UniFFI's handle map also pins it).
    @Volatile
    private var notificationListener: NotificationListener? = null

    override fun setNotificationListener(onNotification: (String) -> Unit) {
        if (closed) return
        val listener = object : NotificationListener {
            override fun onNotification(notificationJson: String) {
                try {
                    onNotification(notificationJson)
                } catch (t: Throwable) {
                    // Never throw into the engine's events thread.
                    engineLog("notification listener failed: $t")
                }
            }
        }
        notificationListener = listener
        handle.setNotificationListener(listener)
    }

    override fun setDeviceInfo(platform: String, name: String) {
        if (closed) return
        handle.setDeviceInfo(platform, name)
    }

    override fun deviceId(): String = if (closed) "" else handle.deviceId()

    override fun listNotifications(before: String?, limit: Int, appId: String?): String =
        catalog("list notifications") { handle.listNotifications(before, limit.toUIntArg(), appId) }

    override fun markNotificationsRead(ids: List<String>): String =
        catalog("mark ${ids.size} notification(s) read") { handle.markNotificationsRead(ids) }

    override fun markAllNotificationsRead(appId: String?): String =
        catalog("mark all notifications read") { handle.markAllNotificationsRead(appId) }

    override fun notificationChannels(): String = catalog("notification channels") { handle.notificationChannels() }

    override fun setChannelEnabled(appId: String, channel: String, enabled: Boolean) {
        catalog("channel $appId.$channel enabled=$enabled") {
            handle.setChannelEnabled(appId, channel, enabled)
            ""
        }
    }

    override fun pushPublicKey(): String = catalog("push public key") { handle.pushPublicKey() }

    override fun registerPush(endpoint: String, p256dh: String, auth: String) {
        // Log only the push service's host: the endpoint path is a capability.
        catalog("register push at ${endpoint.substringAfter("://").substringBefore('/')}") {
            handle.registerPush(endpoint, p256dh, auth)
            ""
        }
    }

    override fun unregisterPush() {
        catalog("unregister push") {
            handle.unregisterPush()
            ""
        }
    }

    override fun handlePushMessage(payloadJson: String): String? = if (closed) null else handle.handlePushMessage(payloadJson)

    override fun stageAttachment(path: String): Result<AttachmentInfo> {
        if (closed) return Result.failure(AttachmentException(CLOSED))
        return try {
            val info = AttachmentInfo.parse(handle.stageAttachment(path))
            engineLog("attachment ${info.id} staged (${info.width}x${info.height})")
            Result.success(info)
        } catch (e: uniffi.client_ffi.AttachmentException.Failed) {
            engineLog("staging an attachment failed: ${e.reason}")
            Result.failure(AttachmentException(e.reason))
        } catch (e: AttachmentException) {
            Result.failure(e)
        } catch (e: IllegalStateException) {
            // Closed by another thread meanwhile (UniFFI: "... object has already been destroyed").
            if (!closed) throw e
            Result.failure(AttachmentException(CLOSED))
        }
    }

    override fun discardAttachment(id: String) {
        if (closed) return
        engineLog("attachment $id discarded")
        handle.discardAttachment(id)
    }

    /**
     * Frees the Rust engine now (its fetch, sync and events threads stop; no more snapshots), instead of waiting for
     * the cleaner. Idempotent. Calls already running finish first (UniFFI counts them); later calls do nothing
     * (dispatch, logout), return false (handleAuthRedirect) or throw an Offline [LayoutApiException] (catalog).
     */
    override fun close() {
        if (closed) return
        closed = true
        engineLog("engine closed")
        synchronized(observers) { observers.clear() }
        notificationListener = null
        handle.close()
    }

    @Volatile
    private var closed = false

    /**
     * Runs a blocking catalog call, mapping the UniFFI error to a [LayoutApiException] with the engine's sentence
     * (and a call on a closed engine to an Offline one).
     */
    private inline fun catalog(what: String, call: () -> String): String {
        if (closed) throw LayoutApiException(CLOSED, LayoutApiException.Kind.Offline)
        return try {
            call()
        } catch (e: uniffi.client_ffi.LayoutApiException) {
            val mapped = e.toLayoutApiException()
            engineLog("pod call: $what failed: ${mapped.kind} ${mapped.message}")
            throw mapped
        } catch (e: IllegalStateException) {
            // Closed by another thread meanwhile (UniFFI: "... object has already been destroyed").
            if (!closed) throw e
            throw LayoutApiException(CLOSED, LayoutApiException.Kind.Offline)
        }
    }

    companion object {
        private const val CLOSED = "This app was closed."

        /**
         * Creates the engine (which starts loading [workspace] from [baseUrl] in the background,
         * caching under [storageDir]) and wraps it. [storageDir] must exist and be writable.
         *
         * Sign-in (DECISIONS D19): [secureStore] null = the platform default (macOS Keychain, in-memory
         * elsewhere); [urlOpener] null = sign-in reports "no browser available"; [redirectUri] null = loopback
         * redirect (desktop), else a custom scheme (Android `construct://oauth`) whose redirects the app passes
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
    layoutJson = layoutJson,
    loadsJson = loadsJson,
    notificationsJson = notificationsJson,
)

/** A negative limit can't be meant; 0 makes the pod answer 400 with a readable error. */
private fun Int.toUIntArg(): UInt = if (this < 0) 0u else toUInt()

internal fun uniffi.client_ffi.LayoutApiException.toLayoutApiException(): LayoutApiException = when (this) {
    is uniffi.client_ffi.LayoutApiException.Offline -> LayoutApiException(reason, LayoutApiException.Kind.Offline)
    is uniffi.client_ffi.LayoutApiException.Unauthorized ->
        LayoutApiException(reason, LayoutApiException.Kind.Unauthorized)
    is uniffi.client_ffi.LayoutApiException.NotFound -> LayoutApiException(reason, LayoutApiException.Kind.NotFound)
    is uniffi.client_ffi.LayoutApiException.Server ->
        LayoutApiException(reason, LayoutApiException.Kind.Server, status.toInt())
}

/** Platform log line: stdout with a `[desktop]` prefix on desktop, logcat tag `ConstructEngine` on Android. */
internal expect fun engineLog(message: String)
