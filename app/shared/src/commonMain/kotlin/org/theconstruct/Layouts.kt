package org.theconstruct

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.EnterTransition
import androidx.compose.animation.ExitTransition
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.MutableTransitionState
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.animate
import androidx.compose.animation.core.spring
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.Orientation
import androidx.compose.foundation.gestures.draggable
import androidx.compose.foundation.gestures.rememberDraggableState
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.absoluteOffset
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
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
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.Velocity
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.longOrNull
import kotlin.math.roundToInt

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

/** The page being drawn is inline in another layout's page: its content's height, no scroll of its own. */
internal val LocalInlineLayout: ProvidableCompositionLocal<Boolean> = staticCompositionLocalOf { false }

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
                LocalInlineLayout provides inline,
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
 * whole space as a workspace's one slot, else as tall as its content), or presented as a sheet, a side panel or an
 * overlay from an edge while `show_when` holds. Dismissing a presented one clears the `$form` field `show_when` names.
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
        EmbedPresentation.Sheet, EmbedPresentation.Panel, EmbedPresentation.Overlay -> {
            val nav = controller.navigation
            val nodeKey = remember { "node-${nodeKeys++}" }
            val presentation = when (node.presentation) {
                EmbedPresentation.Sheet -> PresentationMode.Sheet
                EmbedPresentation.Panel -> PresentationMode.Panel
                else -> PresentationMode.Overlay
            }
            // Dismissing clears the form value that shows it (`$form.<field>`), so it stays closed.
            val field = node.showWhen?.trim()?.removePrefix("\$form.")?.takeIf { it != node.showWhen?.trim() }
            LaunchedEffect(visible, layoutId) {
                if (visible) {
                    nav.presentFromNode(
                        nodeKey, layoutId, params, presentation, node.animation,
                        onDismiss = { if (field != null) form?.remove(field) },
                        edge = node.edge,
                        sizeDp = node.sizeDp,
                    )
                } else {
                    nav.dismissNode(nodeKey)
                }
            }
            DisposableEffect(nodeKey) { onDispose { nav.dismissNode(nodeKey) } }
        }
    }
}

/**
 * How a layout presented as [presentation] comes in. An Overlay slides in from its [edge] (decision
 * `edge-overlays-and-gestures`) unless [animation] is fade or none; [rtl] mirrors start and end.
 */
internal fun enterFor(
    animation: Animation?,
    presentation: PresentationMode,
    edge: Edge? = null,
    rtl: Boolean = false,
): EnterTransition {
    if (presentation == PresentationMode.Overlay) {
        return when (animation) {
            Animation.None -> EnterTransition.None
            Animation.Fade -> fadeIn()
            else -> when (edge ?: Edge.Bottom) {
                Edge.Top -> slideInVertically { -it }
                Edge.Bottom -> slideInVertically { it }
                Edge.Start -> slideInHorizontally { if (rtl) it else -it }
                Edge.End -> slideInHorizontally { if (rtl) -it else it }
            }
        }
    }
    return when (animation ?: defaultAnimation(presentation)) {
        Animation.None -> EnterTransition.None
        Animation.Fade -> fadeIn()
        Animation.SheetUp -> slideInVertically { it }
        Animation.Slide -> if (presentation == PresentationMode.Panel) slideInHorizontally { -it } else slideInHorizontally { it }
    }
}

/** How a layout presented as [presentation] goes out: the way [enterFor] brought it in, reversed. */
internal fun exitFor(
    animation: Animation?,
    presentation: PresentationMode,
    edge: Edge? = null,
    rtl: Boolean = false,
): ExitTransition {
    if (presentation == PresentationMode.Overlay) {
        return when (animation) {
            Animation.None -> ExitTransition.None
            Animation.Fade -> fadeOut()
            else -> when (edge ?: Edge.Bottom) {
                Edge.Top -> slideOutVertically { -it }
                Edge.Bottom -> slideOutVertically { it }
                Edge.Start -> slideOutHorizontally { if (rtl) it else -it }
                Edge.End -> slideOutHorizontally { if (rtl) -it else it }
            }
        }
    }
    return when (animation ?: defaultAnimation(presentation)) {
        Animation.None -> ExitTransition.None
        Animation.Fade -> fadeOut()
        Animation.SheetUp -> slideOutVertically { it }
        Animation.Slide -> if (presentation == PresentationMode.Panel) slideOutHorizontally { -it } else slideOutHorizontally { it }
    }
}

