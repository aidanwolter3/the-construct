package org.theconstruct

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.LocalContentColor
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.selected
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
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

/**
 * `core://Box` (schema 6, D44): the content on a rounded surface. Colours are bound ([interpolate]) then parsed as
 * `#RRGGBB`; one that does not parse is not drawn. [UiNode.Box.contentColor] becomes the content colour of the text
 * and icons inside.
 */
@Composable
internal fun NodeBox(
    node: UiNode.Box,
    entities: Map<String, JsonObject>,
    onDispatch: (ActionIntent) -> Unit,
    item: JsonObject?,
    modifier: Modifier,
) {
    fun color(expr: String?) = expr?.let { parseHexColor(interpolate(it, entities, item).trim()) }
    val bg = color(node.background)
    val fg = color(node.contentColor)
    val border = color(node.borderColor)
    val shape = RoundedCornerShape(node.cornerDp.coerceAtLeast(0).dp)
    var mod = modifier
    node.widthDp?.takeIf { it > 0 }?.let { mod = mod.width(it.dp) }
    node.heightDp?.takeIf { it > 0 }?.let { mod = mod.height(it.dp) }
    mod = mod.clip(shape)
    if (bg != null) mod = mod.background(bg, shape)
    if (border != null) mod = mod.border(1.dp, border, shape)
    mod = mod.padding(horizontal = node.paddingHDp.dp, vertical = node.paddingVDp.dp)
    val alignment = when (node.align) {
        Align.Start -> Alignment.CenterStart
        Align.Center -> Alignment.Center
        Align.End -> Alignment.CenterEnd
    }
    Box(mod, contentAlignment = alignment) {
        if (fg != null) {
            CompositionLocalProvider(LocalContentColor provides fg) {
                RenderUiNode(node.content, entities, onDispatch, Modifier, item)
            }
        } else {
            RenderUiNode(node.content, entities, onDispatch, Modifier, item)
        }
    }
}

private val ChoiceSelectedBg = Color(0xFF1B1B1F)
private val ChoiceBg = Color(0xFFF2F2F5)
private val ChoiceText = Color(0xFF3A3A3C)

/**
 * `core://ChoiceChips` (schema 6, D44): a sideways-scrolling row of chips editing the form value
 * [UiNode.ChoiceChips.field]. Tapping one stores its value (an empty value clears the field). Tags:
 * `choice:<field>:<value>`.
 */
