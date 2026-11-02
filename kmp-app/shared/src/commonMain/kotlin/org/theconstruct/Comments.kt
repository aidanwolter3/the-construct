package org.theconstruct

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlin.random.Random
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.longOrNull

// Comments on a Pixelfed post (DECISIONS D34). The layout's comment button navigates to
// `pixelfed://posts/<id>/comments`; ConstructController opens this sheet for it. Reading is a blocking pod call
// (ConstructEngineBridge.postComments, not cached); posting is an ordinary outbox mutation ([commentIntent]) so it
// works offline and is retried. Test tags are listed in README "Simulation mode".

/** Longest comment the pod accepts. */
const val MAX_COMMENT_CHARS = 500

/** One comment from `GET /v1/pixelfed/posts/{id}/comments`. [createdAt] is epoch seconds (0 = unknown). */
data class Comment(
    val id: String,
    val inReplyToId: String? = null,
    val depth: Int = 0,
    val username: String = "",
    val displayName: String? = null,
    val avatarUrl: String? = null,
    val text: String = "",
    val createdAt: Long = 0,
    val likeCount: Long = 0,
    val isLiked: Boolean = false,
    val isOwn: Boolean = false,
)

/** A comment sent from this sheet that the thread does not show yet. [waiting]: still in the outbox. */
data class PendingComment(
    val clientId: String,
    val text: String,
    val replyTo: Comment? = null,
)

object CommentsJson {
    /** Parses the comments list; entries without an id are skipped. Throws on a body that is not the pod's shape. */
    fun parse(json: String): List<Comment> {
        val root = ConstructJson.json.parseToJsonElement(json).jsonObject
        val list = root["comments"] as? JsonArray ?: throw IllegalArgumentException("no comments array")
        return list.mapNotNull { e ->
            val o = e as? JsonObject ?: return@mapNotNull null
            val id = o.s("id") ?: return@mapNotNull null
            val author = o["author"] as? JsonObject
            Comment(
                id = id,
                inReplyToId = o.s("in_reply_to_id"),
                depth = (o.l("depth") ?: 0L).coerceIn(0L, 8L).toInt(),
                username = author?.s("username") ?: "",
                displayName = author?.s("display_name"),
                avatarUrl = author?.s("avatar_url"),
                text = o.s("text") ?: "",
                createdAt = o.l("created_at") ?: 0L,
                likeCount = (o.l("like_count") ?: 0L).coerceAtLeast(0L),
                isLiked = o.b("is_liked") == true,
                isOwn = o.b("is_own") == true,
            )
        }
    }

    private fun JsonObject.p(k: String): JsonPrimitive? = (this[k] as? JsonPrimitive)?.takeIf { it !is JsonNull }
    private fun JsonObject.s(k: String): String? = p(k)?.takeIf { it.isString }?.contentOrNull?.takeIf { it.isNotBlank() }
    private fun JsonObject.l(k: String): Long? = p(k)?.takeIf { !it.isString }?.longOrNull
    private fun JsonObject.b(k: String): Boolean? = p(k)?.takeIf { !it.isString }?.booleanOrNull
}

private val COMMENTS_TARGET = Regex("^pixelfed://posts/([A-Za-z0-9_-]{1,64})/comments$")

/** The post id of a `pixelfed://posts/<id>/comments` navigation target, else null. */
fun commentsTarget(targetUri: String): String? = COMMENTS_TARGET.matchEntire(targetUri.trim())?.groupValues?.get(1)

/** The post entity's URI. */
fun postEntityUri(postId: String): String = "pixelfed://posts/$postId"

/** A random nonce for the pod's `client_id` (its upstream `Idempotency-Key`): 24 hex characters. */
fun newCommentClientId(random: Random = Random.Default): String =
    random.nextBytes(12).joinToString("") { (it.toInt() and 0xFF).toString(16).padStart(2, '0') }

/**
 * The outbox intent that posts [text] on post [postId] (as a reply to comment [replyToId] when given): an optimistic
 * `reply_count + 1` on the post entity (rolled back to [replyCount] if the pod rejects it) and a POST to
 * `/v1/pixelfed/posts/comment`. A reply to a comment leaves the post's count as is (it counts direct comments).
 */