/** Push slides in from the end, Sheet slides up, Panel slides in from the start. */
internal fun defaultAnimation(presentation: PresentationMode): Animation = when (presentation) {
    PresentationMode.Sheet -> Animation.SheetUp
    PresentationMode.Panel -> Animation.Slide
    else -> Animation.Slide
}

/**
 * The back stack over the root layout: every presented layout and screen in order, each animated in and out. System
 * Back pops the top entry; a scrim tap dismisses a sheet, panel or overlay, and so does dragging it toward its edge.
 * Entries that just left stay drawn until they have animated out.
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
    // A screen opened inside a sheet or panel is drawn by that frame ([FrameContent]), not over the whole screen.
    val live = nav.stack.filter { (it as? ScreenEntry)?.frame == null }
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
    // A sheet, panel or overlay: drawn from this edge, over a scrim (decision `edge-overlays-and-gestures`).
    val edge = (entry as? LayoutEntry)?.overlayEdge
    val rtl = LocalLayoutDirection.current == LayoutDirection.Rtl
    // How far it is dragged toward its edge; its scrim fades as it goes.
    val drag = remember { SheetDrag() }
    Box(Modifier.fillMaxSize()) {
        if (edge != null) {
            AnimatedVisibility(visibleState = transition, enter = fadeIn(), exit = fadeOut()) {
                Box(
                    Modifier.fillMaxSize().graphicsLayer { alpha = 1f - drag.fraction }
                        .background(Color.Black.copy(alpha = 0.32f)).testTag("scrim")
                        .clickable(interactionSource = remember { MutableInteractionSource() }, indication = null) {
                            nav.dismiss(entry)
                        },
                )
            }
        }
        AnimatedVisibility(
            visibleState = transition,
            enter = enterFor(animation, presentation, edge, rtl),
            exit = exitFor(animation, presentation, edge, rtl),
            modifier = Modifier.align(
                when (edge) {
                    Edge.Bottom -> Alignment.BottomCenter
                    Edge.Top -> Alignment.TopCenter
                    Edge.Start -> Alignment.CenterStart
                    Edge.End -> Alignment.CenterEnd
                    null -> Alignment.Center
                },
            ),
        ) {
            // Where the layout is drawn, for a plumbing page's own Back (`local://nav`, feature `nav_data`).
            val navContext = when (edge) {
                Edge.Bottom, Edge.Top -> NavContext("sheet", canGoBack = true, inFrame = true)
                Edge.Start, Edge.End -> NavContext("panel", canGoBack = true, inFrame = true)
                null -> NavContext("push", canGoBack = true)
            }
            CompositionLocalProvider(LocalNav provides navContext) {
                when (entry) {
                    is ScreenEntry -> ScreenView(controller, state, entry, device)
                    is LayoutEntry -> when {
                        edge != null -> OverlayFrame(entry, edge, drag, onDismiss = { nav.dismiss(entry) }) {
                            FrameContent(controller, state, device, entry) { PresentedBody(controller, entry, settingsExtra) }
                        }
                        else -> PushFrame(entry) { PresentedBody(controller, entry, settingsExtra) }
                    }
                }
            }
        }
    }
}

/**
 * A sheet's or panel's content (issue #119): its layout ([base]), and over it the screens opened inside it, in order.
 * Opening one slides the content left and the screen in from the end, like a push inside the frame; Back (the screen's
 * arrow or system Back) slides it out again. Layers underneath stay composed (their scroll survives) but leave the
 * semantics tree. Screens that left stay drawn until they have slid out; while the frame itself leaves, it keeps
 * showing what it showed.
 */
