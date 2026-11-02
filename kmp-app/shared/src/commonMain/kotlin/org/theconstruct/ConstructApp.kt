package org.theconstruct

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.serialization.json.JsonObject

/**
 * UI state derived from engine snapshots. [workspace] is null until the engine has a layout.
 * [pendingActions] counts queued (unsent) outbox mutations. [seq] is the seq of the last applied
 * snapshot (-1 before the first). [queries] maps a FeedList `query_key` to ordered entity URIs (M6) and
 * [auth] is the sign-in status. [activeLayout] names the app on display and its latest version. [isOnline] is null
 * while connectivity is unknown (before the first snapshot; no offline badge then).
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
 * ([pinned]) and the menu's state ([menu]).
 */
class ConstructController(
    bridge: ConstructEngineBridge,
    appId: String? = null,
    private val openBridge: ((appId: String) -> ConstructEngineBridge)? = null,
) {
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

    private val switchLock = Any()

    /** Set by [close] (under [switchLock]): [openApp] and [pin] then do nothing, so no engine outlives the controller. */
    @Volatile
    private var closed = false

    /** Whether [close] was called. */
    val isClosed: Boolean get() = closed

    init {
        subscribe(bridge, 0)
    }

    private fun subscribe(b: ConstructEngineBridge, engine: Int) {
        b.subscribe { onSnapshot(it, engine) }
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
            )
        } catch (t: Throwable) {
            println("[Construct] snapshot decode failed: $t")
            decodeError = "Failed to decode engine state: ${t.message}"
            null
        }
        _state.update { cur ->
            when {
                cur.engine != engine || snapshot.seq <= cur.seq -> cur
                decoded != null -> decoded
                else -> cur.copy(
                    auth = AuthInfo.parse(snapshot.authJson),
                    isOnline = snapshot.isOnline,
                    pendingActions = snapshot.pendingActions,
                    error = decodeError,
                    seq = snapshot.seq,
                    activeLayout = ActiveLayout.parse(snapshot.layoutJson),
                )
            }
        }
    }

    /** Encodes [intent] and forwards it to the engine. */
    fun dispatch(intent: ActionIntent) {
        val json = ConstructJson.encodeIntent(intent)
        // Logging is the bridge's job (desktop prints "[desktop] dispatch: <json>").
        try {
            bridge.dispatch(json)
        } catch (t: Throwable) {
            println("[Construct] dispatch failed: $t")
            _state.update { it.copy(error = "Dispatch failed: ${t.message}") }
        }
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
            bridge.close()
        }
    }

    /**
     * Shows [version] of the current app ([layout] = its spec) instead of the latest, until [followLatest]. False (and
     * nothing changes) once [close]d.
     */
    fun pin(appId: String, version: Long, layout: UiNode): Boolean {
        if (closed) return false
        _pinned.value = PinnedVersion(appId, version, layout)
        return true
    }

    /** Back to the latest version (and following new ones as they are published). */
    fun followLatest() {
        _pinned.value = null
    }

    /**
     * Opens app [id]: a no-op (true) when it is already on screen; otherwise creates its engine with the platform's
     * factory, shows it (starting from "Loading...", following its latest version; connectivity carries over, it is
     * the same server) and closes the old engine. False, with nothing changed, when this platform can't open other apps,
     * the engine could not be created, or the controller is [close]d.
     */
    fun openApp(id: String): Boolean {
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
            _pinned.value = null
            _state.value = ConstructState(isOnline = _state.value.isOnline, engine = engine)
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
 * versions, connection, account, server). [server] is the platform's server picker (null = no Server section).
 */
@Composable
fun ConstructApp(controller: ConstructController, modifier: Modifier = Modifier, server: ServerChoice? = null) {
    val state by controller.state.collectAsState()
    val pin by controller.pinned.collectAsState()
    // Material3 light scheme on a white background and surface (feed photos sit on white, as on Pixelfed/Instagram).
    MaterialTheme(colorScheme = lightColorScheme(background = Color.White, surface = Color.White)) {
        Surface(modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
            MenuDrawer(controller, state, pin, server) {
                Box(Modifier.fillMaxSize()) {
                    val workspace = displayedWorkspace(state, pin)
                    if (workspace == null) {
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
                            LocalConstructQueries provides state.queries,
                            LocalWorkspaceTitleInset provides MENU_BUTTON_INSET,
                        ) {
                            RenderUiNode(workspace, state.entities, controller::dispatch, Modifier.fillMaxSize())
                        }
                    }
                    MenuButton(
                        badge = showMenuBadge(state, pin),
                        onClick = controller.menu::openMenu,
                        modifier = Modifier.align(Alignment.TopStart).padding(start = 8.dp, top = 8.dp),
                    )
                }
            }
        }
    }
}
