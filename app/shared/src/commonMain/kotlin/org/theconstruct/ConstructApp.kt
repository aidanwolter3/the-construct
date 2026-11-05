package org.theconstruct

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonObject

/**
 * UI state derived from engine snapshots. [workspace] is the root layout, null until the engine has one; [layouts] are
 * the other layouts the engine holds (decision `layouts-not-apps`). [pendingActions] counts queued (unsent) outbox
 * mutations. [seq] is the seq of the last applied snapshot (-1 before the first). [queries] maps a List `query_key` to
 * ordered entity URIs and [auth] is the sign-in status. [activeLayout] names the root and its latest version.
 * [isOnline] is null while connectivity is unknown (no offline badge then).
 */
data class ConstructState(
    val workspace: UiNode? = null,
    val entities: Map<String, JsonObject> = emptyMap(),
    val queries: Map<String, List<String>> = emptyMap(),
    val auth: AuthInfo = AuthInfo(),
    val isOnline: Boolean? = null,
    val pendingActions: Int = 0,
    val error: String? = null,
    val seq: Long = -1,
    val activeLayout: ActiveLayout = ActiveLayout(),
    /** Data path loads (D34), from `loads_json`; another layout's is under `layout:<id>`. */
    val loads: Map<String, LoadState> = emptyMap(),
    /** The other layouts the engine holds, from `layouts_json`. */
    val layouts: Map<String, LayoutSnapshot> = emptyMap(),
)

/** How often the device's list of the pod's layouts (`local:layouts`) is reloaded, so new ones show up on their own. */
const val APPS_REFRESH_MS = 15_000L

/** The notice when the root layout was deleted. */
const val APP_DELETED = "This was deleted."

/**
 * Bridges one [ConstructEngineBridge] to Compose (decision `layouts-not-apps`: one engine serves every layout on
 * screen). [rootId] is the layout the engine opened (the pod's [HOME_APP] by default); every other layout is loaded into
 * the same engine with [openLayout] and presented on [navigation]'s back stack.
 *
 * Snapshot callbacks may arrive on any thread; [state] is a thread-safe [StateFlow]. A snapshot whose seq is not above
 * the last applied one is dropped (the check and the write are one atomic [MutableStateFlow.update]). Decode failures
 * are surfaced in [ConstructState.error] (keeping the last good layout) instead of throwing into the engine.
 *
 * Also holds what must outlive a composition (Android rotation; not process restart): the versions pinned per layout
 * ([pins]), the device data's pod data ([device]) and the back stack ([navigation]).
 *
 * Notifications (D52): [unread] follows the snapshots; [onNotification] hears live arrivals; [openNotification] presents
 * a notification's layout and opens its target. Blocking pod calls run on [io].
 */
