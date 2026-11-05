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
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DrawerValue
import androidx.compose.material3.Icon
import androidx.compose.material3.ModalDrawerSheet
import androidx.compose.material3.ModalNavigationDrawer
import androidx.compose.material3.rememberDrawerState
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
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.key
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

// The pod's home (decisions `blueprints-and-home`, `layouts-not-apps`): every pod has a `home` layout, the root clients
// open first; it embeds and presents the pod's other layouts. Before a pod is open the client draws the bundled sign-in
// layout ([BundledLayouts.bootstrap]); when the pod has no home it can draw, the bundled rescue layout. The device data
// ([deviceData]) is what layouts bind with no feed.

/** The app id of every pod's home app (`pod_core::HOME_APP`). */
const val HOME_APP = "home"

/** Data the device provides to layouts (feature `local_data`): `local://` entities and `local:` queries. */
data class DeviceData(
    val entities: Map<String, JsonObject> = emptyMap(),
    val queries: Map<String, List<String>> = emptyMap(),
)

/**
 * What the device knows beyond the engine's state, for [deviceData]: the inbox's newest notifications, the
 * notification channels, the pod's sign-ins and the providers not signed in, and the platform's servers.
 */
data class DeviceSources(
    val notifications: List<AppNotification> = emptyList(),
    val channels: List<NotificationChannelInfo> = emptyList(),
    val signIns: List<SignInSummary> = emptyList(),
    val providers: List<SignInProvider> = emptyList(),
    val server: ServerChoice? = null,
    val nowSeconds: Long = 0,
    /** The version pinned per app on this device. */
    val pins: Map<String, Long> = emptyMap(),
    /** The app `show_versions` named, and its history as loaded. */
    val versionsApp: String? = null,
    val versions: VersionHistory? = null,
)

/**
 * The device data for the current state: `local://connection` (online, pending, label, attention), `local://inbox`
 * (unread, label), `local://apps` (count) and the `local:apps` query of the pod's apps but home (`local://apps/<id>`:
 * id, title, icon, kind, detail, unread), `local://device` (platform), `local://session` (open, server, error,
 * message), and from [sources] the `local:notifications`, `local:channels`, `local:sign_ins`, `local:providers` and
 * `local:servers` queries.
 */
