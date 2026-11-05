package org.theconstruct

import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.layout.IntrinsicMeasurable
import androidx.compose.ui.layout.IntrinsicMeasureScope
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.layout.Measurable
import androidx.compose.ui.layout.MeasurePolicy
import androidx.compose.ui.layout.MeasureResult
import androidx.compose.ui.layout.MeasureScope
import androidx.compose.ui.layout.Placeable
import androidx.compose.ui.layout.layoutId
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import kotlinx.serialization.json.JsonObject
import kotlin.math.roundToInt
import kotlin.math.sign

/**
 * How [NodeRow] sizes a child of `core://Row` (issue #117).
 *
 * - [Fixed]: controls and anything else (Switch, Chip, Button, IconButton, sized Image, Box, sized Spacer) keep their
 *   natural width; they are measured first.
 * - [Flexible]: text-like children (Text, Container, Column, or a Conditional or Tappable around one) take what the
 *   fixed children leave, so long text wraps instead of pushing the controls after it off screen.
 * - [Fill]: a Spacer without a size shares out the width left over at the end.
 */
internal enum class RowPart { Fixed, Flexible, Fill }

internal fun rowPart(node: UiNode): RowPart = when (node) {
    is UiNode.Spacer -> if (node.sizeDp == null) RowPart.Fill else RowPart.Fixed
    is UiNode.Text, is UiNode.Container, is UiNode.Column -> RowPart.Flexible
    is UiNode.Conditional -> if (rowPart(node.content) == RowPart.Flexible) RowPart.Flexible else RowPart.Fixed
    is UiNode.Tappable -> if (rowPart(node.content) == RowPart.Flexible) RowPart.Flexible else RowPart.Fixed
    else -> RowPart.Fixed
}

/**
 * `core://Row`: children side by side, [UiNode.Row.spacingDp] apart, aligned by [UiNode.Row.align]. When everything
 * fits, each child gets the width a Compose `Row` would give it (unsized Spacers share the rest as `weight(1f)`
 * would). When it doesn't, the text-like children ([RowPart.Flexible]) shrink and wrap while the controls stay whole.
 */
@Composable
internal fun NodeRow(
    node: UiNode.Row,
    entities: Map<String, JsonObject>,
    onDispatch: (ActionIntent) -> Unit,
    item: JsonObject?,
    modifier: Modifier,
) {
    val spacing = node.spacingDp.dp
    val align = when (node.align) {
        Align.Start -> Alignment.Top
        Align.End -> Alignment.Bottom
        Align.Center, null -> Alignment.CenterVertically
    }
    val policy = remember(spacing, align) { FlexRowMeasurePolicy(spacing, align) }
    Layout(
        content = {
            node.children.forEach { child ->
                when (val part = rowPart(child)) {
                    RowPart.Fill -> Spacer(Modifier.layoutId(part))
                    RowPart.Flexible -> RenderUiNode(child, entities, onDispatch, Modifier.layoutId(part), item)
                    RowPart.Fixed -> RenderUiNode(child, entities, onDispatch, Modifier, item)
                }
            }
        },
        modifier = modifier.padding(horizontal = node.paddingHDp.dp, vertical = node.paddingVDp.dp),
        measurePolicy = policy,
    )
}

/**
 * Widths for a row's flexible children whose natural (one-line) widths are [natural], sharing [available]: each
 * gets its natural width when they all fit; otherwise the narrow ones still get theirs and the rest split what is
 * left evenly, so a short value beside a long title stays readable.
 */
internal fun shareRowWidth(natural: List<Int>, available: Int): List<Int> {
    val out = IntArray(natural.size)
    var left = available.coerceAtLeast(0)
    val order = natural.indices.sortedBy { natural[it] }
    order.forEachIndexed { k, i ->
        val share = left / (order.size - k)
        out[i] = minOf(natural[i].coerceAtLeast(0), share)
        left -= out[i]
    }
    return out.toList()
}

/**
 * Splits [leftover] among [count] unsized Spacers the way a Compose `Row` splits it among `weight(1f)` children
 * (each the rounded share, the rounding difference spread one pixel at a time from the first), so rows look the
 * same as before to the pixel.
 */