@Composable
internal fun NodeChoiceChips(node: UiNode.ChoiceChips, modifier: Modifier) {
    val form = LocalForm.current
    val current = form?.get(node.field).orEmpty()
    Row(
        modifier
            .fillMaxWidth()
            .horizontalScroll(rememberScrollState())
            .padding(horizontal = node.paddingHDp.coerceAtLeast(0).dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        node.options.forEach { option ->
            val isSelected = option.value == current
            Text(
                option.label,
                style = MaterialTheme.typography.labelLarge,
                color = if (isSelected) Color.White else ChoiceText,
                modifier = Modifier
                    .clip(RoundedCornerShape(50))
                    .background(if (isSelected) ChoiceSelectedBg else ChoiceBg)
                    .clickable {
                        if (option.value.isEmpty()) form?.remove(node.field) else form?.set(node.field, option.value)
                    }
                    .semantics {
                        selected = isSelected
                        role = Role.Tab
                    }
                    .testTag("choice:${node.field}:${option.value}")
                    .padding(horizontal = 14.dp, vertical = 8.dp),
            )
        }
    }
}

private val PickerTileBg = Color(0xFFF2F2F5)
private val PickerTileBorder = Color(0xFFD8D8DE)
private val PickerTileSize = 80.dp

/**
 * `core://MediaPicker` (schema 8, D47/D49): the picked photos as square thumbnails (from each attachment's local
 * entity) with a remove button, and an add tile while fewer than `max_items` are picked. The form value is a JSON
 * array of staged attachment ids ([parseFormList]). Picking and staging run off the UI thread; a staging failure
 * shows its reason under the tiles. Tags: `media-picker:<field>`, `media-picker-add:<field>`,
 * `media-picker-remove:<id>`, `media-picker-error:<field>`.
 */
@Composable
internal fun NodeMediaPicker(node: UiNode.MediaPicker, entities: Map<String, JsonObject>, modifier: Modifier) {
    val form = LocalForm.current
    val attachments = LocalAttachments.current
    val platform = LocalMediaPickerPlatform.current
    val ids = parseFormList(form?.get(node.field)).orEmpty()
    val max = node.maxItems.coerceAtLeast(0)
    var error by remember(node.field) { mutableStateOf<String?>(null) }
    var busy by remember(node.field) { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    fun current(): List<String> = parseFormList(form?.get(node.field)).orEmpty()
    fun store(list: List<String>) {
        if (list.isEmpty()) form?.remove(node.field) else form?.set(node.field, encodeFormList(list))
    }
    val pick: () -> Unit = pick@{
        error = null
        if (platform == null || attachments == null || form == null) {
            error = "Picking photos isn't available here."
            return@pick
        }
        busy = true
        scope.launch {
            MediaCalls.add(1)
            try {
                val paths = platform.pick((max - current().size).coerceAtLeast(1))
                for (path in paths) {
                    if (current().size >= max) break
                    // Not cancellable: a staged file must be discarded, not lost, if the screen closes meanwhile.
                    val result = withContext(NonCancellable) { attachments.stage(path) }
                    val info = result.getOrNull()
                    if (info == null) {
                        error = result.exceptionOrNull()?.message ?: "Couldn't add that photo."
                        continue
                    }
                    if (!isActive) {
                        attachments.discard(info.id)
                        break
                    }
                    store(current() + info.id)
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                error = e.message ?: "Couldn't open the photo picker."
            } finally {
                busy = false
                MediaCalls.add(-1)
            }
        }
    }
    Column(modifier.fillMaxWidth().testTag("media-picker:${node.field}")) {
        FlowRow(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            ids.forEachIndexed { index, id ->
                // Keyed by attachment id: removing one keeps the others' loaded images.
                key(id) {
                    Box(Modifier.size(PickerTileSize).clip(RoundedCornerShape(8.dp))) {
                        ConstructImage(
                            url = attachmentUrl(id, entities, attachments),
                            contentDescription = pageContentDescription(null, index, ids.size),
                            modifier = Modifier.fillMaxSize(),
                        )
                        Box(
                            Modifier
                                .align(Alignment.TopEnd)
                                .padding(4.dp)
                                .size(24.dp)
                                .clip(CircleShape)
                                .background(Color(0x99000000))
                                .clickable(onClickLabel = "Remove photo") {
                                    attachments?.discard(id)
                                    store(current() - id)
                                }
                                .semantics { contentDescription = "Remove photo ${index + 1}" }
                                .testTag("media-picker-remove:$id"),
                            contentAlignment = Alignment.Center,
                        ) {
                            Icon(ConstructIcons.Close, contentDescription = null, tint = Color.White, modifier = Modifier.size(16.dp))
                        }
                    }
                }
            }
            if (ids.size < max) {
                val label = node.label?.takeIf { it.isNotBlank() } ?: "Add photo"
                Column(
                    Modifier
                        .size(PickerTileSize)
                        .clip(RoundedCornerShape(8.dp))
                        .background(PickerTileBg)
                        .border(1.dp, PickerTileBorder, RoundedCornerShape(8.dp))
                        .clickable(enabled = !busy, onClickLabel = label, onClick = pick)
                        .semantics { role = Role.Button }
                        .testTag("media-picker-add:${node.field}")
                        .padding(6.dp),
                    verticalArrangement = Arrangement.Center,
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    if (busy) {
                        CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 2.dp)
                    } else {
                        Icon(ConstructIcons.AddPhoto, contentDescription = null, tint = MutedText, modifier = Modifier.size(28.dp))
                    }
                    Text(
                        label,
                        style = MaterialTheme.typography.labelSmall,
                        color = MutedText,
                        textAlign = TextAlign.Center,
                        maxLines = 2,
                        modifier = Modifier.padding(top = 4.dp),
                    )
                }
            }
        }
        error?.let {
            Text(
                it,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(top = 6.dp).testTag("media-picker-error:${node.field}"),
            )
        }
    }
}
