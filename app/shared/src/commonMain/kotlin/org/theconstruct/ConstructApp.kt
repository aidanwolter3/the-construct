package org.theconstruct

import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.snapshots.SnapshotStateMap
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
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
 * UI state derived from engine snapshots. [workspace] is null until the engine has a layout.
 * [pendingActions] counts queued (unsent) outbox mutations. [seq] is the seq of the last applied
 * snapshot (-1 before the first). [queries] maps a FeedList `query_key` to ordered entity URIs (M6) and
 * [auth] is the sign-in status. [activeLayout] names the app on display and its latest version. [isOnline] is null
 * while connectivity is unknown (before the engine first hears from the server; no offline badge then). After
 * [ConstructController.openApp] the previous engine's answer stays until the new engine has one.
 * [engine] counts engine switches ([ConstructController.openApp]): a snapshot of a replaced engine is dropped.
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
    val engine: Int = 0,
    /** Data path loads (D34), from `loads_json`. */
    val loads: Map<String, LoadState> = emptyMap(),
)

/**
 * Bridges a [ConstructEngineBridge] to Compose. Snapshot callbacks may arrive on any thread;
 * [state] is a thread-safe [StateFlow]. A snapshot whose seq is not above the last applied one is
 * dropped (the engine notifies from several threads, so an older snapshot can arrive late); the check
 * and the write are one atomic [MutableStateFlow.update]. Decode failures are surfaced in
 * [ConstructState.error] (keeping the last good workspace/entities) instead of throwing into the engine.
 *
 * [appId] is the app (workspace) the engine shows, when the platform knows it. [openBridge], when given, creates an
 * engine for another app: [openApp] then swaps engines (the old one is [ConstructEngineBridge.close]d). Without it,
 * [openApp] only reports that other apps can't be opened here.
 *
 * Also holds what must outlive a composition (Android rotation; not process restart): the version the user pinned
 * ([pinned]), the menu's state ([menu]) and the notification inbox's ([inbox], D52).
 *
 * Notifications (D52): [unread] follows the snapshots; [onNotification] hears live arrivals of every engine this
 * controller holds; [openNotification] opens a notification's app and target. Blocking pod calls run on [io].
 */
/** How often the shell reloads the pod's app list (`local:apps`), so apps the agent builds show up on their own. */
const val APPS_REFRESH_MS = 15_000L

/** The notice when the app on screen was deleted. */
const val APP_DELETED = "This app was deleted."