@Composable
private fun FrameContent(
    controller: ConstructController,
    state: ConstructState,
    device: DeviceData,
    frame: LayoutEntry,
    base: @Composable () -> Unit,
) {
    val nav = controller.navigation
    val last = remember { arrayOf<List<ScreenEntry>>(emptyList()) }
    val live = if (frame in nav.stack) {
        nav.stack.filterIsInstance<ScreenEntry>().filter { it.frame === frame }.also { last[0] = it }
    } else {
        last[0]
    }
    // Not Compose state, as in [StackHost]: screens that left are dropped after sliding out, recomposing via [gone].
    val shown = remember { mutableListOf<ScreenEntry>() }
    var gone by remember { mutableIntStateOf(0) }
    gone.let { }
    live.forEach { if (it !in shown) shown.add(it) }
    val transitions = remember { mutableMapOf<Int, MutableTransitionState<Boolean>>() }
    Box(Modifier.fillMaxSize().clipToBounds()) {
        FrameLayer(covered = live.isNotEmpty()) { base() }
        for (entry in shown.toList()) {
            key(entry.key) {
                val inFrame = entry in live
                val t = transitions.getOrPut(entry.key) { MutableTransitionState(false) }
                t.targetState = inFrame
                if (!inFrame && t.isIdle && !t.currentState) {
                    LaunchedEffect(Unit) {
                        shown.remove(entry)
                        transitions.remove(entry.key)
                        gone++
                    }
                }
                AnimatedVisibility(
                    visibleState = t,
                    enter = slideInHorizontally(tween(FRAME_SLIDE_MS, easing = FastOutSlowInEasing)) { it },
                    exit = slideOutHorizontally(tween(FRAME_SLIDE_MS, easing = FastOutSlowInEasing)) { it },
                ) {
                    FrameLayer(covered = inFrame && entry !== live.last()) {
                        ScreenView(controller, state, entry, device)
                    }
                }
            }
        }
    }
}

/** One layer of [FrameContent]: slid off to the start (and out of the semantics tree) while a screen covers it. */
@Composable
private fun FrameLayer(covered: Boolean, content: @Composable () -> Unit) {
    val shift by animateFloatAsState(if (covered) 1f else 0f, tween(FRAME_SLIDE_MS, easing = FastOutSlowInEasing))
    Box(
        Modifier.fillMaxSize()
            .graphicsLayer { translationX = -shift * size.width }
            .then(if (covered) Modifier.clearAndSetSemantics { } else Modifier),
    ) { content() }
}

/** A presented layout's body; the platform's Settings extra (e.g. Android's notification permission) above Settings. */
@Composable
private fun PresentedBody(controller: ConstructController, entry: LayoutEntry, settingsExtra: (@Composable () -> Unit)?) {
    Column(Modifier.fillMaxSize()) {
        if (entry.layoutId == SETTINGS_LAYOUT && settingsExtra != null) settingsExtra()
        LayoutBody(controller, entry.layoutId, entry.params, entry.form, Modifier.fillMaxWidth().weight(1f))
    }
}

/** A pushed layout: the whole screen. Its page draws its own Back (decision `plumbing-and-porcelain`). */
@Composable
private fun PushFrame(entry: LayoutEntry, content: @Composable () -> Unit) {
    Surface(Modifier.fillMaxSize().testTag("layout:${entry.layoutId}"), color = MaterialTheme.colorScheme.background) {
        content()
    }
}

/**
 * An overlay's drag-to-dismiss state (issues #113, #123): how far it is pulled toward its edge from where it rests, in
 * px, shared with its scrim. [vertical] and [sign] say which way that is on screen (a bottom sheet: down, +1; a top
 * one: up, -1; a panel from the left: left, -1). [settle] ends a drag: past [SHEET_DISMISS_FRACTION] of its size or a
 * fling toward the edge animates it out and dismisses it, anything else springs it back.
 */
private class SheetDrag {
    var offset by mutableFloatStateOf(0f)

    /** Its height (top, bottom) or width (start, end), in px. */
    var extent by mutableFloatStateOf(0f)

    /** A finger (or mouse button) is down on the content: only then does a scroll pull the overlay. */
    var pressed = false

    // Set by [OverlayFrame] each composition: plain fields, read only from gesture callbacks.
    var vertical = true
    var sign = 1f
    var scope: CoroutineScope? = null
    var flingPx = 0f
    var onDismiss: () -> Unit = {}
    private var settling: Job? = null

