package org.theconstruct

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

// The notification inbox (D47): a bell over the top-right corner of the layout (badged with the unread count) opens a
// panel listing every app's notifications, newest first, by day; tapping one opens its app and target. The menu's
// "Notifications" entry lists the apps' channels with switches. Generic: apps are only ids, titles and icons from the
// apps catalog. Test tags are listed in README "Simulation mode".

/** End padding the workspace title gets so it stays clear of the bell (8 dp margin + 40 dp button + 8 dp). */
internal val BELL_BUTTON_INSET = 56.dp

/**
 * The inbox's and the notification settings' state, owned by [ConstructController] (survives rotation, like
 * [MenuState]). Loads run in the panels' effects on the background dispatcher; state changes on the UI thread.
 */
@Stable
class InboxState internal constructor(private val controller: ConstructController) {
    var open by mutableStateOf(false)
        private set

    /** The notifications loaded so far, newest first. */
    var items by mutableStateOf<List<AppNotification>>(emptyList())
        private set
    var nextBefore by mutableStateOf<String?>(null)
        private set
    var loaded by mutableStateOf(false)
        private set
    var loadingMore by mutableStateOf(false)
        private set
    var error by mutableStateOf<String?>(null)
        private set

    /** Bumped on every open and by Try again: the first-page load is keyed on it. */
    var token by mutableStateOf(0)
        private set

    /** The apps catalog (titles, icons) for the rows; empty while unknown. */
    var apps by mutableStateOf<List<AppSummary>>(emptyList())
        private set

    var channels: Load<List<NotificationChannelInfo>> by mutableStateOf(Load.Loading)
        private set
    var channelsToken by mutableStateOf(0)
        private set

    /** A channel switch that could not be saved (it was switched back). */
    var channelError by mutableStateOf<String?>(null)
        private set

    fun openInbox() {
        controller.menu.closeMenu()
        open = true
        token++
    }

    fun close() {
        open = false
    }

    fun retry() {
        token++
    }

    /** (Re)loads the newest page, keeping what is shown meanwhile. */
    internal suspend fun load() {
        error = null
        if (apps.isEmpty()) {
            apps = controller.apps.lastApps ?: try {
                controller.apps.apps()
            } catch (e: LayoutApiException) {
                emptyList()
            }
        }
        try {
            val page = controller.apps.notifications { NotificationsJson.parsePage(it.listNotifications(null, PAGE_SIZE, null)) }
            items = page.notifications
            nextBefore = page.nextBefore
            loaded = true
        } catch (e: LayoutApiException) {
            error = when (e.kind) {
                LayoutApiException.Kind.Unauthorized -> SIGNED_OUT
                else -> e.message
            }
        }
    }

    /** Appends the next (older) page. */
    fun loadMore(scope: CoroutineScope) {
        val before = nextBefore ?: return
        if (loadingMore) return
        loadingMore = true
        scope.launch {
            try {
                val page = controller.apps.notifications { NotificationsJson.parsePage(it.listNotifications(before, PAGE_SIZE, null)) }
                if (nextBefore == before) {
                    val seen = items.mapTo(HashSet()) { it.id }
                    items = items + page.notifications.filter { it.id !in seen }
                    nextBefore = page.nextBefore
                }
            } catch (e: LayoutApiException) {
                error = e.message
            } finally {
                loadingMore = false
            }
        }
    }

    /** Marks everything read (at once here; the engine is optimistic too). */
    fun markAllRead() {
        items = items.map { if (it.read) it else it.copy(read = true) }
        controller.markAllNotificationsRead()
    }

    /** A tapped row: shown read, then [ConstructController.openNotification] (which closes the inbox). */
    fun tap(n: AppNotification) {
        items = items.map { if (it.id == n.id) it.copy(read = true) else it }
        controller.openNotification(n.appId, n.targetUri, n.id)
    }

    /** The app's catalog entry, if known. */
    fun app(appId: String): AppSummary? = apps.firstOrNull { it.id == appId }

    fun retryChannels() {
        channelsToken++
    }

    internal suspend fun loadChannels() {
        if (apps.isEmpty()) {
            apps = controller.apps.lastApps ?: try {
                controller.apps.apps()
            } catch (e: LayoutApiException) {
                emptyList()
            }
        }
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
        channelError = null
        scope.launch {
            try {
                controller.apps.notifications { it.setChannelEnabled(channel.appId, channel.id, enabled) }
            } catch (e: LayoutApiException) {
                set(!enabled)
                channelError = "Couldn't change \"${channel.title}\": ${e.message}"
            }
        }
    }

    companion object {
        const val PAGE_SIZE = 30
        const val SIGNED_OUT = "Sign in to your pod to see notifications."
        const val EMPTY = "No notifications yet"
    }
}