internal fun shareFill(leftover: Int, count: Int): List<Int> {
    if (count <= 0) return emptyList()
    val unit = leftover.toFloat() / count
    val each = unit.roundToInt()
    var remainder = leftover - each * count
    return List(count) {
        val extra = remainder.sign
        remainder -= extra
        (each + extra).coerceAtLeast(0)
    }
}

private class FlexRowMeasurePolicy(private val spacing: Dp, private val align: Alignment.Vertical) : MeasurePolicy {
    override fun MeasureScope.measure(measurables: List<Measurable>, constraints: Constraints): MeasureResult {
        val gap = spacing.roundToPx()
        val count = measurables.size
        val parts = measurables.map { it.layoutId as? RowPart ?: RowPart.Fixed }
        val bounded = constraints.hasBoundedWidth
        val placeables = arrayOfNulls<Placeable>(count)
        var used = gap * (count - 1).coerceAtLeast(0)
        fun remaining(): Int = if (bounded) (constraints.maxWidth - used).coerceAtLeast(0) else Constraints.Infinity
        fun measureChild(i: Int, maxWidth: Int) {
            val p = measurables[i].measure(Constraints(maxWidth = maxWidth, maxHeight = constraints.maxHeight))
            placeables[i] = p
            used += p.width
        }

        // 1. Controls first, at their natural width.
        for (i in 0 until count) if (parts[i] == RowPart.Fixed) measureChild(i, remaining())

        // 2. Text-like children share what the controls leave.
        val flexible = (0 until count).filter { parts[it] == RowPart.Flexible }
        val naturals = if (bounded && flexible.size > 1) {
            // A child that can't answer intrinsics (lazy or subcomposed content) falls back to measuring in order.
            runCatching { flexible.map { measurables[it].maxIntrinsicWidth(constraints.maxHeight) } }.getOrNull()
        } else {
            null
        }
        if (naturals != null) {
            val widths = shareRowWidth(naturals, remaining())
            flexible.forEachIndexed { k, i -> measureChild(i, widths[k]) }
        } else {
            flexible.forEach { i -> measureChild(i, remaining()) }
        }

        // 3. Unsized Spacers split whatever is left.
        val fills = (0 until count).filter { parts[it] == RowPart.Fill }
        val fillWidths = shareFill(if (bounded) remaining() else 0, fills.size)
        fills.forEachIndexed { k, i ->
            val w = fillWidths[k]
            placeables[i] = measurables[i].measure(Constraints(minWidth = w, maxWidth = w, maxHeight = constraints.maxHeight))
        }

        val placed = placeables.map { it!! }
        val contentWidth = placed.sumOf { it.width } + gap * (count - 1).coerceAtLeast(0)
        val width = if (fills.isNotEmpty() && bounded) {
            constraints.maxWidth
        } else {
            contentWidth.coerceIn(constraints.minWidth, constraints.maxWidth)
        }
        val height = (placed.maxOfOrNull { it.height } ?: 0).coerceIn(constraints.minHeight, constraints.maxHeight)
        return layout(width, height) {
            var x = 0
            placed.forEach { p ->
                p.placeRelative(x, align.align(p.height, height))
                x += p.width + gap
            }
        }
    }

    override fun IntrinsicMeasureScope.maxIntrinsicWidth(measurables: List<IntrinsicMeasurable>, height: Int): Int =
        measurables.sumOf { it.maxIntrinsicWidth(height) } + gaps(measurables.size)

    override fun IntrinsicMeasureScope.minIntrinsicWidth(measurables: List<IntrinsicMeasurable>, height: Int): Int =
        measurables.sumOf { it.minIntrinsicWidth(height) } + gaps(measurables.size)

    override fun IntrinsicMeasureScope.maxIntrinsicHeight(measurables: List<IntrinsicMeasurable>, width: Int): Int =
        measurables.maxOfOrNull { it.maxIntrinsicHeight(width) } ?: 0

    override fun IntrinsicMeasureScope.minIntrinsicHeight(measurables: List<IntrinsicMeasurable>, width: Int): Int =
        measurables.maxOfOrNull { it.minIntrinsicHeight(width) } ?: 0

    private fun IntrinsicMeasureScope.gaps(count: Int): Int = spacing.roundToPx() * (count - 1).coerceAtLeast(0)
}
