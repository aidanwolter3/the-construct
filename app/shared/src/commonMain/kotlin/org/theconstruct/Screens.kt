package org.theconstruct

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.ProvidableCompositionLocal
import androidx.compose.runtime.Stable
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.snapshots.SnapshotStateMap
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import kotlin.random.Random
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject

// Screens, the back stack and form values (DECISIONS D34). Nothing here knows any app: the layout declares screens
// (route patterns, a data path, content) and the client matches `action.navigation.open` targets against them.

/** Load state of a data path, from the snapshot's `loads_json`. [state]: loading, ready or error. */
data class LoadState(val state: String, val error: String? = null) {
    companion object {
        /** Lenient: entries that don't parse are skipped. */
        fun parseAll(json: String): Map<String, LoadState> = try {
            ConstructJson.json.parseToJsonElement(json).jsonObject.mapNotNull { (path, v) ->
                val o = v as? JsonObject ?: return@mapNotNull null
                val state = (o["state"] as? JsonPrimitive)?.contentOrNull ?: return@mapNotNull null
                path to LoadState(state, (o["error"] as? JsonPrimitive)?.takeIf { it !is JsonNull }?.contentOrNull)
            }.toMap()
        } catch (t: Exception) {
            emptyMap()
        }
    }
}

private val ROUTE_PARAM = Regex("""^\{([a-z_]+)\}$""")
private val PARAM_VALUE = Regex("""^[A-Za-z0-9_-]{1,64}$""")

/**
 * The parameters [uri] binds in [route] (`scheme://a/{id}/b`), or null when it doesn't match: same scheme, same number
 * of segments, literal segments equal, each `{name}` segment a value of `[A-Za-z0-9_-]{1,64}`.
 */
fun matchRoute(route: String, uri: String): Map<String, String>? {
    val (rScheme, rPath) = route.split("://", limit = 2).takeIf { it.size == 2 } ?: return null
    val (uScheme, uPath) = uri.trim().split("://", limit = 2).takeIf { it.size == 2 } ?: return null
    if (rScheme != uScheme) return null
    val rSegs = rPath.split('/')
    val uSegs = uPath.split('/')
    if (rSegs.size != uSegs.size) return null
    val params = LinkedHashMap<String, String>()
    for ((r, u) in rSegs.zip(uSegs)) {
        val name = ROUTE_PARAM.find(r)?.groupValues?.get(1)
        if (name != null) {
            if (!PARAM_VALUE.matches(u)) return null
            params[name] = u
        } else if (r != u) {
            return null
        }
    }
    return params
}

private val ROUTE_TOKEN = Regex("""\${'$'}route\.([A-Za-z_][A-Za-z0-9_]*)""")
private val FORM_TOKEN = Regex("""\${'$'}form(\?)?\.([A-Za-z_][A-Za-z0-9_]*)|\${'$'}nonce""")
private val WHOLE_FORM_TOKEN = Regex("""^\${'$'}form(\?)?\.([A-Za-z_][A-Za-z0-9_]*)$""")

/** Replaces every string in [e] (except the values of keys in [skip]) with [f] of it. */
private fun mapStrings(e: JsonElement, skip: Set<String> = emptySet(), f: (String) -> String): JsonElement =
    mapStringElements(e, skip) { JsonPrimitive(f(it)) }

/** Replaces every string in [e] (except the values of keys in [skip]) with the element [f] makes of it. */
private fun mapStringElements(e: JsonElement, skip: Set<String> = emptySet(), f: (String) -> JsonElement): JsonElement = when (e) {
    is JsonObject -> JsonObject(e.mapValues { (k, v) -> if (k in skip) v else mapStringElements(v, skip, f) })
    is JsonArray -> JsonArray(e.map { mapStringElements(it, skip, f) })
    is JsonPrimitive -> if (e.isString) f(e.content) else e
}

/**
 * A list form value (a `core://MediaPicker`'s staged attachment ids) as its items: the value is a JSON array of strings
 * such as `["a","b"]`. Null when [value] is not one (an ordinary text value, or unset).
 */
