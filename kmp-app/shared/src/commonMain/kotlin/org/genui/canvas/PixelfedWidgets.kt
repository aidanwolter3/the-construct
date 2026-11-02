package org.genui.canvas

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.serialization.json.JsonObject

// Pixelfed domain widgets (DESIGN.md §2.2, M6 contract "Shared UI"). Each reads the current item (the post
// entity plus its `uri`), which PostCard passes down as the item context.

private val LikeRed = Color(0xFFED4956)
private val Muted = Color(0xFF737373)
private val DotActive = Color(0xFF0095F6)
private val DotInactive = Color(0xFFC7C7C7)

/**
 * Swipeable media: a [HorizontalPager] (test tag `carousel:<post uri>`) with a "n/N" badge and page dots
 * (content description "Page n of N", tag `carousel-dots:<post uri>`) when there is more than one item.
 * Aspect ratio per [carouselAspectRatio]. No media renders nothing.
 */
@Composable
internal fun MediaCarousel(
    node: UiNode.MediaCarousel,
    entities: Map<String, JsonObject>,
    item: JsonObject?,
    modifier: Modifier,
) {
    val refs = resolveMediaRefs(node.mediaRefs, item)
    if (refs.isEmpty()) return
    val media = refs.map { entities[it] }
    val ratio = carouselAspectRatio(media.first(), node.aspectRatio)
    val postUri = item?.str("uri") ?: refs.first()
    val pagerState = rememberPagerState { refs.size }
    Column(modifier.fillMaxWidth()) {
        Box(Modifier.fillMaxWidth().aspectRatio(ratio)) {
            if (refs.size == 1) {
                MediaImage(media[0], 0, 1, Modifier.fillMaxSize())
            } else {
                HorizontalPager(state = pagerState, modifier = Modifier.fillMaxSize().testTag("carousel:$postUri")) { page ->
                    MediaImage(media[page], page, refs.size, Modifier.fillMaxSize())
                }
                Text(
                    "${pagerState.currentPage + 1}/${refs.size}",
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
        if (refs.size > 1) {
            // Below the image, like Instagram: overlay dots disappear on light images.
            Row(
                Modifier
                    .align(Alignment.CenterHorizontally)
                    .padding(top = 10.dp)
                    .testTag("carousel-dots:$postUri")
                    .semantics { contentDescription = "Page ${pagerState.currentPage + 1} of ${refs.size}" },
                horizontalArrangement = Arrangement.spacedBy(5.dp),
            ) {
                repeat(refs.size) { i ->
                    val active = i == pagerState.currentPage
                    Box(Modifier.size(6.dp).background(if (active) DotActive else DotInactive, CircleShape))
                }
            }
        }
    }
}

/**
 * Accessibility label of carousel image [index] (0-based) of [count]: its alt text, else "Photo n of N"
 * (just "Photo" for a single image).
 */
internal fun mediaContentDescription(altText: String?, index: Int, count: Int): String =
    altText?.takeIf { it.isNotBlank() } ?: if (count > 1) "Photo ${index + 1} of $count" else "Photo"

@Composable
private fun MediaImage(media: JsonObject?, index: Int, count: Int, modifier: Modifier) {
    val alt = media?.str("alt_text")?.takeIf { it.isNotBlank() }
    val bg = blurhashAverageRgb(media?.str("blurhash"))?.let { Color(0xFF000000 or it.toLong()) } ?: Color(0xFFE8E8EC)
    CanvasImage(
        url = media?.str("url"),
        contentDescription = mediaContentDescription(alt, index, count),
        modifier = modifier,
        placeholderColor = bg,
        error = {
            Column(
                Modifier.align(Alignment.Center).padding(16.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Icon(CanvasIcons.BrokenImage, contentDescription = null, tint = Color(0xFF9A9AA0), modifier = Modifier.size(40.dp))
                Text(
                    alt ?: "Image unavailable",
                    textAlign = TextAlign.Center,
                    color = Color(0xFF6E6E73),
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.padding(top = 8.dp),
                )
            }
        },
    )
}

/** Round avatar, display name over @username, and a "<name> boosted" line when the item has `reblogged_by_uri`. */
@Composable
internal fun AuthorHeader(
    node: UiNode.AuthorHeader,
    entities: Map<String, JsonObject>,
    item: JsonObject?,
    dispatch: (ActionIntent) -> Unit,
    modifier: Modifier,
) {
    val userUri = resolveRef(node.userRef, item)
    val user = userUri?.let { entities[it] }
    val username = user?.str("username") ?: userUri ?: "unknown"
    val displayName = user?.str("display_name")
    val booster = item?.str("reblogged_by_uri")?.let { entities[it] }
        ?.let { it.str("display_name") ?: it.str("username") }
    val click = node.onClick
    Column(modifier.fillMaxWidth().let { if (click != null) it.clickable { dispatch(click) } else it }) {
        if (booster != null) {
            Row(Modifier.padding(start = 16.dp, top = 10.dp), verticalAlignment = Alignment.CenterVertically) {
                Icon(CanvasIcons.Repeat, contentDescription = null, tint = Muted, modifier = Modifier.size(14.dp))
                Spacer(Modifier.width(6.dp))
                Text("$booster boosted", style = MaterialTheme.typography.labelMedium, color = Muted)
            }
        }
        Row(Modifier.padding(horizontal = 12.dp, vertical = 10.dp), verticalAlignment = Alignment.CenterVertically) {
            val initial = (displayName ?: username).trimStart('@').take(1).uppercase()
            CanvasImage(
                url = user?.str("avatar_url"),
                contentDescription = null,
                modifier = Modifier.size(36.dp).clip(CircleShape),
                placeholderColor = MaterialTheme.colorScheme.primaryContainer,
                placeholder = { Text(initial, style = MaterialTheme.typography.labelLarge, modifier = Modifier.align(Alignment.Center)) },
                error = { Text(initial, style = MaterialTheme.typography.labelLarge, modifier = Modifier.align(Alignment.Center)) },
            )
            Spacer(Modifier.width(10.dp))
            Column {
                if (displayName != null) {
                    Text(
                        displayName,
                        style = MaterialTheme.typography.titleSmall,
                        fontWeight = FontWeight.SemiBold,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                Text(
                    "@$username",
                    style = if (displayName != null) MaterialTheme.typography.bodySmall else MaterialTheme.typography.titleSmall,
                    color = if (displayName != null) Muted else Color.Unspecified,
                    maxLines = 1,
                )
            }
        }
    }
}

/**
 * Like (heart, red when `is_liked`) with `like_count`, comment with `reply_count`, and a bookmark toggle
 * (filled when `is_bookmarked`). Content descriptions: Like/Unlike, Comment, Bookmark/Remove bookmark; test
 * tags `like:`/`comment:`/`bookmark:<post uri>`.
 */
@Composable
internal fun ActionBar(node: UiNode.ActionBar, item: JsonObject?, dispatch: (ActionIntent) -> Unit, modifier: Modifier) {
    val liked = item?.flag("is_liked") == true
    val bookmarked = item?.flag("is_bookmarked") == true
    val likeCount = item?.str("like_count")
    val replyCount = item?.str("reply_count")?.takeIf { it != "0" }
    val uri = item?.str("uri").orEmpty()
    Row(modifier.fillMaxWidth().padding(horizontal = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        IconButton(onClick = { dispatch(node.likeIntent) }, modifier = Modifier.testTag("like:$uri")) {
            Icon(
                if (liked) CanvasIcons.Favorite else CanvasIcons.FavoriteBorder,
                contentDescription = if (liked) "Unlike" else "Like",
                tint = if (liked) LikeRed else MaterialTheme.colorScheme.onSurface,
                modifier = Modifier.size(26.dp),
            )
        }
        if (likeCount != null) {
            Text(likeCount, style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
        }
        Spacer(Modifier.width(8.dp))
        IconButton(onClick = { dispatch(node.commentIntent) }, modifier = Modifier.testTag("comment:$uri")) {
            Icon(CanvasIcons.ChatBubbleOutline, contentDescription = "Comment", modifier = Modifier.size(24.dp))
        }
        if (replyCount != null) {
            Text(replyCount, style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
        }
        Spacer(Modifier.weight(1f))
        IconButton(onClick = { dispatch(node.bookmarkIntent) }, modifier = Modifier.testTag("bookmark:$uri")) {
            Icon(
                if (bookmarked) CanvasIcons.Bookmark else CanvasIcons.BookmarkBorder,
                contentDescription = if (bookmarked) "Remove bookmark" else "Bookmark",
                modifier = Modifier.size(26.dp),
            )
        }
    }
}

/** Label text for a content warning: "CW: " + [text], unless the author already wrote a "CW" word (any case). */
internal fun contentWarningText(text: String): String {
    val t = text.trim()
    val prefixed = t.startsWith("CW", ignoreCase = true) && (t.length == 2 || !t[2].isLetterOrDigit())
    return if (prefixed) t else "CW: $t"
}

/** The post's `content_warning` as a label above the caption (nothing is hidden yet). */
@Composable
internal fun ContentWarningLabel(text: String, modifier: Modifier = Modifier) {
    Row(
        modifier
            .background(Color(0xFFFFF4E0), RoundedCornerShape(8.dp))
            .padding(horizontal = 10.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(CanvasIcons.Warning, contentDescription = null, tint = Color(0xFFB26A00), modifier = Modifier.size(16.dp))
        Spacer(Modifier.width(6.dp))
        Text(contentWarningText(text), style = MaterialTheme.typography.labelLarge, color = Color(0xFF7A4A00))
    }
}
