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
import androidx.compose.ui.unit.dp
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/** Maximum width of a FeedList item on wide screens. */
val FEED_MAX_WIDTH = 560.dp

/** The workspace's queries (`query_key` -> ordered entity URIs), read by FeedList. Provided by [ConstructApp]. */
val LocalConstructQueries: ProvidableCompositionLocal<Map<String, List<String>>> = staticCompositionLocalOf { emptyMap() }

/**
 * Renders one [UiNode]. [item] is the current item context inside a FeedList item template
 * (the post entity's fields plus a synthetic `uri` field holding its entity key). Intents
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
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
            )
            node.slots.forEach { slot ->
                // Keyed by slot id: a layout change that inserts a slot above (e.g. the agent adding a widget at
                // the top) keeps the state of the slots below (feed pager positions).
                key(slot.slotId) {
                    RenderUiNode(slot.content, entities, onDispatch, Modifier, item)
                }
            }
            // Room to scroll the last item out from under the agent FAB (ConstructApp).
            Spacer(Modifier.height(88.dp))
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
            },
            modifier = modifier,
        )

        is UiNode.Button -> Button(onClick = { dispatch(node.onClick) }, modifier = modifier) {
            Text(interpolate(node.label, entities, item))
        }

        is UiNode.FeedList -> {
            // Nested inside the Workspace's scrolling Column, so a plain Column (not LazyColumn).
            // Recomputed only when a snapshot changes the inputs, not on every recomposition of the list.
            val queries = LocalConstructQueries.current
            val posts = remember(node.queryKey, queries, entities) { feedItems(node.queryKey, queries, entities) }
            Column(modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally) {
                if (posts.isEmpty()) {
                    Text(
                        "No posts",
                        style = MaterialTheme.typography.bodyMedium,
                        color = Color.Gray,
                        modifier = Modifier.padding(16.dp),
                    )
                }
                posts.forEach { post ->
                    // Key by entity URI so per-item state (e.g. pager position) follows the post
                    // when the feed is reordered or a new post is inserted.
                    key(post["uri"]) {
                        // Phone: full width. Wider screens: a centered column like Instagram/Pixelfed web.
                        RenderUiNode(node.itemTemplate, entities, onDispatch, Modifier.widthIn(max = FEED_MAX_WIDTH), post)
                    }
                }
            }
        }

        is UiNode.PostCard -> {
            // post_ref is usually "$item.uri"; if it names another entity, use that as the item.
            val postUri = resolveRef(node.postRef, item)
            val post = postUri?.let { uri -> entities[uri]?.let { JsonObject(it + ("uri" to JsonPrimitive(uri))) } }
                ?: item
            Column(modifier.fillMaxWidth().padding(bottom = 12.dp)) {
                // Fixed render order author, media, action (then caption), whatever node each slot holds. Each is
                // keyed by its node type, so when a layout change moves a node to another slot (e.g. media to the
                // top) its state (the carousel page) moves with it.
                postCardSlotKeys(listOf(node.authorSlot, node.mediaSlot, node.actionSlot)).forEach { (slotKey, slot) ->
                    key(slotKey) {
                        RenderUiNode(slot, entities, onDispatch, Modifier, post)
                    }
                }
                post?.str("content_warning")?.let {
                    ContentWarningLabel(it, Modifier.padding(start = 14.dp, end = 14.dp, bottom = 6.dp))
                }
                // An empty caption (e.g. a boost without text) takes no space.
                val caption = node.captionSlot
                if (caption !is UiNode.Text || interpolate(caption.valueExpr, entities, post).isNotBlank()) {
                    RenderUiNode(caption, entities, onDispatch, Modifier.padding(horizontal = 14.dp), post)
                }
                HorizontalDivider(Modifier.padding(top = 12.dp), color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.5f))
            }
        }

        is UiNode.MediaCarousel -> MediaCarousel(node, entities, item, modifier)

        is UiNode.AuthorHeader -> AuthorHeader(node, entities, item, dispatch, modifier)

        is UiNode.ActionBar -> ActionBar(node, item, dispatch, modifier)

        is UiNode.Unknown -> Text(
            "Unsupported component: ${node.type.ifEmpty { "<missing type>" }}",
            style = MaterialTheme.typography.labelSmall,
            color = Color.Gray,
            modifier = modifier.padding(4.dp),
        )
    }
}

/** Pairs each node with a key unique among [slots]: its node kind plus an occurrence index ("ActionBar#0"). */
internal fun postCardSlotKeys(slots: List<UiNode>): List<Pair<String, UiNode>> {
    val seen = mutableMapOf<String, Int>()
    return slots.map { n ->
        val type = when (n) {
            is UiNode.Unknown -> "unknown:${n.type}"
            else -> n::class.simpleName ?: "node"
        }
        val i = seen[type] ?: 0
        seen[type] = i + 1
        "$type#$i" to n
    }
}