fun parseFormList(value: String?): List<String>? {
    val v = value?.trim() ?: return null
    if (!v.startsWith("[") || !v.endsWith("]")) return null
    val arr = try {
        ConstructJson.json.parseToJsonElement(v) as? JsonArray
    } catch (e: Exception) {
        null
    } ?: return null
    return arr.map { (it as? JsonPrimitive)?.takeIf { p -> p.isString }?.content ?: return null }
}

/** [items] as a list form value ([parseFormList]). */
fun encodeFormList(items: List<String>): String = JsonArray(items.map(::JsonPrimitive)).toString()

/** [screen] with every `$route.<name>` replaced by [params] (unknown names are left as they are). */
fun bindRoute(screen: Screen, params: Map<String, String>): Screen {
    val json = ConstructJson.json.encodeToJsonElement(Screen.serializer(), screen)
    val bound = mapStrings(json) { s -> ROUTE_TOKEN.replace(s) { m -> params[m.groupValues[1]] ?: m.value } }
    return ConstructJson.json.decodeFromJsonElement(Screen.serializer(), bound)
}

/** A random nonce for `$nonce` (24 hex characters): one per dispatch, e.g. a temporary URI or an idempotency key. */
fun newNonce(random: Random = Random.Default): String =
    random.nextBytes(12).joinToString("") { (it.toInt() and 0xFF).toString(16).padStart(2, '0') }

/**
 * Binds `$form.<field>` (from [form]) and `$nonce` (one fresh value for the whole intent) in every string of [intent]
 * except its `value_expr`s, in one pass (a value is never re-scanned, so typed text containing `$nonce` stays as
 * typed). Returns null, so nothing is dispatched, when a field referenced as `$form.<field>` is blank (a list value,
 * [parseFormList], is blank when empty). `$form?.<field>` (schema 9) binds a blank field to "" and never blocks.
 * A string that is exactly `$form.<field>` (or `$form?.<field>`) of a list value becomes that JSON array, e.g.
 * `"attachments": {"photos": ["a","b"]}`.
 */
fun bindForm(intent: ActionIntent, form: Map<String, String>, nonce: () -> String = { newNonce() }): ActionIntent? {
    val encoded = ConstructJson.encodeIntent(intent)
    if (!encoded.contains("\$form.") && !encoded.contains("\$form?.") && !encoded.contains("\$nonce")) return intent
    var missing = false
    val n by lazy(nonce)
    val json = ConstructJson.json.parseToJsonElement(encoded)
    val bound = mapStringElements(json, skip = setOf("intent", "value_expr")) { s ->
        WHOLE_FORM_TOKEN.matchEntire(s)?.let { m ->
            val optional = m.groupValues[1].isNotEmpty()
            val list = parseFormList(form[m.groupValues[2]])
            if (list != null) {
                if (list.isEmpty() && !optional) missing = true
                return@mapStringElements JsonArray(list.map(::JsonPrimitive))
            }
        }
        JsonPrimitive(
            FORM_TOKEN.replace(s) { m ->
                if (m.value == "\$nonce") {
                    n
                } else {
                    val raw = form[m.groupValues[2]]
                    val v = if (parseFormList(raw)?.isEmpty() == true) "" else raw?.trim().orEmpty()
                    if (v.isEmpty() && m.groupValues[1].isEmpty()) missing = true
                    v
                }
            },
        )
    }
    if (missing) return null
    return ConstructJson.json.decodeFromJsonElement(ActionIntent.serializer(), bound)
}

/**
 * One entry of the back stack (decision `layouts-not-apps`): a presented layout ([LayoutEntry]) or a screen of a
 * layout ([ScreenEntry]). [layoutId] is the layout it belongs to; [form] holds its form values.
 */
@Stable
sealed class StackEntry(val key: Int) {
    abstract val layoutId: String
    val form: SnapshotStateMap<String, String> = mutableStateMapOf()
}

/**
 * One open screen of layout [layoutId]: [screen] already route-bound. [frame] is the Sheet or Panel it opened inside
 * (issue #119): it is drawn within that frame, not over the whole screen.
 */
@Stable
class ScreenEntry internal constructor(
    key: Int,
    val screen: Screen,
    val params: Map<String, String>,
    override val layoutId: String,
    val frame: LayoutEntry? = null,
) : StackEntry(key)

