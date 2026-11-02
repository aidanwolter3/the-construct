package org.genui.canvas

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
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
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.key
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.longOrNull

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
                RenderUiNode(slot.content, entities, onDispatch, Modifier, item)
            }
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
            val posts = entities.entries
                .filter { it.key.contains("/posts/") }
                .sortedByDescending { (it.value["created_at"] as? JsonPrimitive)?.longOrNull ?: 0L }
                .map { (uri, record) -> JsonObject(record + ("uri" to JsonPrimitive(uri))) }
            Column(modifier.fillMaxWidth()) {
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
                        RenderUiNode(node.itemTemplate, entities, onDispatch, Modifier, post)
                    }
                }
            }
        }

        is UiNode.PostCard -> {
            // post_ref is usually "$item.uri"; if it names another entity, use that as the item.
            val postUri = resolveRef(node.postRef, item)
            val post = postUri?.let { uri -> entities[uri]?.let { JsonObject(it + ("uri" to JsonPrimitive(uri))) } }
                ?: item
            Card(modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp)) {
                Column {
                    RenderUiNode(node.authorSlot, entities, onDispatch, Modifier, post)
                    RenderUiNode(node.mediaSlot, entities, onDispatch, Modifier, post)
                    RenderUiNode(node.actionSlot, entities, onDispatch, Modifier, post)
                    RenderUiNode(node.captionSlot, entities, onDispatch, Modifier.padding(12.dp), post)
                }
            }
        }

        is UiNode.MediaCarousel -> {
            val refs = resolveMediaRefs(node.mediaRefs, item)
            val ratio = node.aspectRatio.takeIf { it.isFinite() && it > 0.1f } ?: 1f
            val boxMod = modifier.fillMaxWidth().aspectRatio(ratio)
            when (refs.size) {
                0 -> MediaPlaceholder("No media", boxMod)
                1 -> MediaPlaceholder(mediaLabel(refs[0], entities), boxMod)
                else -> {
                    val pagerState = rememberPagerState { refs.size }
                    HorizontalPager(state = pagerState, modifier = boxMod) { page ->
                        MediaPlaceholder(
                            mediaLabel(refs[page], entities) + "\n(${page + 1}/${refs.size})",
                            Modifier.fillMaxSize(),
                        )
                    }
                }
            }
        }

        is UiNode.AuthorHeader -> {
            val userUri = resolveRef(node.userRef, item)
            val user = userUri?.let { entities[it] }
            val username = user?.get("username")?.let(::renderJsonValue)?.takeIf { it.isNotBlank() }
                ?: userUri ?: "unknown"
            val click = node.onClick
            Row(
                modifier
                    .fillMaxWidth()
                    .let { if (click != null) it.clickable { dispatch(click) } else it }
                    .padding(12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Box(
                    Modifier.size(32.dp).background(MaterialTheme.colorScheme.primaryContainer, CircleShape),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(username.take(1).uppercase(), style = MaterialTheme.typography.labelLarge)
                }
                Spacer(Modifier.width(8.dp))
                Text("@$username", style = MaterialTheme.typography.titleSmall)
            }
        }

        is UiNode.ActionBar -> {
            val liked = (item?.get("is_liked") as? JsonPrimitive)?.booleanOrNull == true
            val likeCount = item?.get("like_count")?.let(::renderJsonValue)
            Row(
                modifier.fillMaxWidth().padding(horizontal = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                TextButton(onClick = { dispatch(node.likeIntent) }) {
                    Text(
                        (if (liked) "♥ Like" else "♡ Like") + (likeCount?.let { " ($it)" } ?: ""),
                        color = if (liked) Color(0xFFE0245E) else MaterialTheme.colorScheme.onSurface,
                    )
                }
                TextButton(onClick = { dispatch(node.commentIntent) }) { Text("Comment") }
                Spacer(Modifier.weight(1f))
                TextButton(onClick = { dispatch(node.bookmarkIntent) }) { Text("Bookmark") }
            }
        }

        is UiNode.Unknown -> Text(
            "Unsupported component: ${node.type.ifEmpty { "<missing type>" }}",
            style = MaterialTheme.typography.labelSmall,
            color = Color.Gray,
            modifier = modifier.padding(4.dp),
        )
    }
}

private fun mediaLabel(uri: String, entities: Map<String, JsonObject>): String {
    val media = entities[uri]
    val alt = media?.get("alt_text")?.let(::renderJsonValue).orEmpty()
    val url = media?.get("url")?.let(::renderJsonValue)?.takeIf { it.isNotBlank() } ?: uri
    return listOf(alt, url).filter { it.isNotBlank() }.joinToString("\n")
}

@Composable
private fun MediaPlaceholder(text: String, modifier: Modifier) {
    // Image loading (e.g. Coil) comes later; show alt text and URL for now.
    Box(modifier.background(Color(0xFFDDDDDD)), contentAlignment = Alignment.Center) {
        Text(text, textAlign = TextAlign.Center, color = Color.DarkGray, modifier = Modifier.padding(16.dp))
    }
}
