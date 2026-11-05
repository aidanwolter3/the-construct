package org.theconstruct

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.NavigationRail
import androidx.compose.material3.NavigationRailItem
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.ProvidableCompositionLocal
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

// The layout-first shell (decision `blueprints-and-home`): every pod has a `home` app, which starts as a reference
// to the home blueprint (a `core://Dock` of the pod's apps). When a platform opens [HOME_APP], [ConstructController]
// runs in shell mode: it keeps the last home layout it saw ([ShellSnapshot]) and draws it with [ShellRoot], while its
// one engine follows the app shown in the selected `core://AppFrame`. Before a pod is open it draws the bundled
// sign-in layout ([BundledLayouts.bootstrap]); when the pod has no home it can draw, the bundled rescue layout.

/** The app id of every pod's home app (`pod_core::HOME_APP`). */
const val HOME_APP = "home"

/** The form scope of the shell's own form values (the dock's selected tab). */
const val SHELL_FORM_SCOPE = "shell"

/** The home layout as last drawn: its workspace and the data its engine loaded with it. */
data class ShellSnapshot(
    val workspace: UiNode,
    val entities: Map<String, JsonObject> = emptyMap(),
    val queries: Map<String, List<String>> = emptyMap(),
    val version: Long? = null,
)

/** Data the device provides to layouts (feature `local_data`): `local://` entities and `local:` queries. */
data class DeviceData(
    val entities: Map<String, JsonObject> = emptyMap(),
    val queries: Map<String, List<String>> = emptyMap(),
)

/**
 * The device data for the current state: `local://connection` (online, pending, label, attention), `local://inbox`
 * (unread, label), `local://apps` (count) and the `local:apps` query of the pod's apps but home (`local://apps/<id>`:
 * id, title, icon, kind, detail, unread), `local://device` (platform) and `local://session` (open, error, message:
 * the pod sign-in's state).
 */
fun deviceData(state: ConstructState, unread: UnreadCounts, apps: List<AppSummary>, platform: String): DeviceData {
    val entities = LinkedHashMap<String, JsonObject>()
    val pending = state.pendingActions
    val connection = when (state.isOnline) {
        true -> "Online"
        false -> "Offline"
        null -> "Connecting…"
    } + if (pending > 0) " · $pending pending" else ""
    entities["local://connection"] = buildJsonObject {
        put("online", state.isOnline == true)
        put("pending", pending)
        put("label", connection)
        put("attention", state.isOnline == false || pending > 0)
    }
    entities["local://inbox"] = buildJsonObject {
        put("unread", unread.total)
        put("label", if (unread.total == 0) "All read" else "${unread.total} unread")
    }
    val listed = apps.filter { it.id != HOME_APP && !it.hidden }
    val uris = listed.map { app ->
        val uri = "local://apps/${app.id}"
        entities[uri] = buildJsonObject {
            put("id", app.id)
            put("title", app.title)
            put("icon", app.icon)
            put("kind", if (app.custom) "custom" else "blueprint")
            put("detail", app.updatesFrom?.let { "${it.title} has updates" } ?: app.latestVersion?.let { "v$it" } ?: "")
            put("unread", unread.byApp[app.id] ?: 0)
        }
        uri
    }
    entities["local://apps"] = buildJsonObject { put("count", listed.size) }
    entities["local://device"] = buildJsonObject { put("platform", platform) }
    entities["local://session"] = buildJsonObject {
        put("open", state.auth.session)
        put("error", state.auth.error.orEmpty())
        put("message", state.auth.message.orEmpty())
    }
    return DeviceData(entities, mapOf("local:apps" to uris))
}

/** The device data, for every layout on screen (provided by [ConstructApp]). */
val LocalDeviceData: ProvidableCompositionLocal<DeviceData> = staticCompositionLocalOf { DeviceData() }

/** The shell's controller, for `core://AppFrame` (null outside shell mode: frames then say they can't open apps). */
val LocalShellController: ProvidableCompositionLocal<ConstructController?> = staticCompositionLocalOf { null }

/** Layouts bundled with the app: they draw with no pod at all. */
object BundledLayouts {
    /** The first screen: a pod code, or a temporary pod (the same intents as the pod host's "Open your pod"). */
    val bootstrap: UiNode by lazy { ConstructJson.decodeNode(BOOTSTRAP_JSON) }