/**
 * A presented layout: [presentation] Push (the whole screen), Sheet, Panel or Overlay (from [edge], [sizeDp] tall or
 * wide); [params] are its `$route.<name>` values. [nodeKey] is set when a `core://Layout` presents it while its
 * `show_when` holds; [onDismiss] then clears that.
 */
@Stable
class LayoutEntry internal constructor(
    key: Int,
    override val layoutId: String,
    val params: Map<String, String>,
    val presentation: PresentationMode,
    val animation: Animation?,
    internal val nodeKey: String? = null,
    internal val onDismiss: (() -> Unit)? = null,
    val edge: Edge? = null,
    val sizeDp: Int? = null,
) : StackEntry(key) {
    /**
     * The edge it is drawn over the screen from (decision `edge-overlays-and-gestures`): a Sheet's is the bottom, a
     * Panel's the start, an Overlay's its [edge] (the bottom when unset). Null for a pushed layout.
     */
    val overlayEdge: Edge? get() = overlayEdgeOf(presentation, edge)
}

/** The edge [presentation] draws a layout over the screen from, or null when it covers the whole screen. */
internal fun overlayEdgeOf(presentation: PresentationMode, edge: Edge?): Edge? = when (presentation) {
    PresentationMode.Sheet -> Edge.Bottom
    PresentationMode.Panel -> Edge.Start
    PresentationMode.Overlay -> edge ?: Edge.Bottom
    else -> null
}

/** The form values of the root layout and of each stack entry (Compose state). */
val LocalForm: ProvidableCompositionLocal<SnapshotStateMap<String, String>?> = staticCompositionLocalOf { null }

/** [workspace] with every `$route.<name>` replaced by [params] (a presented layout's parameters). */
fun bindRouteParams(workspace: UiNode, params: Map<String, String>): UiNode {
    if (params.isEmpty()) return workspace
    val json = ConstructJson.json.encodeToJsonElement(UiNode.serializer(), workspace)
    val bound = mapStrings(json) { s -> ROUTE_TOKEN.replace(s) { m -> params[m.groupValues[1]] ?: m.value } }
    return ConstructJson.json.decodeFromJsonElement(UiNode.serializer(), bound)
}

/**
 * The back stack, owned by [ConstructController] (survives rotation): presented layouts and screens, in order. System
 * Back pops its top ([back]); nothing else needs a Back of its own. [open] presents a layout, or pushes the screen of
 * the layout on top whose route matches (and asks the engine to load its data).
 *
 * Remembered form values (schema 10): the root form is filled from the device when a layout shows a remembered field
 * for the first time ([restoreRoot]), a screen's form when the screen opens; [remember] saves a new choice.
 */
@Stable
class Navigation internal constructor(private val controller: ConstructController) {
    val stack = mutableStateListOf<StackEntry>()
    val rootForm: SnapshotStateMap<String, String> = mutableStateMapOf()
    private var nextKey = 0

    /** Remembered root fields already restored (snapshots arrive on any thread: guarded). */
    private val rootRestored = mutableSetOf<String>()

    /**
     * Restores the remembered fields of [workspace] (the displayed root layout) that have not been restored yet,
     * before the layout is drawn. A field the user changed meanwhile keeps its value. Reads the device only when a new
     * remembered field appears.
     */
    internal fun restoreRoot(workspace: UiNode?) {
        val nodes = when (workspace) {
            is UiNode.Workspace -> workspace.slots.map { it.content }
            is UiNode.Page -> listOf(workspace.root)
            else -> return
        }
        val choices = rememberedChoices(nodes)
        if (choices.isEmpty()) return
        synchronized(rootRestored) {
            val fresh = choices.filterKeys { it !in rootRestored }
            if (fresh.isEmpty()) return
            applyRemembered(rootForm, fresh, controller.rememberedFormValues(ROOT_FORM_SCOPE))
            rootRestored += fresh.keys
        }
    }

    /** Saves a remembered value of form [scope] on the device (null = the default option). */
    fun remember(scope: String, field: String, value: String?) {
        controller.rememberFormValue(scope, field, value)
    }

    val top: StackEntry? get() = stack.lastOrNull()

    /** The topmost open screen, if any. */
    val topScreen: ScreenEntry? get() = stack.lastOrNull() as? ScreenEntry