/** The bell (`inbox-button`, "Notifications") with the unread count (`inbox-badge`). */
@Composable
internal fun BellButton(unread: Int, onClick: () -> Unit, modifier: Modifier = Modifier) {
    Box(modifier) {
        Surface(
            shape = CircleShape,
            color = MaterialTheme.colorScheme.surface.copy(alpha = 0.92f),
            shadowElevation = 2.dp,
            modifier = Modifier.size(40.dp),
        ) {
            IconButton(onClick = onClick, modifier = Modifier.testTag("inbox-button")) {
                Icon(ConstructIcons.Notifications, contentDescription = "Notifications", modifier = Modifier.size(22.dp))
            }
        }
        val text = unreadBadgeText(unread)
        if (text.isNotEmpty()) {
            Box(
                Modifier.align(Alignment.TopEnd).offset(x = 6.dp, y = (-4).dp).heightIn(min = 18.dp).widthIn(min = 18.dp)
                    .background(MaterialTheme.colorScheme.error, CircleShape)
                    .border(1.5.dp, Color.White, CircleShape)
                    .padding(horizontal = 4.dp)
                    .testTag("inbox-badge"),
                contentAlignment = Alignment.Center,
            ) {
                Text(text, color = Color.White, fontSize = 11.sp, fontWeight = FontWeight.SemiBold, maxLines = 1)
            }
        }
    }
}

/** The inbox panel (`inbox`) over everything, from the right edge (full width on a phone), with a scrim. */
@Composable
internal fun InboxPanel(controller: ConstructController) {
    val inbox = controller.inbox
    if (!inbox.open) return
    val unread by controller.unread.collectAsState()
    val scope = rememberCoroutineScope()
    PlatformBackHandler(enabled = true) { inbox.close() }
    LaunchedEffect(inbox.token) { inbox.load() }
    // Live arrivals and reads elsewhere change the count: refresh the first page.
    LaunchedEffect(unread.total) { if (inbox.loaded) inbox.load() }
    Box(Modifier.fillMaxSize()) {
        Box(
            Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.32f))
                .clickable(interactionSource = remember { MutableInteractionSource() }, indication = null) { inbox.close() },
        )
        Surface(
            Modifier.align(Alignment.TopEnd).fillMaxHeight().widthIn(max = 420.dp).fillMaxWidth().testTag("inbox"),
            color = MaterialTheme.colorScheme.surface,
            shadowElevation = 8.dp,
        ) {
            Column(Modifier.fillMaxSize()) {
                Row(Modifier.fillMaxWidth().padding(start = 20.dp, end = 4.dp, top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        "Notifications",
                        style = MaterialTheme.typography.titleLarge,
                        fontWeight = FontWeight.SemiBold,
                        modifier = Modifier.weight(1f).semantics { heading() },
                    )
                    if (inbox.items.any { !it.read } || unread.total > 0) {
                        TextButton(onClick = inbox::markAllRead, modifier = Modifier.testTag("inbox-mark-all")) { Text("Mark all read") }
                    }
                    IconButton(onClick = inbox::close, modifier = Modifier.testTag("inbox-close")) {
                        Icon(ConstructIcons.Close, contentDescription = "Close notifications", modifier = Modifier.size(22.dp))
                    }
                }
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.6f))
                InboxList(inbox, scope)
            }
        }
    }
}

@Composable
private fun InboxList(inbox: InboxState, scope: CoroutineScope) {
    val error = inbox.error
    when {
        !inbox.loaded && error == null -> {
            InboxMessage("Loading…", busy = true)
            return
        }
        !inbox.loaded && error != null -> {
            InboxMessage(error, onRetry = if (error == InboxState.SIGNED_OUT) null else inbox::retry)
            return
        }
        inbox.items.isEmpty() -> {
            InboxMessage(InboxState.EMPTY, onRetry = null)
            return
        }
    }
    val now = nowEpochSeconds()
    val groups = groupNotificationsByDay(inbox.items, now)
    val list = rememberLazyListState()
    // Load more when the end of the list comes into view.
    LaunchedEffect(list, inbox.nextBefore) {
        snapshotFlow { list.layoutInfo.visibleItemsInfo.lastOrNull()?.index to list.layoutInfo.totalItemsCount }
            .collect { (last, total) ->
                if (last != null && total > 0 && last >= total - 3 && inbox.nextBefore != null) inbox.loadMore(scope)
            }
    }
    LazyColumn(Modifier.fillMaxSize().testTag("inbox-list"), state = list) {
        if (error != null) {
            item(key = "error") {
                Text(
                    error,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(horizontal = 20.dp, vertical = 6.dp),
                )
            }
        }
        groups.forEach { (day, rows) ->
            item(key = "day:$day") {
                Text(
                    day,
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.padding(start = 20.dp, end = 20.dp, top = 14.dp, bottom = 4.dp).semantics { heading() },
                )
            }
            items(rows, key = { "n:${it.id}" }) { n -> NotificationRow(n, inbox.app(n.appId), now) { inbox.tap(n) } }
        }
        if (inbox.loadingMore) {
            item(key = "more") { InboxMessage("Loading more…", busy = true) }
        }
    }
}

