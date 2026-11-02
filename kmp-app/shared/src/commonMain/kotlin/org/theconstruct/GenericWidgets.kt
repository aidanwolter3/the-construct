package org.theconstruct

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.ProvidableCompositionLocal
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

// Generic widgets of schema 3 (DECISIONS D36): no app knowledge, everything comes from the node and the bound item.

internal val MutedText = Color(0xFF737373)
private val DotActive = Color(0xFF0095F6)
private val DotInactive = Color(0xFFC7C7C7)

/** True inside a carousel page: an image without a size fills it. */
internal val LocalFillParent: ProvidableCompositionLocal<Boolean> = staticCompositionLocalOf { false }

/** `#RRGGBB` -> a colour, else null. */
internal fun parseHexColor(hex: String?): Color? {
    val h = hex?.removePrefix("#") ?: return null
    if (h.length != 6) return null
    val v = h.toLongOrNull(16) ?: return null
    return Color(0xFF000000 or v)
}

/** The entities of a list binding, each with its synthetic `uri` (URIs without an entity are skipped). */
internal fun boundItems(binding: ListBinding, item: JsonObject?, entities: Map<String, JsonObject>): List<JsonObject> =
    resolveList(binding, item).mapNotNull { uri -> entities[uri]?.let { JsonObject(it + ("uri" to JsonPrimitive(uri))) } }

/**
 * Accessibility label of carousel page [index] (0-based) of [count] when it has no description: "Photo n of N"
 * ("Photo" alone for one).
 */
internal fun pageContentDescription(description: String?, index: Int, count: Int): String =
    description?.takeIf { it.isNotBlank() } ?: if (count > 1) "Photo ${index + 1} of $count" else "Photo"

/**
 * `core://Carousel`: a [HorizontalPager] over the bound entities (test tag = the bound `tag`) with an "n/N" badge and
 * page dots (content description "Page n of N", tag `dots:<tag>`) when there is more than one. Nothing for no items.
 */
@Composable
internal fun NodeCarousel(
    node: UiNode.Carousel,
    entities: Map<String, JsonObject>,
    onDispatch: (ActionIntent) -> Unit,
    item: JsonObject?,
    modifier: Modifier,
) {
    val items = boundItems(node.items, item, entities)
    if (items.isEmpty()) return
    val ratio = carouselAspectRatio(items.first(), node.aspectField, node.minAspect, node.maxAspect, node.aspectRatio)
    val tag = node.tag?.let { bindItemTokens(it, item) }
    val pagerState = rememberPagerState { items.size }
    val page = @Composable { index: Int ->
        Box(Modifier.fillMaxSize()) {
            CompositionLocalProvider(LocalFillParent provides true, LocalPageInfo provides (index to items.size)) {
                RenderUiNode(node.itemTemplate, entities, onDispatch, Modifier.fillMaxSize(), items[index])
            }
        }
    }
    Column(modifier.fillMaxWidth()) {
        Box(Modifier.fillMaxWidth().aspectRatio(ratio)) {
            if (items.size == 1) {
                page(0)
            } else {
                HorizontalPager(
                    state = pagerState,
                    modifier = Modifier.fillMaxSize().let { m -> tag?.let { m.testTag(it) } ?: m },
                ) { index -> page(index) }
                Text(
                    "${pagerState.currentPage + 1}/${items.size}",
                    color = Color.White,
                    style = MaterialTheme.typography.labelMedium,
                    modifier = Modifier
                        .align(Alignment.TopEnd)
                        .padding(12.dp)
                        .background(Color(0x99000000), RoundedCornerShape(12.dp))
                        .padding(horizontal = 9.dp, vertical = 3.dp),
                )
            }
        }
        if (items.size > 1) {
            // Below the image, like Instagram: overlay dots disappear on light images.
            Row(
                Modifier
                    .align(Alignment.CenterHorizontally)
                    .padding(top = 10.dp)
                    .let { m -> tag?.let { m.testTag("dots:$it") } ?: m }
                    .semantics { contentDescription = "Page ${pagerState.currentPage + 1} of ${items.size}" },
                horizontalArrangement = Arrangement.spacedBy(5.dp),
            ) {
                repeat(items.size) { i ->
                    val active = i == pagerState.currentPage
                    Box(Modifier.size(6.dp).background(if (active) DotActive else DotInactive, CircleShape))
                }
            }
        }
    }
}

/** (page index, page count) inside a carousel page, for image descriptions; null elsewhere. */
internal val LocalPageInfo: ProvidableCompositionLocal<Pair<Int, Int>?> = staticCompositionLocalOf { null }

/**
 * `core://Image`: [UiNode.Image.sizeDp] makes a square; without it, the image fills a carousel page or the width at the
 * aspect ratio. The bound blurhash colours the placeholder; on error the description (or "Image unavailable") shows
 * over a broken-image icon (sized images just keep the placeholder).
 */