    /**
     * The Sheet or Panel the top of the stack is drawn in: the top entry when it is one, or the frame of the screen on
     * top. A screen opened now opens inside it. Null when the top is the root, a pushed layout or a full screen.
     */
    val topFrame: LayoutEntry? get() = when (val t = top) {
        is LayoutEntry -> t.takeIf { it.overlayEdge != null }
        is ScreenEntry -> t.frame
        null -> null
    }

    /** The form the visible entry edits (the root layout's when nothing is open). */
    fun activeForm(): SnapshotStateMap<String, String> = top?.form ?: rootForm

    /** The layout on top: the top entry's, else the root. */
    fun currentLayout(): String = top?.layoutId ?: controller.rootId

    /** Navigation intents this client resolves; true when handled (not sent to the engine). */
    internal fun handle(intent: ActionIntent, from: String? = null): Boolean = when (intent) {
        is ActionIntent.Navigate -> open(intent, from)
        ActionIntent.NavigateBack -> back()
        else -> false
    }

    /** Whether a screen of layout [layoutId] (the one on top when null) matches [targetUri]. */
    fun canOpen(targetUri: String, layoutId: String? = null): Boolean {
        val workspace = controller.workspaceOf(layoutId ?: currentLayout()) ?: return false
        return screensOf(workspace).any { matchRoute(it.route, targetUri) != null }
    }

    /**
     * `action.navigation.open`: with a `layout`, presents it (then opens its screen matching `target_uri`, once it is
     * loaded); otherwise opens the screen of layout [from] (the one on top when null, falling back to the root) whose
     * route matches. False when nothing matched.
     */
    fun open(intent: ActionIntent.Navigate, from: String? = null): Boolean {
        val layout = intent.layout?.trim()?.takeIf { it.isNotEmpty() }
        if (layout != null) {
            present(layout, intent.params, intent.presentation, intent.animation, intent.edge, intent.sizeDp)
            if (intent.targetUri.isNotBlank()) controller.openWhenLoaded(layout, intent.targetUri.trim())
            return true
        }
        val owner = from ?: currentLayout()
        return openScreen(intent.targetUri, owner, intent.presentation) ||
            (owner != controller.rootId && openScreen(intent.targetUri, controller.rootId, intent.presentation))
    }

    /**
     * Pushes the screen of [layoutId] whose route matches [targetUri]; false when none does. While a Sheet or Panel is
     * on top ([topFrame]) and the screen is one of its layout's, the screen opens inside that frame (issue #119); a
     * screen of another layout (the root's, as a fallback) still covers the whole screen.
     */
    internal fun openScreen(targetUri: String, layoutId: String, presentation: PresentationMode = PresentationMode.Push): Boolean {
        val workspace = controller.workspaceOf(layoutId) ?: return false
        for (screen in screensOf(workspace)) {
            val params = matchRoute(screen.route, targetUri) ?: continue
            val frame = topFrame?.takeIf { it.layoutId == layoutId }
            val entry = ScreenEntry(nextKey++, bindRoute(screen, params), params, layoutId, frame)
            val choices = rememberedChoices(listOfNotNull(entry.screen.content, entry.screen.footer, entry.screen.root))
            if (choices.isNotEmpty()) applyRemembered(entry.form, choices, controller.rememberedFormValues(screen.id))
            if (presentation == PresentationMode.Replace && stack.lastOrNull() is ScreenEntry) {
                controller.attachments.release(stack[stack.lastIndex].form)
                stack[stack.lastIndex] = entry
            } else {
                stack.add(entry)
            }
            entry.screen.data?.let { controller.send(ActionIntent.LoadData(it)) }
            return true
        }
        return false
    }

    /**
     * Presents layout [layoutId] on the stack: Push (Modal too) the whole screen, Sheet a bottom sheet, Panel a side
     * panel, Overlay one from [edge]. Replace swaps the top presented layout. The engine loads the layout if it hasn't
     * yet.
     */
    fun present(
        layoutId: String,
        params: Map<String, String> = emptyMap(),
        presentation: PresentationMode = PresentationMode.Push,
        animation: Animation? = null,
        edge: Edge? = null,
        sizeDp: Int? = null,
    ): LayoutEntry {
        val shown = if (presentation == PresentationMode.Modal) PresentationMode.Push else presentation
        val entry = LayoutEntry(nextKey++, layoutId, params, shown, animation, edge = edge, sizeDp = sizeDp)
        if (presentation == PresentationMode.Replace && stack.lastOrNull() is LayoutEntry) {
            release(stack.removeAt(stack.lastIndex))
            stack.add(LayoutEntry(entry.key, layoutId, params, PresentationMode.Push, animation))
        } else {
            stack.add(entry)
        }
        controller.openLayout(layoutId)
        return stack.last() as LayoutEntry
    }