fun commentIntent(
    postId: String,
    text: String,
    clientId: String,
    replyCount: Long,
    replyToId: String? = null,
): ActionIntent.OptimisticMutate = ActionIntent.OptimisticMutate(
    entityUri = postEntityUri(postId),
    field = "reply_count",
    valueExpr = if (replyToId == null) "entity.reply_count + 1" else "entity.reply_count",
    rollbackValue = JsonPrimitive(replyCount),
    endpoint = "/v1/pixelfed/posts/comment",
    payload = JsonObject(
        mapOf(
            "id" to JsonPrimitive(replyToId ?: postId),
            "text" to JsonPrimitive(text),
            "client_id" to JsonPrimitive(clientId),
        ),
    ),
)

/** Why a draft can't be sent, or null when it can. */
fun commentDraftProblem(draft: String): String? {
    val t = draft.trim()
    return when {
        t.isEmpty() -> "Write something first."
        t.length > MAX_COMMENT_CHARS -> "Comments can be up to $MAX_COMMENT_CHARS characters."
        // The engine replaces a payload string that is exactly "$value" with the new count.
        t == VALUE_TOKEN -> "That comment can't be sent."
        else -> null
    }
}

/** Payload token the engine substitutes (client-core `VALUE_TOKEN`). */
internal const val VALUE_TOKEN = "\$value"

/**
 * The comments sheet's state, owned by [ConstructController] (survives rotation, like the menu). [postId] null =
 * closed. The thread loads in the sheet's effect (keyed on [loadToken]); sent comments show as [pending] until the
 * outbox is empty and a reload has them.
 */
@Stable
class CommentsState internal constructor(private val controller: ConstructController) {
    var postId by mutableStateOf<String?>(null)
        private set

    var comments: Load<List<Comment>> by mutableStateOf(Load.Loading)
        private set

    /** Bumped on open, Try again and after sending: the load is keyed on it. */
    var loadToken by mutableStateOf(0)
        private set

    var draft by mutableStateOf("")

    /** The comment being replied to (shown above the field), or null to comment on the post. */
    var replyTo by mutableStateOf<Comment?>(null)
        private set

    var draftError by mutableStateOf<String?>(null)
        private set

    val pending = mutableStateListOf<PendingComment>()

    /** Opens the sheet for [id] (a no-op for the post already open). */
    fun open(id: String) {
        if (postId == id) return
        postId = id
        comments = Load.Loading
        draft = ""
        replyTo = null
        draftError = null
        pending.clear()
        loadToken++
    }

    fun close() {
        postId = null
        replyTo = null
        draftError = null
        pending.clear()
    }

    /** Back cancels a reply first, then closes the sheet. */
    fun back() {
        if (replyTo != null) replyTo = null else close()
    }

    fun retry() {
        comments = Load.Loading
        loadToken++
    }

    fun reply(to: Comment) {
        replyTo = to
        draftError = null
    }

    fun cancelReply() {
        replyTo = null
    }

    fun onDraftChange(text: String) {
        draft = text
        draftError = null
    }

    /** Sends the draft through the outbox. False (with [draftError]) when it can't be sent. */
    fun send(): Boolean {
        val id = postId ?: return false
        commentDraftProblem(draft)?.let {
            draftError = it
            return false
        }
        val text = draft.trim()
        val clientId = newCommentClientId()
        val count = controller.state.value.entities[postEntityUri(id)]?.str("reply_count")?.toLongOrNull() ?: 0L
        controller.dispatch(commentIntent(id, text, clientId, count, replyTo?.id))
        pending += PendingComment(clientId, text, replyTo)
        draft = ""
        replyTo = null
        draftError = null
        return true
    }

