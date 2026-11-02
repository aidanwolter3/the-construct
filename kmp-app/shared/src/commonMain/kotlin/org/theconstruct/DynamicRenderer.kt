package org.theconstruct

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.draw.clip
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.IconButton
import androidx.compose.material3.Icon
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.Row
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.ProvidableCompositionLocal
import androidx.compose.runtime.key
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/** Maximum width of a list item on wide screens (a readable column, like Instagram/Pixelfed web). */
val FEED_MAX_WIDTH = 560.dp

/** The workspace's queries (`query_key` -> ordered entity URIs), read by `core://List`. Provided by [ConstructApp]. */
val LocalConstructQueries: ProvidableCompositionLocal<Map<String, List<String>>> = staticCompositionLocalOf { emptyMap() }

/**
 * Extra start padding for the workspace title, so it sits beside the menu button [ConstructApp] draws over the top-left
 * corner instead of under it. 0 outside [ConstructApp].
 */
val LocalWorkspaceTitleInset: ProvidableCompositionLocal<Dp> = staticCompositionLocalOf { 0.dp }

/**
 * Renders one [UiNode]. [item] is the current item context inside a list or carousel template (the entity's
 * fields plus a synthetic `uri` field holding its entity key). Intents
 * declared in the tree are bound to [item] via [bindIntent] before reaching [onDispatch].
 * Unknown node types render a small placeholder and never crash.
 */
@Composable
fun RenderUiNode(
    node: UiNode,
    entities: Map<String, JsonObject>,
    onDispatch: (ActionIntent) -> Unit,
    modifier: Modifier = Modifier,
    item: JsonObject? = null,
) {
    val dispatch: (ActionIntent) -> Unit = { onDispatch(bindIntent(it, item)) }
    when (node) {
        is UiNode.Workspace -> Column(modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
            Text(
                node.title,
                style = MaterialTheme.typography.headlineMedium,
                modifier = Modifier.padding(start = 16.dp + LocalWorkspaceTitleInset.current, end = 16.dp, top = 12.dp, bottom = 12.dp),
            )
            node.slots.forEach { slot ->
                // Keyed by slot id: a layout change that inserts a slot above (e.g. a new version adding a banner at
                // the top) keeps the state of the slots below (feed pager positions).
                key(slot.slotId) {
                    RenderUiNode(slot.content, entities, onDispatch, Modifier, item)
                }
            }
            Spacer(Modifier.height(16.dp))
        }

        is UiNode.Container -> {
            val mod = modifier.padding(node.paddingDp.dp)
            when (node.orientation) {
                Orientation.Vertical -> Column(mod, verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    node.children.forEach { RenderUiNode(it, entities, onDispatch, Modifier, item) }
                }
                // FlowRow, not Row: children keep their natural width and wrap onto the next line
                // when they do not fit (a Row squeezes the last child, e.g. a button label wrapping
                // one letter per line on a phone).
                Orientation.Horizontal -> FlowRow(
                    mod,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                    itemVerticalAlignment = Alignment.CenterVertically,
                ) {
                    node.children.forEach { RenderUiNode(it, entities, onDispatch, Modifier, item) }
                }
            }
        }

        is UiNode.Text -> Text(
            text = interpolate(node.valueExpr, entities, item),
            style = when (node.style) {
                TextStyle.Headline -> MaterialTheme.typography.headlineMedium
                TextStyle.Title -> MaterialTheme.typography.titleLarge
                TextStyle.Body -> MaterialTheme.typography.bodyMedium
                TextStyle.Caption -> MaterialTheme.typography.labelSmall
                TextStyle.Label -> MaterialTheme.typography.titleSmall.copy(fontWeight = FontWeight.SemiBold)
                TextStyle.Secondary -> MaterialTheme.typography.bodySmall
            },
            color = if (node.style == TextStyle.Secondary) MutedText else Color.Unspecified,
            modifier = modifier,
        )

        is UiNode.Button -> Button(onClick = { dispatch(node.onClick) }, modifier = modifier) {
            Text(interpolate(node.label, entities, item))
        }

        is UiNode.ItemList -> ItemList(node, entities, onDispatch, modifier)

        is UiNode.Image -> NodeImage(node, entities, item, modifier)

        is UiNode.TextField -> FormTextField(node, dispatch, modifier)

        is UiNode.Row -> Row(
            modifier.padding(horizontal = node.paddingHDp.dp, vertical = node.paddingVDp.dp),
            horizontalArrangement = Arrangement.spacedBy(node.spacingDp.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            node.children.forEach { child ->
                if (child is UiNode.Spacer && child.sizeDp == null) {
                    Spacer(Modifier.weight(1f))
                } else {
                    RenderUiNode(child, entities, onDispatch, Modifier, item)
                }
            }
        }

        is UiNode.Column -> Column(
            modifier.fillMaxWidth().padding(horizontal = node.paddingHDp.dp, vertical = node.paddingVDp.dp),
            verticalArrangement = Arrangement.spacedBy(node.spacingDp.dp),
        ) {
            node.children.forEach { RenderUiNode(it, entities, onDispatch, Modifier, item) }
        }

        is UiNode.Spacer -> Spacer(modifier.size((node.sizeDp ?: 0).dp))

        UiNode.Divider -> HorizontalDivider(modifier, color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.5f))

        is UiNode.Conditional -> if (isTruthy(node.value, item)) {
            RenderUiNode(node.content, entities, onDispatch, modifier, item)
        }

        is UiNode.Carousel -> NodeCarousel(node, entities, onDispatch, item, modifier)

        is UiNode.IconButton -> NodeIconButton(node, item, dispatch, modifier)

        is UiNode.Chip -> NodeChip(node, entities, item, modifier)

        is UiNode.Badge -> NodeBadge(node, entities, item, modifier)

        is UiNode.Unknown -> Text(
            "Unsupported component: ${node.type.ifEmpty { "<missing type>" }}",
            style = MaterialTheme.typography.labelSmall,
            color = Color.Gray,
            modifier = modifier.padding(4.dp),
        )
    }
}