    /**
     * A `core://Layout`'s Sheet, Panel or Overlay ([nodeKey]) is shown while its `show_when` holds; [onDismiss] clears
     * it.
     */
    internal fun presentFromNode(
        nodeKey: String,
        layoutId: String,
        params: Map<String, String>,
        presentation: PresentationMode,
        animation: Animation?,
        edge: Edge? = null,
        sizeDp: Int? = null,
        onDismiss: () -> Unit,
    ) {
        if (stack.any { (it as? LayoutEntry)?.nodeKey == nodeKey }) return
        stack.add(LayoutEntry(nextKey++, layoutId, params, presentation, animation, nodeKey, onDismiss, edge, sizeDp))
        controller.openLayout(layoutId)
    }

    /** The `core://Layout` [nodeKey] no longer shows: its entry (and what is above it) goes. */
    internal fun dismissNode(nodeKey: String) {
        val i = stack.indexOfFirst { (it as? LayoutEntry)?.nodeKey == nodeKey }
        if (i < 0) return
        while (stack.size > i) release(stack.removeAt(stack.lastIndex))
    }

    /** Pops the top entry (discarding the photos its form still holds); false when nothing is open. */
    fun back(): Boolean {
        if (stack.isEmpty()) return false
        val entry = stack.removeAt(stack.lastIndex)
        release(entry)
        (entry as? LayoutEntry)?.onDismiss?.invoke()
        return true
    }

    /** Pops [entry] and everything above it (a scrim tap on a sheet under another entry). */
    fun dismiss(entry: StackEntry) {
        val i = stack.indexOf(entry)
        if (i < 0) return
        while (stack.size > i) back()
    }

    private fun release(entry: StackEntry) {
        controller.attachments.release(entry.form)
    }

    /** Loads the top screen's data again. */
    fun retry() {
        topScreen?.screen?.data?.let { controller.send(ActionIntent.LoadData(it)) }
    }

    /** Closes everything (leaving the pod). */
    fun clear() {
        stack.clear()
        synchronized(rootRestored) {
            rootForm.clear()
            rootRestored.clear()
        }
        controller.attachments.forgetAll()
    }
}

/**
 * An open screen over its layout (in-tree: headless captures see it): a bar with Back, the title and a spinner while
 * the data loads, the load's error (with Try again), the content (scrolling) and the footer. Tags: `screen:<id>`,
 * `screen-back`, `screen-loading`, `screen-error`, `screen-retry`.
 */
