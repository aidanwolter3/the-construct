package org.theconstruct

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.EnterTransition
import androidx.compose.animation.ExitTransition
import androidx.compose.animation.core.MutableTransitionState
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.ProvidableCompositionLocal
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.longOrNull

// Layouts embedded in and presented over other layouts (decision `layouts-not-apps`). There are no apps at runtime:
// one root layout (home) embeds others with `core://Layout`, or presents them on the back stack with
// `action.navigation.open {layout}`. One engine holds them all; the stack is [Navigation]'s, and system Back pops it.

/** Another layout the engine holds, from the snapshot's `layouts_json`. [gone]: it was deleted. */
data class LayoutSnapshot(val workspace: UiNode?, val version: Long? = null, val gone: Boolean = false) {
    companion object {
        /** Lenient: an entry that doesn't parse is left out; an unreadable layout is null. */
        fun parseAll(json: String): Map<String, LayoutSnapshot> = try {
            ConstructJson.json.parseToJsonElement(json).jsonObject.mapNotNull { (id, v) ->
                val o = v as? JsonObject ?: return@mapNotNull null
                val workspace = (o["workspace"] as? JsonObject)?.let {
                    try {
                        ConstructJson.json.decodeFromJsonElement(UiNode.serializer(), it)
                    } catch (e: Exception) {
                        null
                    }
                }
                val version = (o["version"] as? JsonPrimitive)?.longOrNull
                val gone = (o["gone"] as? JsonPrimitive)?.booleanOrNull == true
                id to LayoutSnapshot(workspace, version, gone)
            }.toMap()
        } catch (e: Exception) {
            emptyMap()
        }
    }
}

/** The data-load key of another layout (`loads_json`), as the engine names it. */
fun layoutLoadKey(layoutId: String): String = "layout:$layoutId"

/** The controller of the layouts on screen (for `core://Layout`); null outside [ConstructApp]. */
val LocalController: ProvidableCompositionLocal<ConstructController?> = staticCompositionLocalOf { null }

/** The layouts drawing this one, innermost last: a layout never embeds itself. */
internal val LocalLayoutChain: ProvidableCompositionLocal<List<String>> = staticCompositionLocalOf { emptyList() }

/** A `core://Layout` inline here fills the space given (a workspace's one slot) rather than its content's height. */
internal val LocalFillLayout: ProvidableCompositionLocal<Boolean> = staticCompositionLocalOf { false }

/** The workspace being drawn is inline in another layout's page: its slots only, no title, no scroll of its own. */
internal val LocalInlineWorkspace: ProvidableCompositionLocal<Boolean> = staticCompositionLocalOf { false }

/** The notice for a layout that was deleted. */
const val LAYOUT_DELETED = "This was deleted."

/**
 * Layout [layoutId] drawn over the device data, with [params] as its `$route.<name>` values and [form] as its form.
 * Asks the engine for it when it first shows; a spinner until the engine has it, a message (and Try again) when it
 * can't be drawn.
 */
@Composable
internal fun LayoutBody(
    controller: ConstructController,
    layoutId: String,
    params: Map<String, String>,
    form: androidx.compose.runtime.snapshots.SnapshotStateMap<String, String>,
    modifier: Modifier = Modifier,
    inline: Boolean = false,
) {
    val state by controller.state.collectAsState()
    val pins by controller.pins.collectAsState()
    val chain = LocalLayoutChain.current
    val device = LocalDeviceData.current
    if (layoutId in chain) {
        LayoutMessage("A layout can't show itself inside itself.", modifier)
        return
    }
    LaunchedEffect(layoutId) { controller.openLayout(layoutId) }
    val workspace = remember(state, pins, layoutId, params) {
        controller.workspaceOf(layoutId, state, pins)?.let { bindRouteParams(it, params) }
    }
    val snapshot = state.layouts[layoutId]
    val load = state.loads[layoutLoadKey(layoutId)]
    when {
        workspace == null && snapshot?.gone == true -> LayoutMessage(LAYOUT_DELETED, modifier)
        workspace == null && load?.state == "error" -> LayoutMessage(
            load.error ?: "This can't be shown.",
            modifier,
            onRetry = { controller.openLayout(layoutId) },
        )
        workspace == null -> Box(
            if (inline) modifier.fillMaxWidth().padding(16.dp) else modifier.fillMaxSize(),
            contentAlignment = Alignment.Center,
        ) {
            CircularProgressIndicator(Modifier.size(24.dp).testTag("layout-loading"))
        }
        else -> {
            val entities = remember(state.entities, device) { state.entities + device.entities }
            val queries = remember(state.queries, device) { state.queries + device.queries }
            val scope = "layout:$layoutId"
            CompositionLocalProvider(
                LocalConstructQueries provides queries,
                LocalForm provides form,
                LocalFormMemory provides remember(controller, scope) {
                    FormMemory { f, v -> controller.navigation.remember(scope, f, v) }
                },
                LocalLayoutChain provides chain + layoutId,
                LocalFillLayout provides false,
                LocalInlineWorkspace provides inline,
            ) {
                RenderUiNode(
                    workspace,
                    entities,
                    { controller.dispatchIn(layoutId, form, it) },
                    modifier.testTag("layout-body:$layoutId"),
                )
            }
        }
    }
}