class ConstructController internal constructor(
    private val bridge: ConstructEngineBridge,
    rootId: String?,
    io: CoroutineDispatcher,
) {
    constructor(bridge: ConstructEngineBridge, rootId: String? = null) : this(bridge, rootId, layoutIoDispatcher)

    private val _notice = MutableStateFlow<String?>(null)

    /** A one-line notice over the layouts, until dismissed. */
    val notice: StateFlow<String?> = _notice.asStateFlow()

    fun dismissNotice() {
        _notice.value = null
    }

    private val _state = MutableStateFlow(ConstructState())
    val state: StateFlow<ConstructState> = _state.asStateFlow()

    private val _appId = MutableStateFlow(rootId)

    /** The root layout's id when the platform named it (null: what the snapshots report). */
    val appId: StateFlow<String?> = _appId.asStateFlow()

    /** The root layout: the one the engine was opened on. */
    val rootId: String get() = _appId.value ?: _state.value.activeLayout.appId ?: HOME_APP

    /** The pod's home is the root: before a pod is open, the bundled sign-in layout shows instead. */
    val shellMode: Boolean = rootId == HOME_APP

    /** The apps / versions calls (cached version specs live as long as this controller). */
    val apps: AppRepository = AppRepository({ this.bridge })

    /** The pod data the device data shows (sign-ins, notifications, channels, versions). */
    val device: DeviceState = DeviceState(this)

    /** Photos staged by `core://MediaPicker` that no create has taken yet (D49). */
    val attachments: Attachments = Attachments({ this.bridge })

    /** The back stack of presented layouts and screens, with their form values. */
    val navigation: Navigation = Navigation(this)

    private val _deviceApps = MutableStateFlow<List<AppSummary>>(emptyList())

    /** The pod's layouts for the device data (`local:layouts`); reloaded by [refreshDeviceApps]. */
    val deviceApps: StateFlow<List<AppSummary>> = _deviceApps.asStateFlow()

    /** The platform name for `local://device` (from [setDeviceInfo]). */
    val platform: String get() = deviceInfo?.first ?: "unknown"

    /** Background work that must not block the caller (marking read, push registration); cancelled by [close]. */
    private val background = CoroutineScope(SupervisorJob() + io)

    /**
     * Platform hook for notifications that arrive LIVE while the engine runs (never catch-up, lists or push): e.g. post
     * an OS notification when the app is in the background. Called on the engine's events thread; exceptions are caught.
     */
    @Volatile
    var onNotification: ((AppNotification) -> Unit)? = null

    private val _unread = MutableStateFlow(UnreadCounts())

    /** Unread notifications (all of them), from the engine's snapshots. */
    val unread: StateFlow<UnreadCounts> = _unread.asStateFlow()

    @Volatile
    private var deviceInfo: Pair<String, String>? = null

    /** A screen to open in a presented layout once the engine has that layout ([openWhenLoaded]). */
    private data class PendingTarget(val layoutId: String, val targetUri: String)

    private val pendingTarget = MutableStateFlow<PendingTarget?>(null)

    /** Set by [close]: afterwards [pin] and [openLayout] do nothing. */
    @Volatile
    private var closed = false

    /** Whether [close] was called. */
    val isClosed: Boolean get() = closed

    private val _pins = MutableStateFlow<Map<String, PinnedVersion>>(emptyMap())

    /** The version pinned per layout on this device (`pin_version`), until [followLatest]. */
    val pins: StateFlow<Map<String, PinnedVersion>> = _pins.asStateFlow()

    private val _versionsApp = MutableStateFlow<String?>(null)

    /** The layout whose versions `local:versions` lists (`action.device` `show_versions`). */
    val versionsApp: StateFlow<String?> = _versionsApp.asStateFlow()

    private fun onLiveNotification(json: String) {
        val n = NotificationsJson.parseNotification(json) ?: run {
            println("[Construct] unreadable live notification dropped")
            return
        }
        try {
            onNotification?.invoke(n)
        } catch (t: Throwable) {
            println("[Construct] onNotification failed: $t")
        }
    }

    /** The root layout's id (kept for callers of the app era). */
    fun currentAppId(): String = rootId

    private fun onSnapshot(snapshot: EngineSnapshot) {
        val pre = _state.value
        if (snapshot.seq <= pre.seq) return // cheap pre-check; re-checked atomically below
        var decodeError: String? = null
        val decoded: ConstructState? = try {
            val trimmed = snapshot.workspaceJson.trim()
            val workspace = if (trimmed.isEmpty() || trimmed == "null") null else ConstructJson.decodeNode(trimmed)
            val e = snapshot.entitiesJson.trim()
            val entities = if (e.isEmpty() || e == "null") emptyMap() else ConstructJson.decodeEntities(e)
            ConstructState(
                workspace, entities, decodeQueries(snapshot.queriesJson), AuthInfo.parse(snapshot.authJson),
                snapshot.isOnline, snapshot.pendingActions, error = null, seq = snapshot.seq,
                activeLayout = ActiveLayout.parse(snapshot.layoutJson),
                loads = LoadState.parseAll(snapshot.loadsJson),
                layouts = LayoutSnapshot.parseAll(snapshot.layoutsJson),
            )
        } catch (t: Throwable) {
            println("[Construct] snapshot decode failed: $t")
            decodeError = "Failed to decode engine state: ${t.message}"
            null
        }
        // Remembered form values (schema 10) are in the form before the layout first draws.
        if (_pins.value[rootId] == null) decoded?.workspace?.let(navigation::restoreRoot)
        _state.update { cur ->
            when {
                snapshot.seq <= cur.seq -> cur
                decoded != null -> decoded.copy(isOnline = decoded.isOnline ?: cur.isOnline)
                else -> cur.copy(
                    auth = AuthInfo.parse(snapshot.authJson),
                    isOnline = snapshot.isOnline ?: cur.isOnline,
                    pendingActions = snapshot.pendingActions,
                    error = decodeError,
                    seq = snapshot.seq,
                    activeLayout = ActiveLayout.parse(snapshot.layoutJson),
                    loads = LoadState.parseAll(snapshot.loadsJson),
                )
            }
        }
        val now = _state.value
        if (now.seq == snapshot.seq) {
            if (now.auth.session && !pre.auth.session) refreshDeviceApps()
            val unread = NotificationsJson.parseUnread(snapshot.notificationsJson)
            if (unread.total != _unread.value.total) reloadDeviceData()
            _unread.value = unread
            applyPendingTarget()
            if (now.activeLayout.gone && !pre.activeLayout.gone) _notice.value = APP_DELETED
        }
    }

    // -----------------------------------------------------------------------------------------------------------------
    // Layouts
    // -----------------------------------------------------------------------------------------------------------------

    /** Asks the engine for layout [id] (the root is the engine's own): cached at once, then fetched. */
    fun openLayout(id: String) {
        if (closed || id.isBlank() || id == rootId) return
        try {
            bridge.openLayout(id)
        } catch (t: Throwable) {
            println("[Construct] opening layout $id failed: $t")
        }
    }

    /** Layout [layoutId] as drawn: its pinned version, else the engine's latest (null until the engine has it). */
    fun workspaceOf(
        layoutId: String,
        state: ConstructState = _state.value,
        pins: Map<String, PinnedVersion> = _pins.value,
    ): UiNode? {
        pins[layoutId]?.let { return it.layout }
        return if (layoutId == rootId) state.workspace else state.layouts[layoutId]?.workspace
    }

    /** Opens [targetUri] in presented layout [layoutId] once the engine has that layout. */
    internal fun openWhenLoaded(layoutId: String, targetUri: String) {
        pendingTarget.value = PendingTarget(layoutId, targetUri)
        applyPendingTarget()
    }

    private fun applyPendingTarget() {
        val p = pendingTarget.value ?: return
        if (navigation.stack.none { it.layoutId == p.layoutId } && p.layoutId != rootId) {
            pendingTarget.compareAndSet(p, null) // the layout was closed meanwhile
            return
        }
        val workspace = workspaceOf(p.layoutId) ?: run {
            if (_state.value.layouts[p.layoutId]?.gone == true) pendingTarget.compareAndSet(p, null)
            return
        }
        if (!pendingTarget.compareAndSet(p, null)) return
        if (workspace is UiNode.Workspace && workspace.screens.any { matchRoute(it.route, p.targetUri) != null }) {
            navigation.openScreen(p.targetUri, p.layoutId)
        }
    }

    // -----------------------------------------------------------------------------------------------------------------
    // Intents
    // -----------------------------------------------------------------------------------------------------------------

    /**
     * Binds the visible entry's `$form.<field>` values and a fresh `$nonce` into [intent] (nothing is sent when a
     * referenced field is blank), resolves navigation and device intents here, and sends everything else to the engine.
     */
    fun dispatch(intent: ActionIntent) {
        val bound = bindForm(intent, navigation.activeForm()) ?: return
        run(bound, navigation.currentLayout())
    }

    /** [dispatch] from layout [layoutId], whose form is [form] (a presented or embedded layout, a screen). */
    fun dispatchIn(layoutId: String, form: Map<String, String>, intent: ActionIntent) {
        val bound = bindForm(intent, form) ?: return
        run(bound, layoutId)
    }

    /** [dispatch] with `$form` values from [form] (the root layout's own form). */
    fun dispatchWithForm(intent: ActionIntent, form: Map<String, String>) {
        val bound = bindForm(intent, form) ?: return
        run(bound, rootId)
    }

    /** Carries out a bound intent from layout [from]; the steps of an `action.sequence` one after another. */
    private fun run(bound: ActionIntent, from: String) {
        if (bound is ActionIntent.Sequence) {
            bound.steps.forEach { run(it, from) }
            return
        }
        if (bound is ActionIntent.OpenLink) {
            openLink(bound.url)
            return
        }
        if (bound is ActionIntent.Device) {
            device(bound)
            return
        }
        if (navigation.handle(bound, from)) {
            if (bound is ActionIntent.Navigate && bound.layout != null) reloadDeviceData()
            return
        }
        send(bound)
        if (bound is ActionIntent.Create) {
            attachments.consume(attachmentIds(bound.attachments))
        }
    }

    private val _openedLinks = MutableStateFlow<List<String>>(emptyList())

    /** The links `action.link.open` opened, oldest first (the simulator's `assert-link` reads them). */
    val openedLinks: StateFlow<List<String>> = _openedLinks.asStateFlow()

    /** Opens a link in the platform's browser; [ConstructApp] sets it from `LocalUriHandler`. */
    @Volatile
    internal var linkOpener: (String) -> Unit = {}

    /** `action.link.open`: an `http(s)://` [url] (a binding that came out blank or another scheme does nothing). */
    private fun openLink(url: String) {
        val u = url.trim()
        if (!(u.startsWith("https://") || u.startsWith("http://"))) {
            println("[Construct] action.link.open: not an http(s) link ($u); ignored")
            return
        }
        _openedLinks.value = _openedLinks.value + u
        try {
            linkOpener(u)
        } catch (t: Throwable) {
            println("[Construct] opening $u failed: $t")
        }
    }

    /** Encodes [intent] and forwards it to the engine as is. */
    internal fun send(intent: ActionIntent) {
        val json = ConstructJson.encodeIntent(intent)
        try {
            bridge.dispatch(json)
        } catch (t: Throwable) {
            println("[Construct] dispatch failed: $t")
            _state.update { it.copy(error = "Dispatch failed: ${t.message}") }
        }
    }

    /**
     * `action.device` (feature `device_action`): Settings and the inbox (presented layouts), refresh, leave the pod,
     * notifications, channels, sign-ins, the server, and a layout's versions. Unknown and retired commands (`open_app`,
     * `show`) are logged and ignored.
     */
    private fun device(intent: ActionIntent.Device) {
        when (intent.command) {
            "open_settings" -> navigation.present(SETTINGS_LAYOUT, presentation = PresentationMode.Panel)
            "open_inbox" -> navigation.present(INBOX_LAYOUT, presentation = PresentationMode.Sheet)
            "refresh" -> {
                refreshDeviceApps()
                reloadDeviceData()
                background.launch { bridge.refresh() }
                navigation.stack.map { it.layoutId }.distinct().forEach(::openLayout)
            }
            "leave_pod" -> {
                navigation.clear()
                _deviceApps.value = emptyList()
                _pins.value = emptyMap()
                leavePod()
            }
            "open_notification" -> {
                val n = device.notifications.firstOrNull { it.id == intent.id } ?: return
                device.tap(n)
                reloadDeviceData()
            }
            "mark_all_read" -> {
                device.markAllRead()
                reloadDeviceData()
            }
            "toggle_channel" -> {
                val c = (device.channels as? Load.Ready)?.value
                    ?.firstOrNull { it.appId == intent.app && it.id == intent.id } ?: return
                device.setChannelEnabled(c, !c.enabled, background)
            }
            "sign_in" -> {
                val provider = device.providers.firstOrNull { it.id == intent.id } ?: return
                val start = provider.intent ?: return
                // Another server, when the layout passes one (a typed `$form?.server`); else the provider's default.
                val server = intent.value?.trim().orEmpty()
                send(if (server.isNotEmpty() && provider.serverPlaceholder != null) start.copy(instanceUrl = server) else start)
            }
            "sign_out" -> {
                logout()
                reloadDeviceData()
            }
            "show_versions" -> {
                val layout = intent.app?.trim().orEmpty()
                if (layout.isEmpty()) return
                _versionsApp.value = layout
                background.launch { device.loadHistory(layout) }
            }
            "pin_version" -> {
                val layout = intent.app?.trim()?.takeIf { it.isNotEmpty() } ?: _versionsApp.value ?: return
                val version = intent.id?.trim()?.toLongOrNull() ?: return
                device.selectVersion(layout, version, background)
            }
            "follow_latest" -> {
                val layout = intent.app?.trim()?.takeIf { it.isNotEmpty() } ?: _versionsApp.value ?: return
                followLatest(layout)
            }
            "set_server" -> {
                val choice = serverChoice ?: return
                choice.options.firstOrNull { it.key == intent.id }?.let { choice.onSelect(it.url) }
            }
            "open_app", "show" -> println("[Construct] action.device ${intent.command} was retired (decision `layouts-not-apps`); ignored")
            else -> println("[Construct] action.device: unknown command '${intent.command}'; ignored")
        }
    }

    /** The platform's servers, for `local:servers` and `set_server` (set by [ConstructApp]; null: none to pick). */
    @Volatile
    internal var serverChoice: ServerChoice? = null

    private val _deviceDataToken = MutableStateFlow(0)

    /**
     * Bumped when the device data's pod data (notifications, channels, sign-ins) should be loaded again: when a layout
     * is presented, when the unread count changes, and after a device action that changes it.
     */
    val deviceDataToken: StateFlow<Int> = _deviceDataToken.asStateFlow()

    internal fun reloadDeviceData() {
        _deviceDataToken.update { it + 1 }
    }

    /** Reloads the pod's layouts for the device data. */
    fun refreshDeviceApps() {
        if (closed) return
        background.launch {
            _deviceApps.value = try {
                apps.apps()
            } catch (e: Throwable) {
                println("[Construct] loading the layouts for the device data failed: $e")
                return@launch
            }
        }
    }

    /** Signs out (Settings' "Sign out"); the engine notifies `signed_out` and refetches. */
    fun logout() {
        try {
            bridge.logout()
        } catch (t: Throwable) {
            println("[Construct] logout failed: $t")
            _state.update { it.copy(error = "Sign-out failed: ${t.message}") }
        }
    }

    /** Leaves the pod on this device (Settings' "Leave pod", D46): the engine forgets its session and refetches. */
    fun leavePod() {
        try {
            bridge.forgetSession()
        } catch (t: Throwable) {
            println("[Construct] leaving the pod failed: $t")
            _state.update { it.copy(error = "Leaving the pod failed: ${t.message}") }
        }
    }

    /** Starts the root again (Try again on the rescue screen). */
    fun refreshRoot() {
        if (closed) return
        background.launch { bridge.refresh() }
    }

    /** Forwards an OAuth redirect to the engine ([ConstructEngineBridge.handleAuthRedirect]). */
    fun handleAuthRedirect(url: String): Boolean = bridge.handleAuthRedirect(url)

    /** Closes the engine (the platform calls this when the UI goes away for good). Idempotent. */
    fun close() {
        if (closed) return
        closed = true
        pendingTarget.value = null
        background.cancel()
        bridge.close()
    }

    // -----------------------------------------------------------------------------------------------------------------
    // Notifications (D52)
    // -----------------------------------------------------------------------------------------------------------------

    /** What the pod shows for this device (e.g. "android", the model). */
    fun setDeviceInfo(platform: String, name: String) {
        deviceInfo = platform to name
        try {
            bridge.setDeviceInfo(platform, name)
        } catch (t: Throwable) {
            println("[Construct] setDeviceInfo failed: $t")
        }
    }

    /** Sends this device's Web Push subscription to the pod, in the background (errors are logged). */
    fun registerPush(endpoint: String, p256dh: String, auth: String) {
        inBackground("register push") { it.registerPush(endpoint, p256dh, auth) }
    }

    /** Stops Web Push to this device, in the background (errors are logged). */
    fun unregisterPush() {
        inBackground("unregister push") { it.unregisterPush() }
    }

    /** The pod's VAPID public key for a Web Push subscription; null on any error. BLOCKING: call off the main thread. */
    fun pushPublicKey(): String? = try {
        bridge.pushPublicKey().takeIf { it.isNotBlank() }
    } catch (t: Throwable) {
        println("[Construct] push public key: $t")
        null
    }

    /** A decrypted Web Push body, handed to the engine (cached, counted); the notification, or null. */
    fun handlePushMessage(payloadJson: String): AppNotification? = try {
        bridge.handlePushMessage(payloadJson)?.let(NotificationsJson::parseNotification)
    } catch (t: Throwable) {
        println("[Construct] push message: $t")
        null
    }

    /** Every notification channel with this device's choices; empty on any error. BLOCKING: off the main thread. */
    fun notificationChannels(): List<NotificationChannelInfo> = try {
        NotificationsJson.parseChannels(bridge.notificationChannels())
    } catch (t: Throwable) {
        println("[Construct] notification channels: $t")
        emptyList()
    }

    /** [appId]'s title from the layouts list, when it has been loaded; else null. */
    fun appTitle(appId: String): String? =
        (_deviceApps.value.takeIf { it.isNotEmpty() } ?: apps.lastApps)?.firstOrNull { it.id == appId }?.title

    /** Marks [ids] read in the background; [unread] follows at once. */
    fun markNotificationsRead(ids: List<String>) {
        if (ids.isEmpty()) return
        inBackground("mark read") { _unread.value = NotificationsJson.parseUnread(it.markNotificationsRead(ids)) }
    }

    /** Marks every notification (of [appId], when given) read in the background. */
    fun markAllNotificationsRead(appId: String? = null) {
        inBackground("mark all read") { _unread.value = NotificationsJson.parseUnread(it.markAllNotificationsRead(appId)) }
    }

    /**
     * Opens a notification (a tapped inbox row or OS notification): presents its [layout] (else [appId], the layout of
     * the blueprint that sent it; nothing is presented for the root), then, once the engine has that layout, opens
     * [targetUri] in it through the normal routing, and marks [notificationId] read. Works right after the controller
     * is created (cold start). Call it on the main thread.
     */
    fun openNotification(appId: String, targetUri: String?, notificationId: String?, layout: String? = null) {
        if (closed) return
        notificationId?.let { markNotificationsRead(listOf(it)) }
        val target = layout?.trim()?.takeIf { it.isNotEmpty() } ?: appId
        // The inbox (or anything over the root) closes; the notification's layout goes on the stack.
        navigation.stack.filterIsInstance<LayoutEntry>().firstOrNull { it.layoutId == INBOX_LAYOUT }
            ?.let { navigation.dismiss(it) }
        if (target != rootId) navigation.present(target, presentation = PresentationMode.Push)
        if (!targetUri.isNullOrBlank()) openWhenLoaded(target, targetUri.trim())
    }

    /** Runs [call] on the bridge off the caller's thread; errors are logged. Counted in [LayoutCalls]. */
    private fun inBackground(what: String, call: (ConstructEngineBridge) -> Unit) {
        if (closed) return
        LayoutCalls.add(1)
        background.launch {
            try {
                call(bridge)
            } catch (t: Throwable) {
                println("[Construct] $what failed: $t")
            }
        }.invokeOnCompletion { LayoutCalls.add(-1) }
    }

    /** Shows [version] of layout [layoutId] ([layout] = its spec) on this device until [followLatest]. */
    fun pin(layoutId: String, version: Long, layout: UiNode): Boolean {
        if (closed) return false
        if (layoutId == rootId) navigation.restoreRoot(layout)
        _pins.update { it + (layoutId to PinnedVersion(layoutId, version, layout)) }
        return true
    }

    /** Layout [layoutId] (the root by default) follows its latest version again. */
    fun followLatest(layoutId: String = rootId) {
        _pins.update { it - layoutId }
        if (layoutId == rootId) navigation.restoreRoot(_state.value.workspace)
    }

    /** The remembered values of form [scope] (schema 10); empty on any error. */
    internal fun rememberedFormValues(scope: String): Map<String, String> = try {
        bridge.rememberedFormValues(scope)
    } catch (t: Throwable) {
        println("[Construct] remembered form values: $t")
        emptyMap()
    }

    /** Saves one remembered form value (null forgets it); errors are logged. */
    internal fun rememberFormValue(scope: String, field: String, value: String?) {
        try {
            bridge.rememberFormValue(scope, field, value)
        } catch (t: Throwable) {
            println("[Construct] remembering $field: $t")
        }
    }

    // Last: a bridge may answer `subscribe` at once, and every field above must be ready for it.
    init {
        try {
            bridge.setNotificationListener { json -> onLiveNotification(json) }
        } catch (t: Throwable) {
            println("[Construct] notification setup failed: $t")
        }
        bridge.subscribe { onSnapshot(it) }
        // The pod's layouts change when the agent builds or removes one; look again now and then.
        background.launch {
            while (!closed) {
                delay(APPS_REFRESH_MS)
                if (_state.value.auth.session) refreshDeviceApps()
            }
        }
    }
}