@Composable
private fun NotificationRow(n: AppNotification, app: AppSummary?, now: Long, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().clickable(onClick = onClick).testTag("notification-row:${n.id}")
            .background(if (n.read) Color.Transparent else MaterialTheme.colorScheme.primary.copy(alpha = 0.06f))
            .padding(start = 16.dp, end = 16.dp, top = 10.dp, bottom = 10.dp),
        verticalAlignment = Alignment.Top,
    ) {
        Box(
            Modifier.size(40.dp).clip(CircleShape).background(MaterialTheme.colorScheme.secondaryContainer),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                appIcon(app?.icon.orEmpty()),
                contentDescription = app?.title ?: n.appId,
                tint = MaterialTheme.colorScheme.onSecondaryContainer,
                modifier = Modifier.size(22.dp),
            )
        }
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    n.title,
                    style = MaterialTheme.typography.bodyLarge,
                    fontWeight = if (n.read) FontWeight.Normal else FontWeight.SemiBold,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                Spacer(Modifier.width(8.dp))
                Text(
                    notificationTimeLabel(n.createdAt, now),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                )
                if (!n.read) {
                    Spacer(Modifier.width(6.dp))
                    Box(Modifier.size(8.dp).background(MaterialTheme.colorScheme.primary, CircleShape))
                }
            }
            if (n.body.isNotBlank()) {
                Text(
                    n.body,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Text(
                app?.title ?: n.appId,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.outline,
                maxLines = 1,
                modifier = Modifier.padding(top = 2.dp),
            )
        }
    }
}

@Composable
private fun InboxMessage(text: String, busy: Boolean = false, onRetry: (() -> Unit)? = null) {
    Column(Modifier.fillMaxWidth().padding(horizontal = 24.dp, vertical = 32.dp), horizontalAlignment = Alignment.CenterHorizontally) {
        if (busy) {
            CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
            Spacer(Modifier.size(8.dp))
        }
        Text(text, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.testTag("inbox-message"))
        if (onRetry != null) TextButton(onClick = onRetry) { Text("Try again") }
    }
}

/**
 * The menu's notification settings (`notification-settings`): each app's channels (titled from the apps catalog) with
 * switches (`channel:<app>.<id>`). [extra] is the platform's own rows (e.g. the OS permission), shown first.
 */
@Composable
internal fun NotificationSettings(controller: ConstructController, extra: (@Composable () -> Unit)?) {
    val inbox = controller.inbox
    val menu = controller.menu
    val scope = rememberCoroutineScope()
    LaunchedEffect(inbox.channelsToken) { inbox.loadChannels() }
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).testTag("notification-settings").padding(bottom = 16.dp)) {
        Row(Modifier.fillMaxWidth().padding(start = 4.dp, end = 8.dp, top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            IconButton(onClick = menu::back, modifier = Modifier.testTag("menu-back")) {
                Icon(ConstructIcons.ArrowBack, contentDescription = "Back", modifier = Modifier.size(22.dp))
            }
            Text(
                "Notifications",
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(1f).padding(start = 4.dp).semantics { heading() },
            )
        }
        extra?.invoke()
        inbox.channelError?.let {
            Text(
                it,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(horizontal = 20.dp, vertical = 4.dp).testTag("channel-error"),
            )
        }
        when (val c = inbox.channels) {
            Load.Loading -> Text(
                "Loading…",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 20.dp, vertical = 12.dp),
            )
            is Load.Failed -> Row(Modifier.fillMaxWidth().padding(start = 20.dp, end = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    c.message,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.weight(1f).testTag("channels-error"),
                )
                if (c.message != InboxState.SIGNED_OUT) TextButton(onClick = inbox::retryChannels) { Text("Try again") }
            }
            is Load.Ready -> {
                if (c.value.isEmpty()) {
                    Text(
                        "No app sends notifications yet.",
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.padding(horizontal = 20.dp, vertical = 12.dp),
                    )
                }
                c.value.groupBy { it.appId }.forEach { (appId, channels) ->
                    Text(
                        inbox.app(appId)?.title ?: appId,
                        style = MaterialTheme.typography.labelLarge,
                        color = MaterialTheme.colorScheme.primary,
                        modifier = Modifier.padding(start = 20.dp, end = 20.dp, top = 12.dp, bottom = 4.dp).semantics { heading() },
                    )
                    channels.forEach { ch ->
                        Row(
                            Modifier.fillMaxWidth().clickable { inbox.setChannelEnabled(ch, !ch.enabled, scope) }
                                .testTag("channel:${ch.appId}.${ch.id}")
                                .padding(start = 20.dp, end = 16.dp, top = 6.dp, bottom = 6.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Column(Modifier.weight(1f)) {
                                Text(ch.title, style = MaterialTheme.typography.bodyLarge)
                                if (ch.description.isNotBlank()) {
                                    Text(
                                        ch.description,
                                        style = MaterialTheme.typography.bodySmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        maxLines = 2,
                                        overflow = TextOverflow.Ellipsis,
                                    )
                                }
                            }
                            Spacer(Modifier.width(12.dp))
                            Switch(
                                checked = ch.enabled,
                                onCheckedChange = { inbox.setChannelEnabled(ch, it, scope) },
                                modifier = Modifier.testTag("channel-switch:${ch.appId}.${ch.id}"),
                            )
                        }
                    }
                }
            }
        }
    }
}