// ---------------------------------------------------------------------------
// Generic nodes (DECISIONS D34): core://List, core://Image, core://TextField.
// ---------------------------------------------------------------------------

/**
 * The items of `core://List`: `queries[queryKey]` in order (de-duplicated, URIs without an entity skipped), each the
 * entity plus a synthetic `uri`; null while the query is absent (not loaded yet).
 */
fun listItems(queryKey: String, queries: Map<String, List<String>>, entities: Map<String, JsonObject>): List<JsonObject>? =
    queries[queryKey]?.distinct()?.mapNotNull { uri -> entities[uri]?.let { JsonObject(it + ("uri" to JsonPrimitive(uri))) } }

/** Start padding of a list item: `item[indent.field]` (a number) times `stepDp`, at most `max` (default 4) steps. */
fun indentDp(indent: Indent?, item: JsonObject): Int {
    if (indent == null) return 0
    val n = (item[indent.field] as? JsonPrimitive)?.takeIf { !it.isString }?.content?.toDoubleOrNull()?.toInt() ?: 0
    return n.coerceIn(0, indent.max ?: 4) * indent.stepDp.coerceAtLeast(0)
}

@Composable
private fun ItemList(
    node: UiNode.ItemList,
    entities: Map<String, JsonObject>,
    onDispatch: (ActionIntent) -> Unit,
    modifier: Modifier,
) {
    val queries = LocalConstructQueries.current
    val items = remember(node.queryKey, queries, entities) { listItems(node.queryKey, queries, entities) }
    Column(modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally) {
        if (items != null && items.isEmpty() && node.emptyText != null) {
            Text(
                node.emptyText,
                style = MaterialTheme.typography.bodyMedium,
                color = Color.Gray,
                modifier = Modifier.padding(16.dp).testTag("list-empty"),
            )
        }
        items?.forEach { item ->
            // Keyed by entity URI, so per-item state follows the item when the list changes.
            key(item["uri"]) {
                // Phone: full width. Wider screens: a centered, readable column.
                RenderUiNode(
                    node.itemTemplate, entities, onDispatch,
                    Modifier.widthIn(max = FEED_MAX_WIDTH).fillMaxWidth().padding(start = indentDp(node.indent, item).dp),
                    item,
                )
            }
        }
    }
}

/** A text field bound to the form value [UiNode.TextField.field]; Send dispatches `on_submit` and clears it. */
@Composable
private fun FormTextField(node: UiNode.TextField, dispatch: (ActionIntent) -> Unit, modifier: Modifier) {
    val form = LocalForm.current
    val value = form?.get(node.field).orEmpty()
    val submit = node.onSubmit
    Row(modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        OutlinedTextField(
            value = value,
            onValueChange = { v -> form?.set(node.field, node.maxLength?.let { v.take(it) } ?: v) },
            placeholder = { Text(node.placeholder) },
            maxLines = 4,
            modifier = Modifier.weight(1f).testTag("textfield:${node.field}"),
        )
        if (submit != null) {
            IconButton(
                onClick = {
                    if (value.isNotBlank()) {
                        dispatch(submit)
                        form?.remove(node.field)
                    }
                },
                enabled = value.isNotBlank(),
                modifier = Modifier.testTag("textfield-send:${node.field}"),
            ) {
                Icon(ConstructIcons.Send, contentDescription = "Send")
            }
        }
    }
}