/**
 * The shared root UI: the root layout (or, for the pod's home, the bundled sign-in and rescue layouts) under the back
 * stack of presented layouts and screens. [server] is the platform's server picker (null = none); [mediaPicker] the
 * platform's photo picker for `core://MediaPicker`; [notificationSettingsExtra] is drawn above the Settings layout
 * (e.g. Android's notification permission row).
 */
@Composable
fun ConstructApp(
    controller: ConstructController,
    modifier: Modifier = Modifier,
    server: ServerChoice? = null,
    mediaPicker: MediaPickerPlatform? = null,
    notificationSettingsExtra: (@Composable () -> Unit)? = null,
    openLink: ((String) -> Unit)? = null,
) {
    // `action.link.open` (feature `open_link`): the platform browser, unless the host opens links itself (simulator).
    val uriHandler = LocalUriHandler.current
    controller.linkOpener = openLink ?: { uriHandler.openUri(it) }
    val state by controller.state.collectAsState()
    val unread by controller.unread.collectAsState()
    val notice by controller.notice.collectAsState()
    // Material3 light scheme on a white background and surface (feed photos sit on white, as on Pixelfed/Instagram).
    MaterialTheme(colorScheme = lightColorScheme(background = Color.White, surface = Color.White)) {
        Surface(modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
            CompositionLocalProvider(
                LocalMediaPickerPlatform provides mediaPicker,
                LocalAttachments provides controller.attachments,
                LocalController provides controller,
            ) {
                val deviceApps by controller.deviceApps.collectAsState()
                val pins by controller.pins.collectAsState()
                val versionsApp by controller.versionsApp.collectAsState()
                val dataToken by controller.deviceDataToken.collectAsState()
                controller.serverChoice = server
                val d = controller.device
                // The pod data the device data shows: loaded with the session, and whenever asked.
                LaunchedEffect(dataToken, state.auth.session, state.auth.state) {
                    d.loadNotifications()
                    d.loadChannels()
                    d.loadSignIns()
                }
                val sources = DeviceSources(
                    notifications = d.notifications,
                    channels = (d.channels as? Load.Ready)?.value.orEmpty(),
                    signIns = (d.signIns as? Load.Ready)?.value.orEmpty(),
                    providers = d.providers,
                    server = server,
                    nowSeconds = remember(d.notifications, versionsApp) { nowEpochSeconds() },
                    pins = pins.mapValues { it.value.version },
                    versionsApp = versionsApp,
                    versions = versionsApp?.let { d.history[it] },
                )
                val device = remember(state.isOnline, state.pendingActions, state.auth, unread, deviceApps, sources) {
                    deviceData(state, unread, deviceApps, controller.platform, sources)
                }
                CompositionLocalProvider(LocalDeviceData provides device) {
                    Box(Modifier.fillMaxSize()) {
                        RootView(controller, state, device, Modifier.fillMaxSize())
                        StackHost(controller, state, device, notificationSettingsExtra)
                        notice?.let {
                            NoticeBar(it, onDismiss = controller::dismissNotice, modifier = Modifier.align(Alignment.BottomCenter))
                        }
                    }
                }
            }
        }
    }
}

