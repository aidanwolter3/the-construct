package org.theconstruct

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
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

/** One open screen: [screen] already route-bound; [form] holds its form values. */
@Stable
class ScreenEntry internal constructor(
    val key: Int,
    val screen: Screen,
    val params: Map<String, String>,
) {
    val form: SnapshotStateMap<String, String> = mutableStateMapOf()
}

/** The form values of the root layout and of each open screen (Compose state). */
val LocalForm: ProvidableCompositionLocal<SnapshotStateMap<String, String>?> = staticCompositionLocalOf { null }

/**
 * The back stack, owned by [ConstructController] (survives rotation, like the menu). [open] pushes the screen of the
 * displayed layout whose route matches (and asks the engine to load its data); [back] pops.
 */
@Stable
class Navigation internal constructor(private val controller: ConstructController) {
    val stack = mutableStateListOf<ScreenEntry>()
    val rootForm: SnapshotStateMap<String, String> = mutableStateMapOf()
    private var nextKey = 0

    val top: ScreenEntry? get() = stack.lastOrNull()

    /** The form the visible screen edits (the root layout's when no screen is open). */
    fun activeForm(): SnapshotStateMap<String, String> = top?.form ?: rootForm

    /** Navigation intents this client resolves; true when handled (not sent to the engine). */
    internal fun handle(intent: ActionIntent): Boolean = when (intent) {
        is ActionIntent.Navigate -> open(intent)
        ActionIntent.NavigateBack -> back()
        else -> false
    }

    /** Opens the screen whose route matches; false when no screen of the displayed layout does. */
    fun open(intent: ActionIntent.Navigate): Boolean {
        val workspace = displayedWorkspace(controller.state.value, controller.pinned.value) as? UiNode.Workspace
            ?: return false
        for (screen in workspace.screens) {
            val params = matchRoute(screen.route, intent.targetUri) ?: continue
            val entry = ScreenEntry(nextKey++, bindRoute(screen, params), params)
            if (intent.presentation == PresentationMode.Replace && stack.isNotEmpty()) {
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

    /** Pops the top screen (discarding the photos its form still holds); false when none is open. */
    fun back(): Boolean {
        if (stack.isEmpty()) return false
        val entry = stack.removeAt(stack.lastIndex)
        controller.attachments.release(entry.form)
        return true
    }

    /**
     * Closes the top screen after a create with `dismiss` (schema 9) and clears its form; its photos now belong to that
     * create. False when no screen is open.
     */
    internal fun dismiss(): Boolean {
        if (stack.isEmpty()) return false
        val entry = stack.removeAt(stack.lastIndex)
        controller.attachments.release(entry.form)
        entry.form.clear()
        return true
    }

    /** Loads the top screen's data again. */
    fun retry() {
        top?.screen?.data?.let { controller.send(ActionIntent.LoadData(it)) }
    }

    /** Closes every screen (another app is opening). */
    fun clear() {
        stack.clear()
        rootForm.clear()
        controller.attachments.forgetAll()
    }
}

/**
 * The top screen over the layout (in-tree, like the menu: headless captures see it): a bar with Back and the title, the
 * data load's progress or error (with Try again), the content (scrolling) and the footer. Tags: `screen:<id>`,
 * `screen-back`, `screen-loading`, `screen-error`, `screen-retry`.
 */
@Composable
internal fun ScreenHost(controller: ConstructController, state: ConstructState) {
    val nav = controller.navigation
    val entry = nav.top ?: return
    PlatformBackHandler(enabled = !controller.menu.open) { nav.back() }
    key(entry.key) {
        Surface(Modifier.fillMaxSize().testTag("screen:${entry.screen.id}"), color = MaterialTheme.colorScheme.background) {
            CompositionLocalProvider(
                LocalForm provides entry.form,
                LocalConstructQueries provides state.queries,
            ) {
                Column(Modifier.fillMaxSize(), horizontalAlignment = Alignment.CenterHorizontally) {
                    Column(Modifier.fillMaxSize().widthIn(max = FEED_MAX_WIDTH)) {
                        Row(
                            Modifier.fillMaxWidth().padding(start = 4.dp, end = 12.dp, top = 4.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            IconButton(onClick = { nav.back() }, modifier = Modifier.testTag("screen-back")) {
                                Icon(ConstructIcons.ArrowBack, contentDescription = "Back")
                            }
                            Text(
                                interpolate(entry.screen.title, state.entities),
                                style = MaterialTheme.typography.titleLarge,
                                modifier = Modifier.semantics { heading() },
                            )
                        }
                        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.5f))
                        val load = entry.screen.data?.let { state.loads[it] }
                        LoadStatusRow(load, onRetry = nav::retry)
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
                        Column(Modifier.weight(1f).fillMaxWidth().verticalScroll(scroll)) {
                            RenderUiNode(entry.screen.content, state.entities, controller::dispatch, Modifier.fillMaxWidth())
                        }
                        entry.screen.footer?.let { footer ->
                            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.5f))
                            RenderUiNode(
                                footer, state.entities,
                                { intent ->
                                    controller.dispatch(intent)
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
}

@Composable
private fun LoadStatusRow(load: LoadState?, onRetry: () -> Unit) {
    when (load?.state) {
        "loading" -> Row(
            Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp).testTag("screen-loading"),
            horizontalArrangement = Arrangement.Center,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
            Spacer(Modifier.width(8.dp))
            Text("Loading…", style = MaterialTheme.typography.bodySmall, color = Color(0xFF737373))
        }
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