@Composable
private fun LayoutMessage(text: String, modifier: Modifier, onRetry: (() -> Unit)? = null) {
    Column(
        modifier.fillMaxWidth().padding(24.dp).testTag("layout-message"),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(text, style = MaterialTheme.typography.bodyMedium, textAlign = TextAlign.Center)
        if (onRetry != null) TextButton(onClick = onRetry) { Text("Try again") }
    }
}

private var nodeKeys = 0

/**
 * `core://Layout`: [UiNode.Layout.layout] drawn in place ([EmbedPresentation.Inline]: [UiNode.Layout.heightDp] tall, the
 * whole space as a workspace's one slot, else as tall as its content), or presented as a sheet or a side panel while
 * `show_when` holds. Dismissing a presented one clears the `$form` field `show_when` names.
 */
@Composable
internal fun LayoutNodeView(
    node: UiNode.Layout,
    entities: Map<String, JsonObject>,
    item: JsonObject?,
    modifier: Modifier = Modifier,
) {
    val controller = LocalController.current
    if (controller == null) {
        LayoutMessage("Layouts can't be shown here.", modifier)
        return
    }
    val form = LocalForm.current
    val layoutId = (resolveRef(node.layout, item) ?: interpolate(node.layout, entities, item)).trim()
    val params = node.params.mapValues { (_, v) -> resolveRef(v, item) ?: interpolate(v, entities, item) }
    val visible = node.showWhen == null || conditionShows(node.showWhen, node.equals, item, form, entities)
    if (layoutId.isEmpty()) return
    when (node.presentation) {
        EmbedPresentation.Inline -> {
            val ownForm = remember(layoutId) { mutableStateMapOf<String, String>() }
            val fill = LocalFillLayout.current
            val state = remember { MutableTransitionState(false) }
            state.targetState = visible
            AnimatedVisibility(
                visibleState = state,
                enter = enterFor(node.animation ?: Animation.Fade, PresentationMode.Push),
                exit = exitFor(node.animation ?: Animation.Fade, PresentationMode.Push),
            ) {
                when {
                    node.heightDp != null -> LayoutBody(
                        controller, layoutId, params, ownForm, modifier.fillMaxWidth().height(node.heightDp.dp),
                    )
                    fill -> LayoutBody(controller, layoutId, params, ownForm, modifier.fillMaxSize())
                    else -> LayoutBody(controller, layoutId, params, ownForm, modifier.fillMaxWidth(), inline = true)
                }
            }
        }
        EmbedPresentation.Sheet, EmbedPresentation.Panel -> {
            val nav = controller.navigation
            val nodeKey = remember { "node-${nodeKeys++}" }
            val presentation =
                if (node.presentation == EmbedPresentation.Sheet) PresentationMode.Sheet else PresentationMode.Panel
            // Dismissing clears the form value that shows it (`$form.<field>`), so it stays closed.
            val field = node.showWhen?.trim()?.removePrefix("\$form.")?.takeIf { it != node.showWhen?.trim() }
            LaunchedEffect(visible, layoutId) {
                if (visible) {
                    nav.presentFromNode(nodeKey, layoutId, params, presentation, node.animation) {
                        if (field != null) form?.remove(field)
                    }
                } else {
                    nav.dismissNode(nodeKey)
                }
            }
            DisposableEffect(nodeKey) { onDispose { nav.dismissNode(nodeKey) } }
        }
    }
}

