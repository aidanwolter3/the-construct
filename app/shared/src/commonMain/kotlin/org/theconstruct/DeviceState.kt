package org.theconstruct

import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

// What the device data (`local:` queries, Shell.kt) shows beyond the engine's snapshots, and the `action.device`
// commands that change it: the notifications, the notification channels, the sign-ins, a layout's version history.
// The Settings and inbox layouts draw it (decision `layouts-not-apps`); nothing here draws anything.

/** A load from the pod. */
sealed class Load<out T> {
    object Loading : Load<Nothing>()
    data class Ready<T>(val value: T) : Load<T>()
    data class Failed(val message: String) : Load<Nothing>()
}

/**
 * The pod data the device data shows, owned by [ConstructController] (it survives rotation). Loads run on the
 * controller's background dispatcher; state is Compose state.
 */
@Stable
class DeviceState internal constructor(private val controller: ConstructController) {
    /** The session's sign-ins (`local:sign_ins`). */
    var signIns: Load<List<SignInSummary>> by mutableStateOf(Load.Loading)
        private set

    /** The sign-ins the pod offers (`local:providers`). */
    var providers: List<SignInProvider> by mutableStateOf(emptyList())
        private set

    /** The version history of each layout `show_versions` named (`local:versions`). */
    val history = mutableStateMapOf<String, VersionHistory>()

    /** A version being fetched to pin, and the last failure. */
    var selecting by mutableStateOf<Long?>(null)
        private set
    var selectError by mutableStateOf<String?>(null)
        private set

    /** The newest notifications, newest first (`local:notifications`). */
    var notifications by mutableStateOf<List<AppNotification>>(emptyList())
        private set
    var notificationsError by mutableStateOf<String?>(null)
        private set

    /** The notification channels with this device's choices (`local:channels`). */
    var channels: Load<List<NotificationChannelInfo>> by mutableStateOf(Load.Loading)
        private set

    /** (Re)loads the sign-ins; a failed reload keeps the last list. */
    internal suspend fun loadSignIns() {
        if (signIns !is Load.Ready) signIns = Load.Loading
        try {
            val (listed, offered) = controller.apps.signInsAndProviders()
            signIns = Load.Ready(listed)
            providers = offered
        } catch (e: LayoutApiException) {
            if (signIns !is Load.Ready) signIns = Load.Failed(e.message)
        }
    }

    /** Loads the newest page of [layoutId]'s history (keeping what is shown meanwhile). */
    internal suspend fun loadHistory(layoutId: String) {
        val cur = history[layoutId] ?: VersionHistory()
        history[layoutId] = cur.copy(error = null)
        history[layoutId] = try {
            cur.withFirstPage(controller.apps.versions(layoutId))
        } catch (e: LayoutApiException) {
            cur.copy(error = e.message, loadingMore = false)
        }
    }

    /**
     * Shows [version] of [layoutId] on this device: fetches its spec (cached) and pins it. Nothing changes on the pod. A
     * failure is kept in [selectError].
     */
    fun selectVersion(layoutId: String, version: Long, scope: CoroutineScope) {
        if (selecting != null) return
        selecting = version
        selectError = null
        scope.launch(start = CoroutineStart.UNDISPATCHED) {
            withContext(NonCancellable) {
                try {
                    val spec = controller.apps.spec(layoutId, version)
                    if (!controller.isClosed) controller.pin(layoutId, version, spec)
                } catch (e: LayoutApiException) {
                    selectError = e.message
                } finally {
                    selecting = null
                }
            }
        }
    }

    /** (Re)loads the newest notifications, keeping what is shown meanwhile. */
    internal suspend fun loadNotifications() {
        notificationsError = null
        try {
            val page = controller.apps.notifications {
                NotificationsJson.parsePage(it.listNotifications(null, NOTIFICATIONS_PAGE, null))
            }
            notifications = page.notifications
        } catch (e: LayoutApiException) {
            notificationsError = when (e.kind) {
                LayoutApiException.Kind.Unauthorized -> SIGNED_OUT
                else -> e.message
            }
        }
    }

    /** Marks everything read (at once here; the engine is optimistic too). */
    fun markAllRead() {
        notifications = notifications.map { if (it.read) it else it.copy(read = true) }
        controller.markAllNotificationsRead()
    }

    /** A tapped notification: shown read, then opened ([ConstructController.openNotification]). */
    fun tap(n: AppNotification) {
        notifications = notifications.map { if (it.id == n.id) it.copy(read = true) else it }
        controller.openNotification(n.appId, n.targetUri, n.id, n.layout)
    }

    internal suspend fun loadChannels() {
        if (channels !is Load.Ready) channels = Load.Loading
        channels = try {
            Load.Ready(controller.apps.notifications { NotificationsJson.parseChannels(it.notificationChannels()) })
        } catch (e: LayoutApiException) {
            Load.Failed(if (e.kind == LayoutApiException.Kind.Unauthorized) SIGNED_OUT else e.message)
        }
    }

    /** Switches a channel on or off: shown at once, switched back when the pod refuses. */
    fun setChannelEnabled(channel: NotificationChannelInfo, enabled: Boolean, scope: CoroutineScope) {
        fun set(on: Boolean) {
            val cur = (channels as? Load.Ready)?.value ?: return
            channels = Load.Ready(cur.map { if (it.appId == channel.appId && it.id == channel.id) it.copy(enabled = on) else it })
        }
        set(enabled)
        scope.launch {
            try {
                controller.apps.notifications { it.setChannelEnabled(channel.appId, channel.id, enabled) }
            } catch (e: LayoutApiException) {
                set(!enabled)
                println("[Construct] couldn't change \"${channel.title}\": ${e.message}")
            }
        }
    }

    companion object {
        const val NOTIFICATIONS_PAGE = 30
        const val SIGNED_OUT = "Sign in to your pod to see notifications."
    }
}