@Composable
internal fun ScreenView(controller: ConstructController, state: ConstructState, entry: ScreenEntry, device: DeviceData) {
    val nav = controller.navigation
    val entities = remember(state.entities, device) { state.entities + device.entities }
    val queries = remember(state.queries, device) { state.queries + device.queries }
    val dispatch: (ActionIntent) -> Unit = { controller.dispatchIn(entry.layoutId, entry.form, it) }
    val root = entry.screen.root
    if (root != null) {
        PageScreenView(entry, root, state, entities, queries, dispatch)
        return
    }
    val content = entry.screen.content ?: return
    // Inside a sheet or panel it takes the frame's colour.
    val color = if (entry.frame != null) MaterialTheme.colorScheme.surface else MaterialTheme.colorScheme.background
    Surface(Modifier.fillMaxSize().testTag("screen:${entry.screen.id}"), color = color) {
        val scope = entry.screen.id
        CompositionLocalProvider(
            LocalForm provides entry.form,
            LocalFormMemory provides remember(scope) { FormMemory { f, v -> nav.remember(scope, f, v) } },
            LocalConstructQueries provides queries,
        ) {
            Column(Modifier.fillMaxSize(), horizontalAlignment = Alignment.CenterHorizontally) {
                Column(Modifier.fillMaxSize().widthIn(max = FEED_MAX_WIDTH)) {
                    val load = entry.screen.data?.let { state.loads[it] }
                    Row(
                        Modifier.fillMaxWidth().padding(start = 4.dp, end = 12.dp, top = 4.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        IconButton(onClick = { nav.dismiss(entry) }, modifier = Modifier.testTag("screen-back")) {
                            Icon(ConstructIcons.ArrowBack, contentDescription = "Back")
                        }
                        Text(
                            interpolate(entry.screen.title, entities),
                            style = MaterialTheme.typography.titleLarge,
                            modifier = Modifier.weight(1f).semantics { heading() },
                        )
                        // Progress sits in the bar (its height is fixed by the Back button), so the content
                        // below doesn't jump when the load finishes.
                        if (load?.state == "loading") {
                            CircularProgressIndicator(
                                Modifier.size(16.dp).testTag("screen-loading").semantics { contentDescription = "Loading" },
                                strokeWidth = 2.dp,
                            )
                        }
                    }
                    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.5f))
                    LoadErrorRow(load, onRetry = nav::retry)
                    val scroll = rememberScrollState()
                    // After a submit from the footer (e.g. a new item at the end of a list), follow the content
                    // to its end once it grows.
                    var followEnd by remember { mutableStateOf(false) }
                    LaunchedEffect(scroll.maxValue, followEnd) {
                        if (followEnd && scroll.maxValue > 0) {
                            scroll.animateScrollTo(scroll.maxValue)
                            followEnd = false
                        }
                    }
                    // The bar above does not scroll, so `core://Sticky` content pins right at the top.
                    val sticky = remember(scroll) { StickyScroll(scroll) }
                    Column(Modifier.weight(1f).fillMaxWidth().verticalScroll(scroll).stickyContent(sticky)) {
                        CompositionLocalProvider(LocalStickyScroll provides sticky) {
                            RenderUiNode(content, entities, dispatch, Modifier.fillMaxWidth())
                        }
                    }
                    entry.screen.footer?.let { footer ->
                        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.5f))
                        RenderUiNode(
                            footer, entities,
                            { intent ->
                                dispatch(intent)
                                followEnd = true
                            },
                            Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp),
                        )
                    }
                }
            }
        }
    }
}

/**
 * A `core://Page` screen (feature `page`): its root over the whole screen, with no chrome from the app; `local://screen`
 * is its data load and `$item` the entity its `item` names (feature `screen_item`). Tag: `screen:<id>`.
 */
@Composable
private fun PageScreenView(
    entry: ScreenEntry,
    root: UiNode,
    state: ConstructState,
    entities: Map<String, JsonObject>,
    queries: Map<String, List<String>>,
    dispatch: (ActionIntent) -> Unit,
) {
    val controller = LocalController.current
    val load = entry.screen.data?.let { state.loads[it] }
    val withScreen = remember(entities, load) { entities + ("local://screen" to screenEntity(load)) }
    val item = entry.screen.item?.let { uri -> entities[uri]?.let { JsonObject(it + ("uri" to JsonPrimitive(uri))) } }
    val color = if (entry.frame != null) MaterialTheme.colorScheme.surface else MaterialTheme.colorScheme.background
    Surface(Modifier.fillMaxSize().testTag("screen:${entry.screen.id}"), color = color) {
        val scope = entry.screen.id
        CompositionLocalProvider(
            LocalForm provides entry.form,
            LocalFormMemory provides remember(scope) {
                FormMemory { f, v -> controller?.navigation?.remember(scope, f, v) }
            },
            LocalConstructQueries provides queries,
            LocalNav provides NavContext("screen", canGoBack = true, inFrame = entry.frame != null),
        ) {
            PlumbingRoot(root, withScreen, dispatch, Modifier.fillMaxSize(), item)
        }
    }
}

@Composable
private fun LoadErrorRow(load: LoadState?, onRetry: () -> Unit) {
    when (load?.state) {
        "error" -> Row(
            Modifier.fillMaxWidth().padding(start = 16.dp, end = 8.dp, top = 4.dp, bottom = 4.dp).testTag("screen-error"),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                load.error ?: "Couldn't load this.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = onRetry, modifier = Modifier.testTag("screen-retry")) { Text("Try again") }
        }
        else -> {}
    }
}