    /** When the pod has no home this app can draw: try again, or leave the pod. */
    val rescue: UiNode by lazy { ConstructJson.decodeNode(RESCUE_JSON) }

    private const val BOOTSTRAP_JSON = """
{"type": "core://Workspace", "id": "bootstrap", "title": "The Construct", "slots": [
  {"slot_id": "pod", "provider_uri": "local://bootstrap", "content": {"type": "core://Column", "spacing_dp": 12,
    "padding_h_dp": 16, "padding_v_dp": 8, "children": [
    {"type": "core://Text", "style": "Body",
     "value_expr": "Enter your pod code to open your apps. Your data lives in your own private pod."},
    {"type": "core://TextField", "field": "pod_token", "placeholder": "Pod code", "max_length": 128},
    {"type": "core://Button", "label": "Open pod",
     "on_click": {"intent": "action.auth.session", "path": "/v1/pods/open", "payload": {"token": "${'$'}form.pod_token"}}},
    {"type": "core://Conditional", "value": "${'$'}{local://session#error}",
     "content": {"type": "core://Text", "style": "Secondary", "value_expr": "${'$'}{local://session#error}"}},
    {"type": "core://Text", "style": "Secondary",
     "value_expr": "No code? Try a temporary pod. It and everything in it is deleted after a day."},
    {"type": "core://Button", "label": "Try a temporary pod",
     "on_click": {"intent": "action.auth.session", "path": "/v1/pods/temporary", "payload": {}}}
  ]}}
]}"""

    private const val RESCUE_JSON = """
{"type": "core://Workspace", "id": "rescue", "title": "The Construct", "slots": [
  {"slot_id": "rescue", "provider_uri": "local://rescue", "content": {"type": "core://Column", "spacing_dp": 12,
    "padding_h_dp": 16, "padding_v_dp": 8, "children": [
    {"type": "core://Text", "style": "Title", "value_expr": "Your pod has no screen this app can show"},
    {"type": "core://Text", "style": "Body",
     "value_expr": "Nothing on this device is lost. Ask your agent to repair the home layout, then try again."},
    {"type": "core://Button", "label": "Try again", "on_click": {"intent": "action.device", "command": "refresh"}},
    {"type": "core://Button", "label": "Leave pod on this device",
     "on_click": {"intent": "action.device", "command": "leave_pod"}}
  ]}}
]}"""
}

/** What the shell draws now. */
internal sealed class ShellView {
    data object Loading : ShellView()
    data object Bootstrap : ShellView()
    data object Rescue : ShellView()
    data class Home(val snapshot: ShellSnapshot) : ShellView()
}

/**
 * Bootstrap until the engine holds a pod session; then the home layout once seen (it is kept while the engine shows
 * other apps); the rescue layout when the engine, on home, has no layout and an error; loading meanwhile.
 */
internal fun shellView(state: ConstructState, shell: ShellSnapshot?, onHome: Boolean): ShellView = when {
    state.seq >= 0 && !state.auth.session -> ShellView.Bootstrap
    shell != null -> ShellView.Home(shell)
    onHome && state.workspace == null && state.error != null -> ShellView.Rescue
    onHome && state.seq >= 0 && state.workspace != null -> ShellView.Rescue // home is there but isn't drawable
    else -> ShellView.Loading
}

