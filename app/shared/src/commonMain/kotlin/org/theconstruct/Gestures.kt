package org.theconstruct

import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.PointerInputScope
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChange
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import kotlinx.serialization.json.JsonObject
import kotlin.math.abs

// `core://Gesture` (feature `gesture`, decision `edge-overlays-and-gestures`): any node with gesture handlers, each an
// ordinary intent. `core://Tappable` is the tap case.

/** Which way a swipe went, on screen. */
internal enum class SwipeDirection { Up, Down, Left, Right }

/** [node]'s intent for a swipe [direction], if it has one. */
internal fun swipeHandler(node: UiNode.Gesture, direction: SwipeDirection): ActionIntent? = when (direction) {
    SwipeDirection.Up -> node.onSwipeUp
    SwipeDirection.Down -> node.onSwipeDown
    SwipeDirection.Left -> node.onSwipeLeft
    SwipeDirection.Right -> node.onSwipeRight
}

/**
 * The direction of a drag of ([dx], [dy]) px, or null: it must go at least [threshold] px, mostly one way (the main
 * axis at least [SWIPE_AXIS_RATIO] times the other).
 */
internal fun swipeDirection(dx: Float, dy: Float, threshold: Float): SwipeDirection? {
    val ax = abs(dx)
    val ay = abs(dy)
    return when {
        maxOf(ax, ay) < threshold -> null
        ay >= ax * SWIPE_AXIS_RATIO -> if (dy > 0f) SwipeDirection.Down else SwipeDirection.Up
        ax >= ay * SWIPE_AXIS_RATIO -> if (dx > 0f) SwipeDirection.Right else SwipeDirection.Left
        else -> null
    }
}

/**
 * `core://Gesture`: [UiNode.Gesture.content] drawn with its handlers on the content's own modifier (like
 * `core://Tappable`), so the whole drawn area takes the gestures. A tap or long press runs `on_tap` / `on_long_press`;
 * a button inside keeps its own taps. A swipe runs its direction's intent when the finger lifts.
 */
@Composable
internal fun GestureNodeView(
    node: UiNode.Gesture,
    entities: Map<String, JsonObject>,
    onDispatch: (ActionIntent) -> Unit,
    item: JsonObject?,
    modifier: Modifier,
) {
    val dispatch: (ActionIntent) -> Unit = { onDispatch(bindIntent(it, item)) }
    val description = node.contentDescription?.let { interpolate(it, entities, item) }
    val current by rememberUpdatedState(node)
    val currentDispatch by rememberUpdatedState(dispatch)
    val tracker = remember { SwipeTracker() }
    tracker.thresholdPx = with(LocalDensity.current) { SWIPE_DISTANCE.toPx() }
    tracker.handles = { d -> swipeHandler(current, d) != null }
    tracker.onSwipe = { d -> swipeHandler(current, d)?.let(currentDispatch) }
    var mod = modifier
    if (node.onTap != null || node.onLongPress != null) {
        val long = node.onLongPress
        mod = mod.combinedClickable(
            onClickLabel = description,
            onLongClick = long?.let { { dispatch(it) } },
            onClick = { node.onTap?.let(dispatch) },
        )
    }
    if (SwipeDirection.entries.any { swipeHandler(node, it) != null }) {
        // Inside the click handling: a swipe that fires takes the lift, so it isn't a tap too.
        mod = mod.pointerInput(tracker) { trackSwipes(tracker) }.nestedScroll(tracker.nestedScroll)
    }
    if (description != null) mod = mod.semantics { contentDescription = description }
    RenderUiNode(node.content, entities, onDispatch, mod, item)
}

/**
 * One gesture's swipe so far: the finger's movement that nothing inside used (content that doesn't scroll), plus the
 * scroll the content had no room for (a page pulled down at its top), via [nestedScroll]. A drag an outer scroll takes
 * is not a swipe.
 */
internal class SwipeTracker {
    var thresholdPx = 0f
    var handles: (SwipeDirection) -> Boolean = { false }
    var onSwipe: (SwipeDirection) -> Unit = {}

    private var dx = 0f
    private var dy = 0f

    /** A pointer is down: only then do scrolls count, so a mouse wheel never swipes. */
    private var pressed = false

    /** Something outside took the drag (an outer scroll): this gesture isn't a swipe. */
    private var stolen = false

    fun start() {
        dx = 0f
        dy = 0f
        stolen = false
        pressed = true
    }

    fun move(delta: Offset) {
        dx += delta.x
        dy += delta.y
    }

    fun steal() {
        stolen = true
    }

    /** The swipe this gesture is, if it is one this node handles. */
    fun recognized(): SwipeDirection? =
        if (stolen) null else swipeDirection(dx, dy, thresholdPx)?.takeIf(handles)

    /** The finger lifted: runs the swipe's intent, if it is one; true when it did. */
    fun end(): Boolean {
        pressed = false
        val d = recognized() ?: return false
        onSwipe(d)
        return true
    }

    /** Scroll the content had no room for counts toward the swipe; a handled direction's is kept from outer scrolls. */
    val nestedScroll = object : NestedScrollConnection {
        override fun onPostScroll(consumed: Offset, available: Offset, source: NestedScrollSource): Offset {
            if (!pressed || available == Offset.Zero) return Offset.Zero
            move(available)
            val x = available.x.takeIf {
                (it < 0f && handles(SwipeDirection.Left)) || (it > 0f && handles(SwipeDirection.Right))
            } ?: 0f
            val y = available.y.takeIf {
                (it < 0f && handles(SwipeDirection.Up)) || (it > 0f && handles(SwipeDirection.Down))
            } ?: 0f
            return Offset(x, y)
        }
    }
}

/**
 * Feeds one pointer gesture at a time into [tracker]. Each event is seen twice: in the main pass, after the content
 * (which consumes what it scrolls), for the movement left over; in the final pass, after outer nodes, to notice one of
 * them taking the drag. Once the movement is a handled swipe it is consumed, so a tap or long press doesn't fire too.
 */
internal suspend fun PointerInputScope.trackSwipes(tracker: SwipeTracker) {
    awaitEachGesture {
        awaitFirstDown(requireUnconsumed = false)
        tracker.start()
        while (true) {
            val event = awaitPointerEvent(PointerEventPass.Main)
            val used = event.changes.map { it.isConsumed }
            event.changes.filter { !it.isConsumed }.forEach { tracker.move(it.positionChange()) }
            val lifted = event.changes.none { it.pressed }
            if (lifted) {
                if (tracker.end()) event.changes.forEach { it.consume() }
                break
            }
            val swiping = tracker.recognized() != null
            if (swiping) event.changes.forEach { it.consume() }
            val final = awaitPointerEvent(PointerEventPass.Final)
            if (!swiping && final.changes.zip(used).any { (c, wasUsed) -> c.isConsumed && !wasUsed }) tracker.steal()
        }
    }
}

/** How far a drag must go to be a swipe. */
internal val SWIPE_DISTANCE = 48.dp

/** A swipe goes at least this many times further along its axis than across it. */
internal const val SWIPE_AXIS_RATIO = 1.5f