/** How a layout presented as [presentation] comes in. */
internal fun enterFor(animation: Animation?, presentation: PresentationMode): EnterTransition =
    when (animation ?: defaultAnimation(presentation)) {
        Animation.None -> EnterTransition.None
        Animation.Fade -> fadeIn()
        Animation.SheetUp -> slideInVertically { it }
        Animation.Slide -> if (presentation == PresentationMode.Panel) slideInHorizontally { -it } else slideInHorizontally { it }
    }

/** How a layout presented as [presentation] goes out. */
internal fun exitFor(animation: Animation?, presentation: PresentationMode): ExitTransition =
    when (animation ?: defaultAnimation(presentation)) {
        Animation.None -> ExitTransition.None
        Animation.Fade -> fadeOut()
        Animation.SheetUp -> slideOutVertically { it }
        Animation.Slide -> if (presentation == PresentationMode.Panel) slideOutHorizontally { -it } else slideOutHorizontally { it }
    }

/** Push slides in from the end, Sheet slides up, Panel slides in from the start. */
internal fun defaultAnimation(presentation: PresentationMode): Animation = when (presentation) {
    PresentationMode.Sheet -> Animation.SheetUp
    PresentationMode.Panel -> Animation.Slide
    else -> Animation.Slide
}

/**
 * The back stack over the root layout: every presented layout and screen in order, each animated in and out. System
 * Back pops the top entry; a scrim tap dismisses a sheet or panel. Entries that just left stay drawn until they have
 * animated out.
 */
@Composable
internal fun StackHost(controller: ConstructController, state: ConstructState, device: DeviceData, settingsExtra: (@Composable () -> Unit)?) {
    val nav = controller.navigation
    PlatformBackHandler(enabled = nav.stack.isNotEmpty()) { nav.back() }
    // Not Compose state: written while composing. Entries that leave are dropped after their exit animation, which
    // [gone] then recomposes for.
    val shown = remember { mutableListOf<StackEntry>() }
    var gone by remember { mutableIntStateOf(0) }
    gone.let { }
    val live = nav.stack.toList()
    // New entries join at the end; removed ones stay until their exit animation ends.
    live.forEach { if (it !in shown) shown.add(it) }
    val transitions = remember { mutableMapOf<Int, MutableTransitionState<Boolean>>() }
    for (entry in shown.toList()) {
        key(entry.key) {
            val inStack = entry in live
            val t = transitions.getOrPut(entry.key) { MutableTransitionState(false) }
            t.targetState = inStack
            if (!inStack && t.isIdle && !t.currentState) {
                LaunchedEffect(Unit) {
                    shown.remove(entry)
                    transitions.remove(entry.key)
                    gone++
                }
            }
            StackEntryView(controller, state, device, entry, t, settingsExtra)
        }
    }
}

@Composable
private fun StackEntryView(
    controller: ConstructController,
    state: ConstructState,
    device: DeviceData,
    entry: StackEntry,
    transition: MutableTransitionState<Boolean>,
    settingsExtra: (@Composable () -> Unit)?,
) {
    val nav = controller.navigation
    val presentation = (entry as? LayoutEntry)?.presentation ?: PresentationMode.Push
    val animation = (entry as? LayoutEntry)?.animation
    val overlay = presentation == PresentationMode.Sheet || presentation == PresentationMode.Panel
    Box(Modifier.fillMaxSize()) {
        if (overlay) {
            AnimatedVisibility(visibleState = transition, enter = fadeIn(), exit = fadeOut()) {
                Box(
                    Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.32f)).testTag("scrim")
                        .clickable(interactionSource = remember { MutableInteractionSource() }, indication = null) {
                            nav.dismiss(entry)
                        },
                )
            }
        }
        AnimatedVisibility(
            visibleState = transition,
            enter = enterFor(animation, presentation),
            exit = exitFor(animation, presentation),
            modifier = Modifier.align(
                when (presentation) {
                    PresentationMode.Sheet -> Alignment.BottomCenter
                    PresentationMode.Panel -> Alignment.CenterStart
                    else -> Alignment.Center
                },
            ),
        ) {
            when (entry) {
                is ScreenEntry -> ScreenView(controller, state, entry, device)
                is LayoutEntry -> when (entry.presentation) {
                    PresentationMode.Sheet -> SheetFrame(entry) { PresentedBody(controller, entry, settingsExtra) }
                    PresentationMode.Panel -> PanelFrame(entry) { PresentedBody(controller, entry, settingsExtra) }
                    else -> PushFrame(controller, entry) { PresentedBody(controller, entry, settingsExtra) }
                }
            }
        }
    }
}