    /** 0 at rest, 1 when pulled toward its edge by its whole size. */
    val fraction: Float get() = if (extent > 0f) (offset / extent).coerceIn(0f, 1f) else 0f

    /** Where it is drawn, from where it rests, in screen px. */
    val shift: IntOffset get() = (sign * offset).roundToInt().let { if (vertical) IntOffset(0, it) else IntOffset(it, 0) }

    /** Moves it by a drag of [delta] screen px along its axis. */
    fun dragBy(delta: Float) {
        settling?.cancel()
        offset = (offset + sign * delta).coerceAtLeast(0f)
    }

    /** Ends a drag released at [velocity] screen px/s along its axis. */
    fun settle(velocity: Float) {
        val toward = sign * velocity
        val dismiss = toward > flingPx || (offset > extent * SHEET_DISMISS_FRACTION && toward >= -flingPx)
        if (!dismiss && offset == 0f) return
        settling?.cancel()
        settling = scope?.launch {
            animate(
                initialValue = offset,
                targetValue = if (dismiss) extent else 0f,
                initialVelocity = toward,
                animationSpec = spring(stiffness = Spring.StiffnessMediumLow),
            ) { value, _ -> offset = value.coerceAtLeast(0f) }
            if (dismiss) onDismiss()
        }
    }

    private fun along(o: Offset): Float = if (vertical) o.y else o.x
    private fun offsetAlong(v: Float): Offset = if (vertical) Offset(0f, v) else Offset(v, 0f)

    /**
     * Pulls the overlay toward its edge from its content: a scroll that way the content has no room for (a bottom
     * sheet's content at its top, a top sheet's at its bottom) moves the overlay, and one the other way first takes it
     * back. Only while [pressed], so a mouse wheel never moves it.
     */
    val nestedScroll = object : NestedScrollConnection {
        override fun onPreScroll(available: Offset, source: NestedScrollSource): Offset {
            val a = along(available)
            if (!pressed || offset <= 0f || sign * a >= 0f) return Offset.Zero
            val before = offset
            dragBy(a)
            return offsetAlong(sign * (offset - before))
        }

        override fun onPostScroll(consumed: Offset, available: Offset, source: NestedScrollSource): Offset {
            val a = along(available)
            if (!pressed || sign * a <= 0f) return Offset.Zero
            dragBy(a)
            return offsetAlong(a)
        }

        override suspend fun onPreFling(available: Velocity): Velocity {
            if (offset <= 0f) return Velocity.Zero
            settle(if (vertical) available.y else available.x)
            return available
        }
    }
}

/**
 * A layout over the screen from [edge] (decision `edge-overlays-and-gestures`). From the bottom or the top it is a sheet
 * up to most of the screen's height, with a handle on its inner side; from the start or the end a side panel at most
 * [PANEL_MAX_WIDTH] wide. [LayoutEntry.sizeDp] sets the height or width. Dragging it toward its edge (a sheet's handle,
 * a panel anywhere), or pulling the content that way once it can't scroll further, moves it with the finger;
 * [SheetDrag.settle] then dismisses it ([onDismiss]) or springs it back.
 */