/**
 * The root layout over the device data. For the pod's home: the bundled sign-in layout until a pod is open, and the
 * bundled rescue layout when the pod has no home this app can draw.
 */
@Composable
private fun RootView(controller: ConstructController, state: ConstructState, device: DeviceData, modifier: Modifier) {
    val pins by controller.pins.collectAsState()
    val workspace = controller.workspaceOf(controller.rootId, state, pins).takeUnless { state.activeLayout.gone }
    val node: UiNode? = if (controller.shellMode) {
        when (shellView(state)) {
            ShellView.Bootstrap -> BundledLayouts.bootstrap
            ShellView.Rescue -> BundledLayouts.rescue
            ShellView.Loading -> null
            ShellView.Home -> workspace
        }
    } else {
        workspace
    }
    if (node == null) {
        Column(modifier.padding(24.dp), verticalArrangement = Arrangement.Center, horizontalAlignment = Alignment.CenterHorizontally) {
            if (state.activeLayout.gone) {
                Text(APP_DELETED, style = MaterialTheme.typography.bodyLarge, modifier = Modifier.testTag("app-gone"))
            } else {
                CircularProgressIndicator()
                state.error?.let {
                    Text(
                        it,
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodySmall,
                        textAlign = TextAlign.Center,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
            }
        }
        return
    }
    val entities = remember(state.entities, device) { state.entities + device.entities }
    val queries = remember(state.queries, device) { state.queries + device.queries }
    CompositionLocalProvider(
        LocalConstructQueries provides queries,
        LocalForm provides controller.navigation.rootForm,
        LocalFormMemory provides remember(controller) {
            FormMemory { f, v -> controller.navigation.remember(ROOT_FORM_SCOPE, f, v) }
        },
        LocalLayoutChain provides listOf(controller.rootId),
    ) {
        RenderUiNode(
            node, entities,
            { controller.dispatchWithForm(it, controller.navigation.rootForm) },
            modifier.testTag(if (controller.shellMode) "shell" else "root"),
        )
    }
}

/** A one-line notice at the bottom (`notice`) with Dismiss (`notice-dismiss`). */
@Composable
private fun NoticeBar(text: String, onDismiss: () -> Unit, modifier: Modifier = Modifier) {
    Surface(
        color = MaterialTheme.colorScheme.inverseSurface,
        shape = RoundedCornerShape(12.dp),
        shadowElevation = 4.dp,
        modifier = modifier.padding(16.dp).fillMaxWidth().testTag("notice"),
    ) {
        Row(Modifier.padding(start = 16.dp, end = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(
                text,
                color = MaterialTheme.colorScheme.inverseOnSurface,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.weight(1f).padding(vertical = 12.dp),
            )
            TextButton(onClick = onDismiss, modifier = Modifier.testTag("notice-dismiss")) {
                Text("Dismiss", color = MaterialTheme.colorScheme.inversePrimary)
            }
        }
    }
}