fun deviceData(
    state: ConstructState,
    unread: UnreadCounts,
    apps: List<AppSummary>,
    platform: String,
    sources: DeviceSources = DeviceSources(),
): DeviceData {
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
        // For a status dot (`core://Box` `background`): green online, grey otherwise.
        put("color", if (state.isOnline == true) "#2E7D32" else "#9E9E9E")
    }
    entities["local://inbox"] = buildJsonObject {
        put("unread", unread.total)
        put("label", if (unread.total == 0) "All read" else "${unread.total} unread")
    }
    val listed = apps.filter { it.id != HOME_APP && !it.hidden }
    fun row(app: AppSummary) = buildJsonObject {
            put("id", app.id)
            put("title", app.title)
            put("icon", app.icon)
            put("kind", if (app.custom) "custom" else "blueprint")
            put("detail", app.updatesFrom?.let { "${it.title} has updates" } ?: app.latestVersion?.let { "v$it" } ?: "")
            put("unread", unread.byApp[app.id] ?: 0)
            val pinned = sources.pins[app.id]
            put("version", if (pinned != null) "v$pinned · pinned" else app.latestVersion?.let { "v$it · latest" } ?: "")
        }
    // `local:layouts` (decision `layouts-not-apps`); `local:apps`, its old name, for layouts written before.
    val layoutUris = listed.map { app -> "local://layouts/${app.id}".also { entities[it] = row(app) } }
    val uris = listed.map { app -> "local://apps/${app.id}".also { entities[it] = row(app) } }
    entities["local://layouts"] = buildJsonObject { put("count", listed.size) }
    entities["local://apps"] = buildJsonObject { put("count", listed.size) }
    entities["local://device"] = buildJsonObject { put("platform", platform) }
    entities["local://session"] = buildJsonObject {
        put("open", state.auth.session)
        put("server", sources.server?.currentUrl.orEmpty())
        put("error", state.auth.error.orEmpty())
        put("message", state.auth.message.orEmpty())
        // A sign-in in progress or failed, for Settings: the engine's progress message, or the error.
        put(
            "status",
            when (state.auth.state) {
                "pending" -> state.auth.message ?: "Signing in…"
                "error" -> state.auth.error.orEmpty()
                else -> ""
            },
        )
    }
    val titles = apps.associate { it.id to it.title }
    val queries = LinkedHashMap<String, List<String>>()
    queries["local:layouts"] = layoutUris
    queries["local:apps"] = uris
    fun add(query: String, uri: String, entity: JsonObject): String {
        entities[uri] = entity
        return uri
    }
    queries["local:notifications"] = sources.notifications.map { n ->
        add("local:notifications", "local://notifications/${n.id}", buildJsonObject {
            put("id", n.id)
            put("app_id", n.appId)
            put("app_title", titles[n.appId] ?: n.appId)
            put("title", n.title)
            put("body", n.body)
            put("when", notificationTimeLabel(n.createdAt, sources.nowSeconds))
            put("day", notificationDayLabel(n.createdAt, sources.nowSeconds))
            put("unread", !n.read)
        })
    }
    queries["local:channels"] = sources.channels.map { c ->
        add("local:channels", "local://channels/${c.appId}/${c.id}", buildJsonObject {
            put("app_id", c.appId)
            put("app_title", titles[c.appId] ?: c.appId)
            put("id", c.id)
            put("title", c.title)
            put("description", c.description)
            put("enabled", c.enabled)
        })
    }
    queries["local:sign_ins"] = sources.signIns.map { s ->
        add("local:sign_ins", "local://sign_ins/${s.id}", buildJsonObject {
            put("id", s.id)
            put("title", s.title)
            put("detail", listOfNotNull(s.username?.let { "@$it" }, s.serverHost).joinToString(" · "))
        })
    }
    val listedSignIns = sources.signIns.map { it.id }.toSet()
    queries["local:providers"] = sources.providers
        .filter { !it.signedIn && it.intent != null && it.id !in listedSignIns }
        .map { p ->
            add("local:providers", "local://providers/${p.id}", buildJsonObject {
                put("id", p.id)
                put("title", p.title)
                put("custom_server", p.serverPlaceholder != null)
                put("server_placeholder", p.serverPlaceholder.orEmpty())
            })
        }
    val server = sources.server
    queries["local:servers"] = server?.options.orEmpty().map { o ->
        add("local:servers", "local://servers/${o.key}", buildJsonObject {
            put("id", o.key)
            put("title", o.label)
            put("url", o.url)
            put("selected", o.url.trimEnd('/') == server?.currentUrl?.trimEnd('/'))
        })
    }
    val versionsApp = sources.versionsApp
    val pinnedVersion = versionsApp?.let { sources.pins[it] }
    entities["local://versions"] = buildJsonObject {
        put("app_id", versionsApp.orEmpty())
        put("app_title", versionsApp?.let { titles[it] ?: it }.orEmpty())
        put("pinned", pinnedVersion?.toString().orEmpty())
        put("loading", versionsApp != null && sources.versions?.loaded != true)
    }
    val history = sources.versions?.versions.orEmpty()
    val newest = history.firstOrNull { it.compatible }?.version
    queries["local:versions"] = history.map { v ->
        add("local:versions", "local://versions/$versionsApp/${v.version}", buildJsonObject {
            put("app_id", versionsApp.orEmpty())
            put("version", v.version)
            put("label", "v${v.version}")
            put("detail", versionDetail(v))
            put("when", versionTimeLabel(v.createdAt, sources.nowSeconds))
            put("compatible", v.compatible)
            put("showing", if (pinnedVersion != null) v.version == pinnedVersion else v.version == newest)
        })
    }
    return DeviceData(entities, queries)
}

/** The device data, for every layout on screen (provided by [ConstructApp]). */
val LocalDeviceData: ProvidableCompositionLocal<DeviceData> = staticCompositionLocalOf { DeviceData() }

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

/** What the root draws for the pod's home. */
internal enum class ShellView { Loading, Bootstrap, Rescue, Home }

/**
 * Bootstrap until the engine holds a pod session; home once the engine has it; the rescue layout when the engine has
 * no home it can draw and says why; loading meanwhile.
 */
internal fun shellView(state: ConstructState): ShellView = when {
    state.seq >= 0 && !state.auth.session -> ShellView.Bootstrap
    state.workspace != null -> ShellView.Home
    state.error != null -> ShellView.Rescue
    else -> ShellView.Loading
}

/**
 * Whether a workspace is drawn whole by its one slot (a filling `core://Box`, or an inline `core://Layout`), with no
 * title bar or page scroll.
 */
internal fun fillsScreen(workspace: UiNode.Workspace): Boolean =
    workspace.slots.size == 1 &&
        workspace.slots[0].content.let {
            (it is UiNode.Box && it.fill) || (it is UiNode.Layout && it.presentation == EmbedPresentation.Inline && it.showWhen == null)
        }
