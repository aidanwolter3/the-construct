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

    /** The notifications loaded so far, newest first (`local:notifications`): the newest page, and older ones asked for. */
    var notifications by mutableStateOf<List<AppNotification>>(emptyList())
        private set
    var notificationsError by mutableStateOf<String?>(null)
        private set

    /** Where the next older page starts (`before`), or null when everything is loaded (`local://inbox#has_older`). */
    var olderBefore by mutableStateOf<String?>(null)
        private set

    /** Whether older pages were added since the list last started from the newest page. */
    private var loadedOlder = false

    /** Whether an older page is loading (`local://inbox#loading`). */
    var loadingOlder by mutableStateOf(false)
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

    /**
     * (Re)loads the newest notifications, keeping what is shown meanwhile. Older pages loaded before stay under the
     * newest page when it reaches them; otherwise the list starts again from the newest page.
     */
    internal suspend fun loadNotifications() {
        notificationsError = null
        try {
            val page = controller.apps.notifications {
                NotificationsJson.parsePage(it.listNotifications(null, NOTIFICATIONS_PAGE, null))
            }
            val merged = if (loadedOlder) withNewestPage(notifications, page) else null
            if (merged == null) {
                loadedOlder = false
                notifications = page.notifications
                olderBefore = page.nextBefore
            } else {
                notifications = merged
            }
        } catch (e: LayoutApiException) {
            notificationsError = when (e.kind) {
                LayoutApiException.Kind.Unauthorized -> SIGNED_OUT
                else -> e.message
            }
        }
    }

    /**
     * Appends the next older page (`load_older_notifications`). Does nothing while one loads or when everything is
     * loaded; a failure keeps the list and is kept in [notificationsError].
     */
    internal suspend fun loadOlderNotifications() {
        val before = olderBefore ?: return
        if (loadingOlder) return
        loadingOlder = true
        try {
            val page = controller.apps.notifications {
                NotificationsJson.parsePage(it.listNotifications(before, NOTIFICATIONS_PAGE, null))
            }
            // A refresh meanwhile may have started the list again: then this page doesn't follow it.
            if (olderBefore == before) {
                val known = notifications.map { it.id }.toSet()
                notifications = notifications + page.notifications.filter { it.id !in known }
                olderBefore = page.nextBefore
                loadedOlder = true
            }
        } catch (e: LayoutApiException) {
            notificationsError = e.message
        } finally {
            loadingOlder = false
        }
    }

    /** Marks one notification read (`mark_read`), shown at once; the engine lowers the unread count. */
    fun markRead(id: String) {
        val n = notifications.firstOrNull { it.id == id }
        if (n != null && !n.read) notifications = notifications.map { if (it.id == id) it.copy(read = true) else it }
        controller.markNotificationsRead(listOf(id))
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

        /**
         * [shown] (newest page plus older pages) with its newest part replaced by [page], when [page] reaches into what
         * is shown (its oldest notification is in [shown] with more after it); else null. Only for a list with older
         * pages loaded.
         */
        internal fun withNewestPage(shown: List<AppNotification>, page: NotificationPage): List<AppNotification>? {
            val oldest = page.notifications.lastOrNull() ?: return null
            val at = shown.indexOfFirst { it.id == oldest.id }
            if (at < 0 || at == shown.lastIndex) return null
            val ids = page.notifications.map { it.id }.toSet()
            return page.notifications + shown.drop(at + 1).filter { it.id !in ids }
        }

        const val SIGNED_OUT = "Sign in to your pod to see notifications."
    }
}