class ConstructController internal constructor(
    bridge: ConstructEngineBridge,
    appId: String?,
    io: CoroutineDispatcher,
    private val defaultAppId: String? = null,
    private val openBridge: ((appId: String) -> ConstructEngineBridge)? = null,
) {
    /**
     * [defaultAppId] is the app to fall back to when the one on screen is deleted (a custom app, decision
     * `custom-apps-and-data-sources`); null: stay and say so.
     */
    constructor(
        bridge: ConstructEngineBridge,
        appId: String? = null,
        defaultAppId: String? = null,
        openBridge: ((appId: String) -> ConstructEngineBridge)? = null,
    ) : this(bridge, appId, layoutIoDispatcher, defaultAppId, openBridge)

    private val _notice = MutableStateFlow<String?>(null)

    /** A one-line notice over the app ("This app was deleted."), until dismissed or another app opens. */
    val notice: StateFlow<String?> = _notice.asStateFlow()

    fun dismissNotice() {
        _notice.value = null
    }

    @Volatile
    private var bridge: ConstructEngineBridge = bridge

    private val _state = MutableStateFlow(ConstructState())
    val state: StateFlow<ConstructState> = _state.asStateFlow()

    private val _appId = MutableStateFlow(appId)

    /** The app this controller's engine was opened for (null: unknown, then the snapshots' `app_id` is used). */
    val appId: StateFlow<String?> = _appId.asStateFlow()

    private val _pinned = MutableStateFlow<PinnedVersion?>(null)

    /** The version picked in the version picker, or null = follow the latest. */
    val pinned: StateFlow<PinnedVersion?> = _pinned.asStateFlow()

    /** Whether [openApp] can switch to another app. */
    val canOpenApps: Boolean get() = openBridge != null

    /** The apps / versions calls (cached version specs live as long as this controller). */
    val apps: AppRepository = AppRepository({ this.bridge })

    /** The hamburger menu's state: kept here so an open drawer or version picker survives rotation. */
    val menu: MenuState = MenuState(this)

    /** Photos staged by `core://MediaPicker` that no create has taken yet (D49). */
    val attachments: Attachments = Attachments({ this.bridge })

    /** The back stack of screens and their form values (D34). */
    val navigation: Navigation = Navigation(this)

    // -----------------------------------------------------------------------------------------------------------------
    // Shell mode (decision `blueprints-and-home`, Shell.kt)
    // -----------------------------------------------------------------------------------------------------------------

    /**
     * Shell mode: the platform opened the pod's [HOME_APP]. The home layout is then drawn from [shell] (kept while the
     * engine shows the app of the selected `core://AppFrame`), and the native menu button and bell are left out.
     */
    val shellMode: Boolean = appId == HOME_APP

    private val _shell = MutableStateFlow<ShellSnapshot?>(null)

    /** The home layout as last seen (shell mode). */
    val shell: StateFlow<ShellSnapshot?> = _shell.asStateFlow()

    /** The shell's own form (the dock's selected tab): kept apart from the framed app's, which [openApp] clears. */
    val shellForm: SnapshotStateMap<String, String> = mutableStateMapOf()

    private var shellFormRestored = false

    private val _dockRequest = MutableStateFlow<String?>(null)

    /** An app `action.device` `open_app` asked the dock to show, until the dock has selected it. */
    val dockRequest: StateFlow<String?> = _dockRequest.asStateFlow()

    internal fun dockRequestHandled() {
        _dockRequest.value = null
    }

    private val _deviceApps = MutableStateFlow<List<AppSummary>>(emptyList())

    /** The pod's apps for the device data (`local:apps`); reloaded by [refreshDeviceApps]. */
    val deviceApps: StateFlow<List<AppSummary>> = _deviceApps.asStateFlow()

    /** The platform name for `local://device` (from [setDeviceInfo]). */
    val platform: String get() = deviceInfo?.first ?: "unknown"

    private val switchLock = Any()

    /** Background work that must not block the caller (marking read, push registration); cancelled by [close]. */
    private val background = CoroutineScope(SupervisorJob() + io)

    /**
     * Platform hook for notifications that arrive LIVE while an engine runs (never catch-up, lists or push): e.g. post
     * an OS notification when the app is in the background. Called on the engine's events thread, not the main thread;
     * exceptions are caught and logged. Installed on the current engine and on every engine [openApp] creates.
     */
    @Volatile
    var onNotification: ((AppNotification) -> Unit)? = null

    private val _unread = MutableStateFlow(UnreadCounts())

    /** Unread notifications (all apps), from the engine's snapshots: drives the bell's badge. */
    val unread: StateFlow<UnreadCounts> = _unread.asStateFlow()

    /** The notification inbox's and the notification settings' state (kept here, like [menu]). */
    val inbox: InboxState = InboxState(this)

    @Volatile
    private var deviceInfo: Pair<String, String>? = null

    /** A notification target waiting for its app's workspace ([openNotification]). */
    private data class PendingTarget(val targetUri: String, val engine: Int)

    private val pendingTarget = MutableStateFlow<PendingTarget?>(null)

    /** Set by [close] (under [switchLock]): [openApp] and [pin] then do nothing, so no engine outlives the controller. */
    @Volatile
    private var closed = false

    /** Whether [close] was called. */
    val isClosed: Boolean get() = closed

    init {
        subscribe(bridge, 0)
        // Shell mode: the pod's app list changes when the agent builds or removes an app, and no event says so to
        // the engine of another app; look again every [APPS_REFRESH_MS] while a home is drawn.
        if (shellMode) {
            background.launch {
                while (!closed) {
                    delay(APPS_REFRESH_MS)
                    if (_shell.value != null) refreshDeviceApps()
                }
            }
        }
    }

    private fun subscribe(b: ConstructEngineBridge, engine: Int) {
        try {
            b.setNotificationListener { json -> onLiveNotification(json, engine) }
            deviceInfo?.let { (platform, name) -> b.setDeviceInfo(platform, name) }
        } catch (t: Throwable) {
            println("[Construct] notification setup failed: $t")
        }
        b.subscribe { onSnapshot(it, engine) }
    }

    private fun onLiveNotification(json: String, engine: Int) {
        if (engine != _state.value.engine) return // a replaced engine
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

    /** The app on screen: the one opened, else what the engine reports. */
    fun currentAppId(): String? = _appId.value ?: _state.value.activeLayout.appId

    private fun onSnapshot(snapshot: EngineSnapshot, engine: Int) {
        val pre = _state.value
        if (pre.engine != engine || snapshot.seq <= pre.seq) return // cheap pre-check; re-checked atomically below
        var decodeError: String? = null
        val decoded: ConstructState? = try {
            val trimmed = snapshot.workspaceJson.trim()
            val workspace = if (trimmed.isEmpty() || trimmed == "null") null else ConstructJson.decodeNode(trimmed)
            val e = snapshot.entitiesJson.trim()
            val entities = if (e.isEmpty() || e == "null") emptyMap() else ConstructJson.decodeEntities(e)
            val queries = decodeQueries(snapshot.queriesJson)
            ConstructState(
                workspace, entities, queries, AuthInfo.parse(snapshot.authJson),
                snapshot.isOnline, snapshot.pendingActions, error = null, seq = snapshot.seq,
                activeLayout = ActiveLayout.parse(snapshot.layoutJson), engine = engine,
                loads = LoadState.parseAll(snapshot.loadsJson),
            )
        } catch (t: Throwable) {
            println("[Construct] snapshot decode failed: $t")
            decodeError = "Failed to decode engine state: ${t.message}"
            null
        }
        // Remembered form values (schema 10) are in the form before the layout first draws.
        if (_pinned.value == null) decoded?.workspace?.let(navigation::restoreRoot)
        _state.update { cur ->
            when {
                cur.engine != engine || snapshot.seq <= cur.seq -> cur
                // Unknown connectivity keeps the last answer (carried over by openApp): no offline flash on a switch.
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
        if (now.engine == engine && now.seq == snapshot.seq) {
            if (shellMode) captureShell(now)
            val unread = NotificationsJson.parseUnread(snapshot.notificationsJson)
            if (shellMode && unread.total != _unread.value.total) reloadShellData()
            _unread.value = unread
            applyPendingTarget()
            if (now.activeLayout.gone) appGone(engine)
        }
    }

    /**
     * The app on screen was deleted (here or on another device): say so and open the default app, off the engine's
     * thread (the old engine is closed by [openApp]). Once per engine.
     */
    private fun appGone(engine: Int) {
        if (closed || goneHandled == engine) return
        goneHandled = engine
        _notice.value = APP_DELETED
        val fallback = defaultAppId?.takeIf { it != currentAppId() } ?: return
        background.launch {
            if (_state.value.engine == engine) openApp(fallback, keepNotice = true)
        }
    }

    @Volatile
    private var goneHandled = -1

    /**
     * Binds the visible form's `$form.<field>` values and a fresh `$nonce` into [intent] (nothing is sent when a
     * referenced field is blank), opens or closes screens for navigation intents that match the layout's screens
     * (DECISIONS D34), and sends everything else to the engine. A create hands its attachments to the engine; to close
     * its screen, a layout follows it with `action.navigation.back` in an `action.sequence`.
     */
    fun dispatch(intent: ActionIntent) {
        val bound = bindForm(intent, navigation.activeForm()) ?: return
        run(bound)
    }

    /** [dispatch] for the shell's own layout: its form is [shellForm]. */
    fun dispatchShell(intent: ActionIntent) = dispatchWithForm(intent, shellForm)

    /** [dispatch] with `$form` values from [form] (e.g. a drawer panel's own form). */
    fun dispatchWithForm(intent: ActionIntent, form: Map<String, String>) {
        val bound = bindForm(intent, form) ?: return
        run(bound)
    }

    /** Carries out a bound intent; the steps of an `action.sequence` one after another, each as if tapped alone. */
    private fun run(bound: ActionIntent) {
        if (bound is ActionIntent.Sequence) {
            bound.steps.forEach(::run)
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
        if (navigation.handle(bound)) return
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
        // Logging is the bridge's job (desktop prints "[desktop] dispatch: <json>").
        try {
            bridge.dispatch(json)
        } catch (t: Throwable) {
            println("[Construct] dispatch failed: $t")
            _state.update { it.copy(error = "Dispatch failed: ${t.message}") }
        }
    }

    /**
     * `action.device` (feature `device_action`): open Settings or the inbox, refresh, leave the pod, or open an app (in
     * shell mode the dock selects it; otherwise the engine switches to it). An unknown command is logged and ignored.
     */
    private fun device(intent: ActionIntent.Device) {
        when (intent.command) {
            "open_settings" -> menu.showSettings()
            "open_inbox" -> inbox.openInbox()
            "refresh" -> {
                refreshDeviceApps()
                if (shellMode && _shell.value == null) refreshFrame(HOME_APP)
            }
            "leave_pod" -> {
                leavePod()
                if (shellMode) {
                    _shell.value = null
                    shellForm.clear()
                    shellFormRestored = false
                    _deviceApps.value = emptyList()
                    openApp(HOME_APP)
                }
            }
            "open_app" -> {
                val app = intent.app?.trim().orEmpty()
                if (app.isEmpty()) return
                if (shellMode) _dockRequest.value = app else openApp(app)
            }
            "open_notification" -> {
                val n = inbox.items.firstOrNull { it.id == intent.id } ?: return
                if (shellMode) _dockRequest.value = n.appId
                inbox.tap(n)
                reloadShellData()
            }
            "mark_all_read" -> {
                inbox.markAllRead()
                reloadShellData()
            }
            "toggle_channel" -> {
                val c = (inbox.channels as? Load.Ready)?.value
                    ?.firstOrNull { it.appId == intent.app && it.id == intent.id } ?: return
                inbox.setChannelEnabled(c, !c.enabled, background)
            }
            "sign_in" -> {
                val provider = menu.providers.firstOrNull { it.id == intent.id } ?: return
                val start = provider.intent ?: return
                // Another server, when the layout passes one (a typed `$form?.server`); else the provider's default.
                val server = intent.value?.trim().orEmpty()
                menu.closeMenu()
                send(if (server.isNotEmpty() && provider.serverPlaceholder != null) start.copy(instanceUrl = server) else start)
            }
            "sign_out" -> {
                logout()
                reloadShellData()
            }
            "show" -> {
                val page = intent.id?.trim().orEmpty()
                if (page.isNotEmpty()) _dockRequest.value = page
            }
            "show_versions" -> {
                val app = intent.app?.trim().orEmpty()
                if (app.isEmpty()) return
                _versionsApp.value = app
                background.launch { menu.loadHistory(app) }
            }
            "pin_version" -> {
                val app = intent.app?.trim()?.takeIf { it.isNotEmpty() } ?: _versionsApp.value ?: return
                val version = intent.id?.trim()?.toLongOrNull() ?: return
                if (shellMode) _dockRequest.value = app
                menu.selectVersion(app, version, background)
            }
            "follow_latest" -> {
                val app = intent.app?.trim()?.takeIf { it.isNotEmpty() } ?: _versionsApp.value ?: return
                _pins.update { it - app }
                if (_pinned.value?.appId == app) followLatest()
            }
            "set_server" -> {
                val choice = serverChoice ?: return
                choice.options.firstOrNull { it.key == intent.id }?.let { choice.onSelect(it.url) }
            }
            else -> println("[Construct] action.device: unknown command '${intent.command}'; ignored")
        }
    }

    /** The platform's servers, for `local:servers` and `set_server` (set by [ConstructApp]; null: none to pick). */
    @Volatile
    internal var serverChoice: ServerChoice? = null

    private val _shellDataToken = MutableStateFlow(0)

    /**
     * Bumped when the shell's pod data (notifications, channels, sign-ins) should be loaded again: on a tab change,
     * when the unread count changes, and after a device action that changes it. [ShellRoot] loads on each value.
     */
    val shellDataToken: StateFlow<Int> = _shellDataToken.asStateFlow()

    internal fun reloadShellData() {
        _shellDataToken.update { it + 1 }
    }

    /**
     * Shell mode: keeps the home layout from a snapshot of the engine on [HOME_APP] (the bundled layouts stand in until
     * then), restores the shell's remembered form values once, and loads the device's app list.
     */
    private fun captureShell(state: ConstructState) {
        if (currentAppId() != HOME_APP || !state.auth.session) return
        val workspace = displayedWorkspace(state, _pinned.value) ?: return
        val first = _shell.value == null
        _shell.value = ShellSnapshot(workspace, state.entities, state.queries, state.activeLayout.version)
        if (!shellFormRestored) {
            shellFormRestored = true
            val remembered = try {
                rememberedFormValues(SHELL_FORM_SCOPE)
            } catch (t: Throwable) {
                emptyMap()
            }
            remembered.forEach { (k, v) -> if (k !in shellForm) shellForm[k] = v }
        }
        if (first) refreshDeviceApps()
    }

    /** Reloads the pod's apps for the device data, and the home layout when a newer drawable version exists. */
    fun refreshDeviceApps() {
        if (closed) return
        background.launch {
            val list = try {
                apps.apps()
            } catch (e: Throwable) {
                println("[Construct] loading the apps for the device data failed: $e")
                return@launch
            }
            _deviceApps.value = list
            val home = list.firstOrNull { it.id == HOME_APP } ?: return@launch
            val have = _shell.value ?: return@launch
            val newest = home.latestCompatibleVersion ?: return@launch
            if (have.version != null && newest > have.version && currentAppId() != HOME_APP) {
                try {
                    val layout = apps.spec(HOME_APP, newest)
                    _shell.update { cur -> cur?.copy(workspace = layout, version = newest) }
                } catch (e: Throwable) {
                    println("[Construct] loading home v$newest failed: $e")
                }
            }
        }
    }

    /** The dock selected a tab: the app list (and the inbox, channels and sign-ins) may have changed meanwhile. */
    internal fun onDockSelected() {
        refreshDeviceApps()
        reloadShellData()
    }

    /** Starts the frame's app again on a fresh engine (Try again on a frame that failed, or on the rescue screen). */
    fun refreshFrame(id: String) {
        if (closed || openBridge == null) return
        if (currentAppId() == id) {
            // openApp is a no-op for the app on screen: forget it first.
            _appId.value = null
            _state.update { it.copy(activeLayout = ActiveLayout()) }
        }
        if (!openApp(id)) _appId.value = id
    }

    /** Signs out (the menu's "Sign out"); the engine notifies `signed_out` and refetches. */
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

    /** Forwards an OAuth redirect to the current engine ([ConstructEngineBridge.handleAuthRedirect]). */
    fun handleAuthRedirect(url: String): Boolean = bridge.handleAuthRedirect(url)

    /**
     * Closes the current engine (the platform calls this when the UI goes away for good). Idempotent; afterwards
     * [openApp] and [pin] are no-ops, so a version selection finishing late can't create an engine nobody closes.
     */
    fun close() {
        synchronized(switchLock) {
            if (closed) return
            closed = true
            pendingTarget.value = null
            background.cancel()
            bridge.close()
        }
    }

    // -----------------------------------------------------------------------------------------------------------------
    // Notifications (D52)
    // -----------------------------------------------------------------------------------------------------------------

    /** What the pod shows for this device (e.g. "android", the model): applied now and to every engine created later. */
    fun setDeviceInfo(platform: String, name: String) {
        deviceInfo = platform to name
        try {
            bridge.setDeviceInfo(platform, name)
        } catch (t: Throwable) {
            println("[Construct] setDeviceInfo failed: $t")
        }
    }

    /**
     * Sends this device's Web Push subscription to the pod, in the background (errors are logged). The engine keeps it
     * in the shared cache, so engines created later keep it too.
     */
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

    /** A decrypted Web Push body, handed to the current engine (cached, counted); the notification, or null. */
    fun handlePushMessage(payloadJson: String): AppNotification? = try {
        bridge.handlePushMessage(payloadJson)?.let(NotificationsJson::parseNotification)
    } catch (t: Throwable) {
        println("[Construct] push message: $t")
        null
    }

    /** Every app's notification channels with this device's choices; empty on any error. BLOCKING: off the main thread. */
    fun notificationChannels(): List<NotificationChannelInfo> = try {
        NotificationsJson.parseChannels(bridge.notificationChannels())
    } catch (t: Throwable) {
        println("[Construct] notification channels: $t")
        emptyList()
    }

    /** [appId]'s title from the apps catalog, when it has been loaded (the menu or the inbox load it); else null. */
    fun appTitle(appId: String): String? =
        ((menu.apps as? Load.Ready)?.value ?: apps.lastApps)?.firstOrNull { it.id == appId }?.title

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
     * Opens a notification (a tapped inbox row or OS notification): opens [appId] unless it is on screen ([openApp]),
     * then, once that app's workspace has loaded, opens [targetUri] (`action.navigation.open`, Push) through the normal
     * routing (D35; a target no screen matches, or none, just leaves the app open), and marks [notificationId] read in
     * the background. Call it on the main thread; it works right after the controller is created (cold start).
     */
    fun openNotification(appId: String, targetUri: String?, notificationId: String?) {
        if (closed) return
        notificationId?.let { markNotificationsRead(listOf(it)) }
        menu.closeMenu()
        inbox.close()
        if (appId != currentAppId() && !openApp(appId)) {
            println("[Construct] cannot open app $appId for a notification")
            return
        }
        if (targetUri.isNullOrBlank()) return
        // An earlier screen stack belongs to what the user was doing; the notification's screen goes on top of it.
        pendingTarget.value = PendingTarget(targetUri.trim(), _state.value.engine)
        applyPendingTarget()
    }

    /**
     * Opens the pending notification target once its engine shows a workspace. Dropped when another engine took over,
     * or once a versioned layout (the app itself, not its signed-out screen) has no screen for it.
     */
    private fun applyPendingTarget() {
        val p = pendingTarget.value ?: return
        val s = _state.value
        if (s.engine != p.engine) {
            pendingTarget.compareAndSet(p, null)
            return
        }
        if (s.workspace == null) return
        val matched = navigation.canOpen(p.targetUri)
        if (!matched && s.activeLayout.version == null) return // the signed-out screen: wait for the app's layout
        if (!pendingTarget.compareAndSet(p, null)) return
        if (matched) navigation.open(ActionIntent.Navigate(p.targetUri, PresentationMode.Push))
    }

    /** Runs [call] on the current bridge off the caller's thread; errors are logged. Counted in [LayoutCalls]. */
    private fun inBackground(what: String, call: (ConstructEngineBridge) -> Unit) {
        if (closed) return
        val b = bridge
        LayoutCalls.add(1)
        background.launch {
            try {
                call(b)
            } catch (t: Throwable) {
                println("[Construct] $what failed: $t")
            }
        }.invokeOnCompletion { LayoutCalls.add(-1) } // also when cancelled before it started ([close])
    }

    /**
     * Shows [version] of the current app ([layout] = its spec) instead of the latest, until [followLatest]. False (and
     * nothing changes) once [close]d.
     */
    fun pin(appId: String, version: Long, layout: UiNode): Boolean {
        if (closed) return false
        navigation.restoreRoot(layout)
        val p = PinnedVersion(appId, version, layout)
        _pinned.value = p
        if (shellMode) _pins.update { it + (appId to p) }
        return true
    }

    /** Back to the latest version (and following new ones as they are published). */
    fun followLatest() {
        _pinned.value?.appId?.let { app -> _pins.update { it - app } }
        _pinned.value = null
        navigation.restoreRoot(_state.value.workspace)
    }

    private val _pins = MutableStateFlow<Map<String, PinnedVersion>>(emptyMap())

    /** Shell mode: the version pinned per app on this device (kept while the frame shows other apps). */
    val pins: StateFlow<Map<String, PinnedVersion>> = _pins.asStateFlow()

    private val _versionsApp = MutableStateFlow<String?>(null)

    /** The app whose versions `local:versions` lists (`action.device` `show_versions`). */
    val versionsApp: StateFlow<String?> = _versionsApp.asStateFlow()

    /** The remembered values of form [scope] for the app on screen (schema 10); empty on any error. */
    internal fun rememberedFormValues(scope: String): Map<String, String> = try {
        bridge.rememberedFormValues(scope)
    } catch (t: Throwable) {
        println("[Construct] remembered form values: $t")
        emptyMap()
    }

    /** Saves one remembered form value of the app on screen (null forgets it); errors are logged. */
    internal fun rememberFormValue(scope: String, field: String, value: String?) {
        try {
            bridge.rememberFormValue(scope, field, value)
        } catch (t: Throwable) {
            println("[Construct] remembering $field: $t")
        }
    }

    /**
     * Opens app [id]: a no-op (true) when it is already on screen; otherwise creates its engine with the platform's
     * factory, shows it (starting from "Loading...", following its latest version; connectivity carries over, it is
     * the same server) and closes the old engine. False, with nothing changed, when this platform can't open other apps,
     * the engine could not be created, or the controller is [close]d.
     */
    fun openApp(id: String): Boolean = openApp(id, keepNotice = false)

    private fun openApp(id: String, keepNotice: Boolean): Boolean {
        if (closed) return false
        if (id == currentAppId()) return true
        val factory = openBridge ?: return false
        synchronized(switchLock) {
            if (closed) return false
            val next = try {
                factory(id)
            } catch (t: Throwable) {
                println("[Construct] cannot open app $id: $t")
                return false
            }
            if (closed) {
                // Closed meanwhile (e.g. by the factory's thread re-entering): this engine would never be closed.
                try {
                    next.close()
                } catch (t: Throwable) {
                    println("[Construct] closing the unused engine failed: $t")
                }
                return false
            }
            val old = bridge
            val engine = _state.value.engine + 1
            bridge = next
            _appId.value = id
            if (!keepNotice) _notice.value = null
            // Shell mode keeps a pin per app: back on an app, its pinned version shows again.
            _pinned.value = if (shellMode) _pins.value[id] else null
            navigation.clear()
            _state.value = ConstructState(isOnline = _state.value.isOnline, engine = engine)
            // A target for the app being opened is re-keyed to its new engine ([openNotification] sets it after this).
            pendingTarget.value = null
            subscribe(next, engine)
            try {
                old.close()
            } catch (t: Throwable) {
                println("[Construct] closing the previous engine failed: $t")
            }
        }
        return true
    }
}

/**
 * The shared root UI: the rendered layout and, over it, the hamburger button that opens the menu (apps and their
 * versions, notifications, connection, account, server) and the bell that opens the notification inbox. [server] is
 * the platform's server picker (null = no Server section); [mediaPicker] the platform's photo picker for
 * `core://MediaPicker` (null = picking photos is not available). [notificationSettingsExtra] is shown at the top
 * of the menu's notification settings (e.g. the platform's OS permission row).
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
    val pin by controller.pinned.collectAsState()
    val unread by controller.unread.collectAsState()
    val notice by controller.notice.collectAsState()
    // Material3 light scheme on a white background and surface (feed photos sit on white, as on Pixelfed/Instagram).
    MaterialTheme(colorScheme = lightColorScheme(background = Color.White, surface = Color.White)) {
        Surface(modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
            CompositionLocalProvider(
                LocalMediaPickerPlatform provides mediaPicker,
                LocalAttachments provides controller.attachments,
            ) {
                val deviceApps by controller.deviceApps.collectAsState()
                val pins by controller.pins.collectAsState()
                val versionsApp by controller.versionsApp.collectAsState()
                controller.serverChoice = server
                val inbox = controller.inbox
                val sources = DeviceSources(
                    notifications = inbox.items,
                    channels = (inbox.channels as? Load.Ready)?.value.orEmpty(),
                    signIns = (controller.menu.signIns as? Load.Ready)?.value.orEmpty(),
                    providers = controller.menu.providers,
                    server = server,
                    nowSeconds = remember(inbox.items, versionsApp) { nowEpochSeconds() },
                    pins = pins.mapValues { it.value.version },
                    versionsApp = versionsApp,
                    versions = versionsApp?.let { controller.menu.history[it] },
                )
                val device = remember(state.isOnline, state.pendingActions, state.auth, unread, deviceApps, sources) {
                    deviceData(state, unread, deviceApps, controller.platform, sources)
                }
                MenuDrawer(controller, state, pin, server, notificationSettingsExtra) {
                  CompositionLocalProvider(LocalDeviceData provides device) {
                    Box(Modifier.fillMaxSize()) {
                        val workspace = displayedWorkspace(state, pin).takeUnless { state.activeLayout.gone }
                        if (controller.shellMode) {
                            // The layout-first shell (decision `blueprints-and-home`): the home layout draws the
                            // navigation and frames the apps; screens open inside the frame.
                            ShellRoot(controller, state, Modifier.fillMaxSize())
                        } else if (workspace == null && state.activeLayout.gone) {
                            Column(Modifier.fillMaxSize().padding(24.dp), verticalArrangement = Arrangement.Center, horizontalAlignment = Alignment.CenterHorizontally) {
                                Text(APP_DELETED, style = MaterialTheme.typography.bodyLarge, modifier = Modifier.testTag("app-gone"))
                                Text(
                                    "Open another app from the menu.",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    modifier = Modifier.padding(top = 8.dp),
                                )
                            }
                        } else if (workspace == null) {
                            Column(Modifier.fillMaxSize().padding(24.dp), verticalArrangement = Arrangement.Center, horizontalAlignment = Alignment.CenterHorizontally) {
                                Text("Loading…", style = MaterialTheme.typography.bodyLarge)
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
                        } else {
                            CompositionLocalProvider(
                                LocalConstructQueries provides state.queries + device.queries,
                                LocalWorkspaceTitleInset provides MENU_BUTTON_INSET,
                                LocalWorkspaceTitleEndInset provides BELL_BUTTON_INSET,
                                LocalForm provides controller.navigation.rootForm,
                                LocalFormMemory provides remember(controller) {
                                    FormMemory { f, v -> controller.navigation.remember(ROOT_FORM_SCOPE, f, v) }
                                },
                            ) {
                                RenderUiNode(workspace, state.entities + device.entities, controller::dispatch, Modifier.fillMaxSize())
                            }
                        }
                        if (!controller.shellMode) {
                            MenuButton(
                                badge = showMenuBadge(state, pin),
                                onClick = controller.menu::openMenu,
                                modifier = Modifier.align(Alignment.TopStart).padding(start = 8.dp, top = 8.dp),
                            )
                            BellButton(
                                unread = unread.total,
                                onClick = controller.inbox::openInbox,
                                modifier = Modifier.align(Alignment.TopEnd).padding(end = 8.dp, top = 8.dp),
                            )
                            // An open screen covers the layout and the buttons; the drawer still opens over it.
                            ScreenHost(controller, state)
                        }
                        notice?.takeUnless { state.activeLayout.gone }?.let {
                            NoticeBar(it, onDismiss = controller::dismissNotice, modifier = Modifier.align(Alignment.BottomCenter))
                        }
                        // The inbox covers everything but the drawer.
                        InboxPanel(controller)
                    }
                  }
                }
            }
        }
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