/** The shell (shell mode's root): the bundled layouts, or the home layout over the device data. */
@Composable
internal fun ShellRoot(controller: ConstructController, state: ConstructState, modifier: Modifier = Modifier) {
    val shell by controller.shell.collectAsState()
    val appId by controller.appId.collectAsState()
    val device = LocalDeviceData.current
    val view = shellView(state, shell, onHome = (appId ?: state.activeLayout.appId) == HOME_APP)
    val content: Pair<UiNode, ShellSnapshot?>? = when (view) {
        ShellView.Loading -> null
        ShellView.Bootstrap -> BundledLayouts.bootstrap to null
        ShellView.Rescue -> BundledLayouts.rescue to null
        is ShellView.Home -> view.snapshot.workspace to view.snapshot
    }
    if (content == null) {
        Box(modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        return
    }
    val (node, snapshot) = content
    val entities = remember(snapshot?.entities, device) { (snapshot?.entities ?: emptyMap()) + device.entities }
    val queries = remember(snapshot?.queries, device) { (snapshot?.queries ?: emptyMap()) + device.queries }
    CompositionLocalProvider(
        LocalConstructQueries provides queries,
        LocalForm provides controller.shellForm,
        LocalFormMemory provides remember(controller) {
            FormMemory { f, v -> controller.navigation.remember(SHELL_FORM_SCOPE, f, v) }
        },
        LocalShellController provides controller,
    ) {
        RenderUiNode(node, entities, controller::dispatchShell, modifier.fillMaxSize().testTag("shell"))
    }
}

/** Whether a workspace is drawn whole by its one slot (a `core://Dock`), with no title bar or page scroll. */
internal fun fillsScreen(workspace: UiNode.Workspace): Boolean =
    workspace.slots.size == 1 && workspace.slots[0].content is UiNode.Dock

/** One tab of a dock as drawn: [item] is the entity it was drawn from (query tabs). */
private data class DrawnTab(
    val value: String,
    val label: String,
    val icon: String,
    val badge: String,
    val content: UiNode,
    val item: JsonObject?,
)

private fun DockTab.drawn(entities: Map<String, JsonObject>, item: JsonObject?): DrawnTab = DrawnTab(
    value = (resolveRef(value, item) ?: value).trim(),
    label = interpolate(label, entities, item),
    icon = icon?.let { resolveRef(it, item) ?: it }.orEmpty(),
    badge = badge?.let { interpolate(it, entities, item) }.orEmpty(),
    content = content,
    item = item,
)

/** A tab's icon: an app icon name, or one of the shell's own (`more`, `home`, `settings`, `inbox`). */
internal fun dockIcon(name: String): ImageVector = when (name) {
    "more", "menu" -> ConstructIcons.Menu
    "settings" -> ConstructIcons.Settings
    "inbox", "notifications" -> ConstructIcons.Notifications
    else -> appIcon(name)
}

/**
 * `core://Dock`: a bar of tabs along the bottom (a rail down the side when the window is wide) and the selected tab's
 * content filling the rest. The selection is the form value of [UiNode.Dock.field], remembered on the device; a tab
 * that is no longer there falls back to the first. `action.device` `open_app` selects the app's tab (or shows the
 * app when it is past `max_items`).
 */
@Composable
internal fun DockView(
    node: UiNode.Dock,
    entities: Map<String, JsonObject>,
    onDispatch: (ActionIntent) -> Unit,
    modifier: Modifier = Modifier,
) {
    val form = LocalForm.current
    val memory = LocalFormMemory.current
    val queries = LocalConstructQueries.current
    val controller = LocalShellController.current
    val itemTabs = remember(node, queries, entities) {
        val template = node.item ?: return@remember emptyList()
        listItems(node.queryKey ?: "", queries, entities).orEmpty().map { template.drawn(entities, it) }
    }
    val before = node.before.map { it.drawn(entities, null) }
    val after = node.after.map { it.drawn(entities, null) }
    val shown = before + itemTabs.take(node.maxItems ?: itemTabs.size) + after
    val request = controller?.dockRequest?.collectAsState()?.value
    LaunchedEffect(request) {
        if (request != null && form != null) {
            form[node.field] = request
            memory?.save(node.field, request)
            controller.dockRequestHandled()
        }
    }
    val wanted = form?.get(node.field).orEmpty()
    val selected = (before + itemTabs + after).firstOrNull { it.value == wanted } ?: shown.firstOrNull()
    val select: (DrawnTab) -> Unit = { tab ->
        form?.set(node.field, tab.value)
        memory?.save(node.field, tab.value)
        controller?.onDockSelected()
    }
    BoxWithConstraints(modifier.fillMaxSize().testTag("dock")) {
        val wide = maxWidth >= 720.dp
        val content: @Composable (Modifier) -> Unit = { m ->
            Box(m) {
                if (selected != null) {
                    key(selected.value) {
                        val c = selected.content
                        if (c is UiNode.AppFrame) {
                            RenderUiNode(c, entities, onDispatch, Modifier.fillMaxSize(), selected.item)
                        } else {
                            Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                                RenderUiNode(c, entities, onDispatch, Modifier.fillMaxWidth(), selected.item)
                            }
                        }
                    }
                }
            }
        }
        if (wide) {
            Row(Modifier.fillMaxSize()) {
                NavigationRail(Modifier.fillMaxHeight()) {
                    shown.forEach { tab ->
                        NavigationRailItem(
                            selected = tab.value == selected?.value,
                            onClick = { select(tab) },
                            icon = { TabIcon(tab) },
                            label = { Text(tab.label, maxLines = 1) },
                            modifier = Modifier.testTag("dock-tab-${tab.value}"),
                        )
                    }
                }
                content(Modifier.weight(1f).fillMaxHeight())
            }
        } else {
            Column(Modifier.fillMaxSize()) {
                content(Modifier.weight(1f).fillMaxWidth())
                NavigationBar {
                    shown.forEach { tab ->
                        NavigationBarItem(
                            selected = tab.value == selected?.value,
                            onClick = { select(tab) },
                            icon = { TabIcon(tab) },
                            label = { Text(tab.label, maxLines = 1) },
                            modifier = Modifier.testTag("dock-tab-${tab.value}"),
                        )
                    }
                }
            }
        }
    }
}