    /** (Re)loads the thread; a failed reload keeps the list on screen. Sent comments the thread now has are dropped. */
    internal suspend fun load(id: String) {
        val wasPending = pending.toList()
        try {
            val list = controller.apps.comments(id)
            if (postId != id) return
            comments = Load.Ready(list)
            // The outbox was empty when this load started (see the sheet's effect), so these are in the thread now or
            // were rejected; either way the thread is the truth.
            if (controller.state.value.pendingActions == 0) pending.removeAll(wasPending)
        } catch (e: LayoutApiException) {
            if (postId != id) return
            if (comments !is Load.Ready) comments = Load.Failed(e.message)
        }
    }

    internal fun reloadAfterSend() {
        loadToken++
    }
}

private val Muted = Color(0xFF737373)

/**
 * The comments sheet over the layout (in-tree, like the menu's drawer, so headless captures see it): a top bar with
 * Close, the thread (indented replies, a Reply button per comment), comments waiting in the outbox, and the input row.
 * Tags: `comments-sheet`, `comments-close`, `comments-list`, `comment:<id>`, `comment-reply:<id>`, `comment-pending`,
 * `comments-error`, `comments-retry`, `comments-empty`, `comment-input`, `comment-send`, `comment-replying`.
 */
@Composable
internal fun CommentsSheet(controller: ConstructController, state: ConstructState, modifier: Modifier = Modifier) {
    val comments = controller.comments
    val postId = comments.postId ?: return
    PlatformBackHandler(enabled = !controller.menu.open) { comments.back() }
    LaunchedEffect(postId, comments.loadToken) { comments.load(postId) }
    // Sent comments: reload once the outbox has flushed them (offline they wait, shown as "Waiting to send").
    val flushed = state.pendingActions == 0 && comments.pending.isNotEmpty()
    LaunchedEffect(flushed) { if (flushed) comments.reloadAfterSend() }
    Surface(modifier.fillMaxSize().testTag("comments-sheet"), color = MaterialTheme.colorScheme.background) {
        Column(Modifier.fillMaxSize(), horizontalAlignment = Alignment.CenterHorizontally) {
            Column(Modifier.fillMaxSize().widthIn(max = FEED_MAX_WIDTH)) {
                Row(Modifier.fillMaxWidth().padding(start = 4.dp, end = 12.dp, top = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                    IconButton(onClick = comments::close, modifier = Modifier.testTag("comments-close")) {
                        Icon(ConstructIcons.ArrowBack, contentDescription = "Close comments")
                    }
                    Text(
                        "Comments",
                        style = MaterialTheme.typography.titleLarge,
                        modifier = Modifier.semantics { heading() },
                    )
                }
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.5f))
                Box(Modifier.weight(1f).fillMaxWidth()) {
                    CommentsBody(comments, state)
                }
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.5f))
                CommentInput(comments)
            }
        }
    }
}

@Composable
private fun CommentsBody(comments: CommentsState, state: ConstructState) {
    when (val load = comments.comments) {
        Load.Loading -> Row(
            Modifier.fillMaxWidth().padding(20.dp),
            horizontalArrangement = Arrangement.Center,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
            Spacer(Modifier.width(10.dp))
            Text("Loading comments…", style = MaterialTheme.typography.bodyMedium, color = Muted)
        }
        is Load.Failed -> Column(Modifier.fillMaxWidth().padding(20.dp).testTag("comments-error")) {
            Text(load.message, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.error)
            TextButton(onClick = comments::retry, modifier = Modifier.testTag("comments-retry")) { Text("Try again") }
        }
        is Load.Ready -> Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).testTag("comments-list")) {
            if (load.value.isEmpty() && comments.pending.isEmpty()) {
                Text(
                    "No comments yet. Be the first!",
                    style = MaterialTheme.typography.bodyMedium,
                    color = Muted,
                    modifier = Modifier.padding(20.dp).testTag("comments-empty"),
                )
            }
            load.value.forEach { c ->
                key(c.id) { CommentRow(c, onReply = { comments.reply(c) }) }
            }
            comments.pending.forEach { p ->
                key(p.clientId) { PendingRow(p, waiting = state.pendingActions > 0, offline = state.isOnline == false) }
            }
        }
    }
}