@Composable
private fun OverlayFrame(
    entry: LayoutEntry,
    edge: Edge,
    drag: SheetDrag,
    onDismiss: () -> Unit,
    content: @Composable () -> Unit,
) {
    val rtl = LocalLayoutDirection.current == LayoutDirection.Rtl
    drag.scope = rememberCoroutineScope()
    drag.flingPx = with(LocalDensity.current) { SHEET_FLING_VELOCITY.toPx() }
    drag.onDismiss = onDismiss
    drag.vertical = edge == Edge.Top || edge == Edge.Bottom
    drag.sign = when (edge) {
        Edge.Bottom -> 1f
        Edge.Top -> -1f
        Edge.Start -> if (rtl) 1f else -1f
        Edge.End -> if (rtl) -1f else 1f
    }
    val handleDrag = rememberDraggableState { delta -> drag.dragBy(delta) }
    val contentArea = Modifier
        .pointerInput(drag) {
            // Watches (never consumes) whether a pointer is down, for [SheetDrag.nestedScroll].
            awaitPointerEventScope {
                while (true) {
                    val event = awaitPointerEvent(PointerEventPass.Initial)
                    drag.pressed = event.changes.any { it.pressed }
                }
            }
        }
        .nestedScroll(drag.nestedScroll)
    if (drag.vertical) {
        val top = edge == Edge.Top
        BoxWithConstraints(Modifier.fillMaxWidth()) {
            val height = entry.sizeDp?.dp?.coerceAtMost(maxHeight) ?: (maxHeight * SHEET_HEIGHT_FRACTION)
            Surface(
                shape = if (top) {
                    RoundedCornerShape(bottomStart = 16.dp, bottomEnd = 16.dp)
                } else {
                    RoundedCornerShape(topStart = 16.dp, topEnd = 16.dp)
                },
                color = MaterialTheme.colorScheme.surface,
                shadowElevation = 8.dp,
                modifier = Modifier.fillMaxWidth().height(height)
                    .absoluteOffset { drag.shift }
                    .onSizeChanged { drag.extent = it.height.toFloat() }
                    .testTag("sheet:${entry.layoutId}"),
            ) {
                Column(Modifier.fillMaxSize()) {
                    if (!top) SheetHandle(handleDrag, drag, top = false)
                    Box(Modifier.fillMaxWidth().weight(1f).then(contentArea)) { content() }
                    if (top) SheetHandle(handleDrag, drag, top = true)
                }
            }
        }
    } else {
        val start = edge == Edge.Start
        BoxWithConstraints(Modifier.fillMaxHeight()) {
            val width = entry.sizeDp?.dp?.coerceAtMost(maxWidth) ?: minOf(PANEL_MAX_WIDTH, maxWidth * 0.86f)
            Surface(
                color = MaterialTheme.colorScheme.surface,
                shadowElevation = 8.dp,
                shape = if (start) {
                    RoundedCornerShape(topEnd = 16.dp, bottomEnd = 16.dp)
                } else {
                    RoundedCornerShape(topStart = 16.dp, bottomStart = 16.dp)
                },
                modifier = Modifier.fillMaxHeight().width(width)
                    .absoluteOffset { drag.shift }
                    .onSizeChanged { drag.extent = it.width.toFloat() }
                    .draggable(handleDrag, Orientation.Horizontal, onDragStopped = { velocity -> drag.settle(velocity) })
                    .then(contentArea)
                    .testTag("panel:${entry.layoutId}"),
            ) {
                content()
            }
        }
    }
}

/** A sheet's handle strip, on its inner side ([top]: a top sheet's, at its bottom); dragging it moves the sheet. */
@Composable
private fun SheetHandle(state: androidx.compose.foundation.gestures.DraggableState, drag: SheetDrag, top: Boolean) {
    Box(
        Modifier.fillMaxWidth()
            .draggable(state, Orientation.Vertical, onDragStopped = { velocity -> drag.settle(velocity) })
            .padding(top = if (top) 4.dp else 8.dp, bottom = if (top) 8.dp else 4.dp)
            .testTag("sheet-handle"),
        contentAlignment = Alignment.Center,
    ) {
        Box(
            Modifier.width(32.dp).height(4.dp)
                .background(MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.4f), RoundedCornerShape(2.dp)),
        )
    }
}

/** The Settings blueprint's id (the platform's Settings extra goes above it). */
const val SETTINGS_LAYOUT = "settings"

/** The inbox blueprint's id. */
const val INBOX_LAYOUT = "inbox"

/** How much of the screen's height a sheet takes. */
private const val SHEET_HEIGHT_FRACTION = 0.85f

/** A sheet or panel released past this much of its size closes. */
private const val SHEET_DISMISS_FRACTION = 0.25f

/** A sheet or panel flung toward its edge faster than this closes, however little it moved. */
private val SHEET_FLING_VELOCITY = 500.dp

/** How long a screen takes to slide into or out of a sheet or panel (and its content aside). */
private const val FRAME_SLIDE_MS = 280

/** The widest a side panel gets. */
private val PANEL_MAX_WIDTH = 340.dp