/** A tab's icon with its badge: a count when the badge is a positive number, a dot when it is otherwise truthy. */
@Composable
private fun TabIcon(tab: DrawnTab) {
    val count = tab.badge.toIntOrNull()
    val dot = count == null && isTruthy(tab.badge, null)
    BadgedBox(badge = {
        when {
            count != null && count > 0 -> Badge { Text(if (count > 99) "99+" else "$count") }
            dot -> Badge()
        }
    }) {
        Icon(dockIcon(tab.icon), contentDescription = null, modifier = Modifier.size(22.dp))
    }
}

/**
 * `core://AppFrame`: the app [UiNode.AppFrame.app] drawn whole by the shell's engine. The engine follows the frame on
 * screen ([ConstructController.openApp]); until it has the app's layout the frame shows a spinner, and an app that
 * can't be drawn says so here while the rest of the shell keeps working.
 */
@Composable
internal fun AppFrameView(node: UiNode.AppFrame, item: JsonObject?, modifier: Modifier = Modifier) {
    val controller = LocalShellController.current
    val appId = (resolveRef(node.app, item) ?: node.app).trim()
    if (controller == null || !controller.canOpenApps) {
        FrameMessage("Apps can't be shown inside other apps here.", modifier)
        return
    }
    val state by controller.state.collectAsState()
    val pin by controller.pinned.collectAsState()
    val current by controller.appId.collectAsState()
    val device = LocalDeviceData.current
    LaunchedEffect(appId) {
        if (appId.isNotEmpty() && controller.currentAppId() != appId) controller.openApp(appId)
    }
    val onThis = (current ?: state.activeLayout.appId) == appId
    val workspace = if (onThis) displayedWorkspace(state, pin) else null
    when {
        onThis && state.activeLayout.gone -> FrameMessage(APP_DELETED, modifier)
        workspace == null && onThis && state.error != null -> FrameMessage(
            "This app can't be drawn: ${state.error}",
            modifier,
            onRetry = { controller.refreshFrame(appId) },
        )
        workspace == null -> Box(modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            CircularProgressIndicator(Modifier.testTag("frame-loading"))
        }
        else -> Box(modifier.fillMaxSize().testTag("frame-$appId")) {
            val entities = remember(state.entities, device) { state.entities + device.entities }
            val queries = remember(state.queries, device) { state.queries + device.queries }
            CompositionLocalProvider(
                LocalConstructQueries provides queries,
                LocalForm provides controller.navigation.rootForm,
                LocalFormMemory provides remember(controller) {
                    FormMemory { f, v -> controller.navigation.remember(ROOT_FORM_SCOPE, f, v) }
                },
                // A frame's app draws its own frames as messages: one engine shows one app.
                LocalShellController provides null,
                LocalWorkspaceTitleInset provides 0.dp,
                LocalWorkspaceTitleEndInset provides 0.dp,
            ) {
                RenderUiNode(workspace, entities, controller::dispatch, Modifier.fillMaxSize())
            }
            ScreenHost(controller, state)
        }
    }
}

@Composable
private fun FrameMessage(text: String, modifier: Modifier, onRetry: (() -> Unit)? = null) {
    Column(
        modifier.fillMaxSize().padding(24.dp).testTag("frame-message"),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(text, style = MaterialTheme.typography.bodyMedium, textAlign = TextAlign.Center)
        if (onRetry != null) TextButton(onClick = onRetry) { Text("Try again") }
    }
}