/** A presented layout's body; the platform's Settings extra (e.g. Android's notification permission) above Settings. */
@Composable
private fun PresentedBody(controller: ConstructController, entry: LayoutEntry, settingsExtra: (@Composable () -> Unit)?) {
    Column(Modifier.fillMaxSize()) {
        if (entry.layoutId == SETTINGS_LAYOUT && settingsExtra != null) settingsExtra()
        LayoutBody(controller, entry.layoutId, entry.params, entry.form, Modifier.fillMaxWidth().weight(1f))
    }
}

/** A pushed layout: the whole screen, with Back at the top start beside its title. */
@Composable
private fun PushFrame(controller: ConstructController, entry: LayoutEntry, content: @Composable () -> Unit) {
    Surface(Modifier.fillMaxSize().testTag("layout:${entry.layoutId}"), color = MaterialTheme.colorScheme.background) {
        Box(Modifier.fillMaxSize()) {
            CompositionLocalProvider(LocalWorkspaceTitleInset provides PUSH_BACK_INSET) { content() }
            IconButton(
                onClick = { controller.navigation.dismiss(entry) },
                modifier = Modifier.align(Alignment.TopStart).padding(start = 4.dp, top = 4.dp).testTag("layout-back"),
            ) {
                Icon(ConstructIcons.ArrowBack, contentDescription = "Back")
            }
        }
    }
}

/** A bottom sheet up to most of the screen's height, with a handle. */
@Composable
private fun SheetFrame(entry: LayoutEntry, content: @Composable () -> Unit) {
    BoxWithConstraints(Modifier.fillMaxWidth()) {
        Surface(
            shape = RoundedCornerShape(topStart = 16.dp, topEnd = 16.dp),
            color = MaterialTheme.colorScheme.surface,
            shadowElevation = 8.dp,
            modifier = Modifier.fillMaxWidth().height(maxHeight * SHEET_HEIGHT_FRACTION).testTag("sheet:${entry.layoutId}"),
        ) {
            Column(Modifier.fillMaxSize()) {
                Box(Modifier.fillMaxWidth().padding(top = 8.dp, bottom = 4.dp), contentAlignment = Alignment.Center) {
                    Box(
                        Modifier.width(32.dp).height(4.dp)
                            .background(MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.4f), RoundedCornerShape(2.dp)),
                    )
                }
                Box(Modifier.fillMaxWidth().weight(1f)) { content() }
            }
        }
    }
}

/** A side panel from the start edge: at most [PANEL_MAX_WIDTH], and most of a phone's width. */
@Composable
private fun PanelFrame(entry: LayoutEntry, content: @Composable () -> Unit) {
    BoxWithConstraints(Modifier.fillMaxHeight()) {
        val width = minOf(PANEL_MAX_WIDTH, maxWidth * 0.86f)
        Surface(
            color = MaterialTheme.colorScheme.surface,
            shadowElevation = 8.dp,
            shape = RoundedCornerShape(topEnd = 16.dp, bottomEnd = 16.dp),
            modifier = Modifier.fillMaxHeight().width(width).testTag("panel:${entry.layoutId}"),
        ) {
            content()
        }
    }
}

/** The Settings blueprint's id (the platform's Settings extra goes above it). */
const val SETTINGS_LAYOUT = "settings"

/** The inbox blueprint's id. */
const val INBOX_LAYOUT = "inbox"

/** How much of the screen's height a sheet takes. */
private const val SHEET_HEIGHT_FRACTION = 0.85f

/** The widest a side panel gets. */
private val PANEL_MAX_WIDTH = 340.dp

/** Start padding of a pushed layout's title, beside its Back button (4 dp + 48 dp button + 4 dp). */
private val PUSH_BACK_INSET = 44.dp
