package org.theconstruct

import androidx.compose.foundation.ScrollState
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.offset
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.ProvidableCompositionLocal
import androidx.compose.runtime.Stable
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.layout.LayoutCoordinates
import androidx.compose.ui.layout.onPlaced
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.zIndex
import kotlinx.serialization.json.JsonObject
import kotlin.math.roundToInt

/**
 * The scrolling view a `core://Sticky` (schema 11) pins to: its [scroll] state and the coordinates of the scrolled
 * content ([stickyContent]). Pinned content stops [topInset] below the top of the view, e.g. clear of the menu and bell
 * buttons [ConstructApp] draws over the workspace.
 */
@Stable
class StickyScroll(val scroll: ScrollState, val topInset: Dp = 0.dp) {
    internal var content: LayoutCoordinates? = null
}

/** The scrolling view `core://Sticky` pins to; null outside one (the node then just draws its content). */
val LocalStickyScroll: ProvidableCompositionLocal<StickyScroll?> = staticCompositionLocalOf { null }

/** Marks the scrolled content of [sticky]'s view: goes right after `verticalScroll(sticky.scroll)`. */
fun Modifier.stickyContent(sticky: StickyScroll): Modifier = onPlaced { sticky.content = it }

/**
 * How far down to move a sticky node, in px: enough to keep its top [inset] below the top of the view while it
 * would scroll above that line, but never past the bottom of its parent (it scrolls away with the parent's end, like
 * CSS `position: sticky`). [top] and [parentBottom] are in the scrolled content's coordinates (they do not change as
 * it scrolls); [scroll] is how far the view is scrolled.
 */
fun stickyShift(scroll: Int, inset: Int, top: Int, height: Int, parentBottom: Int): Int =
    (scroll + inset - top).coerceIn(0, (parentBottom - top - height).coerceAtLeast(0))

/** Where a sticky node sits in the scrolled content, measured as it is placed. */
private class StickyPlace {
    var top = 0
    var height = 0
    var parentBottom = 0

    fun shift(scroll: Int, inset: Int) = stickyShift(scroll, inset, top, height, parentBottom)
}

/**
 * `core://Sticky`: its content, moved down while the view scrolls past it ([stickyShift]) and drawn above its
 * siblings on the page background. Pinned, the background also fills the inset above it (behind the menu buttons) and
 * a hairline marks its bottom edge. Moving it in placement (not in a layer) keeps it in step with the scroll and keeps
 * taps on the pinned content. Tag: `sticky`.
 */
@Composable
internal fun NodeSticky(
    node: UiNode.Sticky,
    entities: Map<String, JsonObject>,
    onDispatch: (ActionIntent) -> Unit,
    item: JsonObject?,
    modifier: Modifier,
) {
    val sticky = LocalStickyScroll.current
    if (sticky == null) {
        RenderUiNode(node.content, entities, onDispatch, modifier, item)
        return
    }
    val place = remember { StickyPlace() }
    val inset = with(LocalDensity.current) { sticky.topInset.roundToPx() }
    val hairline = with(LocalDensity.current) { 1.dp.toPx() }
    val background = MaterialTheme.colorScheme.background
    val line = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.5f)
    Box(
        modifier
            .zIndex(1f)
            .fillMaxWidth()
            .onPlaced { me ->
                val content = sticky.content ?: return@onPlaced
                place.top = content.localPositionOf(me, Offset.Zero).y.roundToInt()
                place.height = me.size.height
                place.parentBottom = me.parentLayoutCoordinates
                    ?.let { content.localBoundingBoxOf(it, clipBounds = false).bottom.roundToInt() }
                    ?: (place.top + place.height)
            }
            .offset { IntOffset(0, place.shift(sticky.scroll.value, inset)) }
            .drawBehind {
                val scroll = sticky.scroll.value
                val shift = place.shift(scroll, inset)
                if (shift <= 0) return@drawBehind
                // The band from the top of the view down to the pinned content (at most the inset).
                val above = (place.top + shift - scroll).coerceIn(0, inset).toFloat()
                drawRect(background, topLeft = Offset(0f, -above), size = Size(size.width, size.height + above))
                drawRect(line, topLeft = Offset(0f, size.height - hairline), size = Size(size.width, hairline))
            }
            .testTag("sticky"),
    ) {
        RenderUiNode(node.content, entities, onDispatch, Modifier, item)
    }
}