@Composable
private fun Avatar(url: String?, name: String) {
    val initial = name.trimStart('@').take(1).uppercase()
    ConstructImage(
        url = url,
        contentDescription = null,
        modifier = Modifier.size(32.dp).clip(CircleShape),
        placeholderColor = MaterialTheme.colorScheme.primaryContainer,
        placeholder = { Text(initial, style = MaterialTheme.typography.labelMedium, modifier = Modifier.align(Alignment.Center)) },
        error = { Text(initial, style = MaterialTheme.typography.labelMedium, modifier = Modifier.align(Alignment.Center)) },
    )
}

@Composable
private fun CommentRow(c: Comment, onReply: () -> Unit) {
    val name = c.displayName ?: c.username
    Row(
        Modifier.fillMaxWidth().padding(start = 16.dp + 24.dp * c.depth.coerceAtMost(4), end = 16.dp, top = 10.dp, bottom = 2.dp)
            .testTag("comment:${c.id}"),
    ) {
        Avatar(c.avatarUrl, name)
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    name,
                    style = MaterialTheme.typography.labelLarge,
                    fontWeight = FontWeight.SemiBold,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f, fill = false),
                )
                val time = versionTimeLabel(c.createdAt)
                if (time.isNotEmpty()) {
                    Spacer(Modifier.width(6.dp))
                    Text(time, style = MaterialTheme.typography.labelSmall, color = Muted, maxLines = 1)
                }
            }
            Text(c.text, style = MaterialTheme.typography.bodyMedium)
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (c.likeCount > 0) {
                    Text(
                        if (c.likeCount == 1L) "1 like" else "${c.likeCount} likes",
                        style = MaterialTheme.typography.labelSmall,
                        color = Muted,
                    )
                    Spacer(Modifier.width(12.dp))
                }
                Text(
                    "Reply",
                    style = MaterialTheme.typography.labelSmall,
                    fontWeight = FontWeight.SemiBold,
                    color = Muted,
                    modifier = Modifier.clickable(onClick = onReply).padding(vertical = 6.dp).testTag("comment-reply:${c.id}"),
                )
            }
        }
    }
}

@Composable
private fun PendingRow(p: PendingComment, waiting: Boolean, offline: Boolean) {
    Row(
        Modifier.fillMaxWidth().padding(start = if (p.replyTo != null) 40.dp else 16.dp, end = 16.dp, top = 10.dp, bottom = 6.dp)
            .testTag("comment-pending"),
    ) {
        Avatar(null, "You")
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f)) {
            Text("You", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
            Text(p.text, style = MaterialTheme.typography.bodyMedium, color = Muted)
            Text(
                when {
                    offline -> "Waiting to send (offline)"
                    waiting -> "Sending…"
                    else -> "Sent"
                },
                style = MaterialTheme.typography.labelSmall,
                color = Muted,
            )
        }
    }
}

@Composable
private fun CommentInput(comments: CommentsState) {
    Column(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp)) {
        comments.replyTo?.let { to ->
            Row(
                Modifier.fillMaxWidth().background(Color(0xFFF2F2F5)).padding(start = 12.dp).testTag("comment-replying"),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    "Replying to @${to.username}",
                    style = MaterialTheme.typography.labelMedium,
                    color = Muted,
                    modifier = Modifier.weight(1f),
                )
                IconButton(onClick = comments::cancelReply) {
                    Icon(ConstructIcons.Close, contentDescription = "Cancel reply", modifier = Modifier.size(18.dp))
                }
            }
        }
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(
                value = comments.draft,
                onValueChange = comments::onDraftChange,
                placeholder = { Text(if (comments.replyTo != null) "Write a reply…" else "Add a comment…") },
                maxLines = 4,
                isError = comments.draftError != null,
                supportingText = comments.draftError?.let { msg -> @Composable { Text(msg) } },
                modifier = Modifier.weight(1f).testTag("comment-input"),
            )
            IconButton(
                onClick = { comments.send() },
                enabled = comments.draft.isNotBlank(),
                modifier = Modifier.testTag("comment-send"),
            ) {
                Icon(ConstructIcons.Send, contentDescription = "Post comment")
            }
        }
    }
}