@Composable
internal fun NodeImage(node: UiNode.Image, entities: Map<String, JsonObject>, item: JsonObject?, modifier: Modifier) {
    val url = resolveRef(node.url, item)?.takeIf { it.isNotBlank() && !it.contains("\$item.") }
    val description = node.contentDescription?.let { interpolate(it, entities, item) }?.takeIf { it.isNotBlank() }
    val page = LocalPageInfo.current
    val a11y = if (page != null) pageContentDescription(description, page.first, page.second) else description
    val size = node.sizeDp?.takeIf { it > 0 }
    val sized = when {
        size != null -> Modifier.size(size.dp)
        LocalFillParent.current && node.aspectRatio == null -> Modifier.fillMaxSize()
        else -> Modifier.fillMaxWidth().aspectRatio(node.aspectRatio?.takeIf { it.isFinite() && it > 0f } ?: 1f)
    }
    val shaped = when (node.shape) {
        ImageShape.Circle -> Modifier.clip(CircleShape)
        ImageShape.Rounded -> Modifier.clip(RoundedCornerShape(8.dp))
        ImageShape.Square -> Modifier
    }
    val hash = node.blurhash?.let { resolveRef(it, item) }
    val bg = blurhashAverageRgb(hash)?.let { Color(0xFF000000 or it.toLong()) } ?: Color(0xFFE8E8EC)
    ConstructImage(
        url = url,
        contentDescription = a11y,
        modifier = modifier.then(sized).then(shaped),
        placeholderColor = bg,
        error = {
            if (size == null) {
                Column(Modifier.align(Alignment.Center).padding(16.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                    Icon(ConstructIcons.BrokenImage, contentDescription = null, tint = Color(0xFF9A9AA0), modifier = Modifier.size(40.dp))
                    Text(
                        description ?: "Image unavailable",
                        textAlign = TextAlign.Center,
                        color = Color(0xFF6E6E73),
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
            }
        },
    )
}

/** `core://IconButton`: the bound `active` flag picks the active icon, tint and description. Tag = the bound `tag`. */
@Composable
internal fun NodeIconButton(node: UiNode.IconButton, item: JsonObject?, dispatch: (ActionIntent) -> Unit, modifier: Modifier) {
    val active = node.active?.let { isTruthy(it, item) } == true
    val name = if (active) node.activeIcon ?: node.icon else node.icon
    val description = if (active) node.activeContentDescription ?: node.contentDescription else node.contentDescription
    val tint = (if (active) parseHexColor(node.activeTint) else null) ?: MaterialTheme.colorScheme.onSurface
    val tag = node.tag?.let { bindItemTokens(it, item) }
    IconButton(
        onClick = { dispatch(node.onClick) },
        modifier = modifier.let { m -> tag?.let { m.testTag(it) } ?: m },
    ) {
        val icon = ConstructIcons.named(name)
        if (icon != null) {
            Icon(icon, contentDescription = description, tint = tint, modifier = Modifier.size((node.sizeDp ?: 24).dp))
        } else {
            Text("?", modifier = Modifier.semantics { contentDescription = description })
        }
    }
}

/** `core://Chip`: a rounded label, warning-coloured for [Tone.Warning]. */
@Composable
internal fun NodeChip(node: UiNode.Chip, entities: Map<String, JsonObject>, item: JsonObject?, modifier: Modifier) {
    val (bg, fg, iconTint) = when (node.tone) {
        Tone.Warning -> Triple(Color(0xFFFFF4E0), Color(0xFF7A4A00), Color(0xFFB26A00))
        Tone.Neutral -> Triple(Color(0xFFF2F2F5), Color(0xFF3A3A3C), MutedText)
    }
    Row(
        modifier.background(bg, RoundedCornerShape(8.dp)).padding(horizontal = 10.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        node.icon?.let(ConstructIcons::named)?.let {
            Icon(it, contentDescription = null, tint = iconTint, modifier = Modifier.size(16.dp))
            Spacer(Modifier.width(6.dp))
        }
        Text(interpolate(node.text, entities, item), style = MaterialTheme.typography.labelLarge, color = fg)
    }
}

/** `core://Badge`: a small muted icon and text line ("Bob boosted"). */
@Composable
internal fun NodeBadge(node: UiNode.Badge, entities: Map<String, JsonObject>, item: JsonObject?, modifier: Modifier) {
    Row(modifier.padding(start = 16.dp, top = 10.dp), verticalAlignment = Alignment.CenterVertically) {
        ConstructIcons.named(node.icon)?.let {
            Icon(it, contentDescription = null, tint = MutedText, modifier = Modifier.size(14.dp))
            Spacer(Modifier.width(6.dp))
        }
        Text(interpolate(node.text, entities, item), style = MaterialTheme.typography.labelMedium, color = MutedText)
    }
}
