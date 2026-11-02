package org.theconstruct.desktop.sim

import java.time.Instant
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.longOrNull
import org.theconstruct.ConstructJson

/**
 * The simulator's comments "server" (DECISIONS D34): the threads of `fixtures/pixelfed/comments.json` (the fake
 * upstream's fixture: `[{id, in_reply_to_id, account (username), created_at, text, favourites_count?}]`, parents
 * first) answered in the pod's `GET /v1/pixelfed/posts/{id}/comments` shape, plus comments posted through the fake
 * outbox. Authors come from the bundle's `pixelfed://users/*` entities by username, or are [selfUsername] (`is_own`).
 * Not thread-safe: [FakeEngineBridge] calls it under its lock.
 */
class FakeComments(
    fixtureJson: String?,
    private val users: Map<String, JsonObject>,
    private val selfUsername: String,
) {
    private data class C(
        val id: String,
        val parent: String,
        val username: String,
        val text: String,
        val createdAt: Long,
        val likes: Long,
        val clientId: String? = null,
    )

    private val all = mutableListOf<C>()
    private var nextId = 900_000_000_000_000_000L

    init {
        val items = fixtureJson?.let { ConstructJson.json.parseToJsonElement(it).jsonArray } ?: JsonArray(emptyList())
        for (e in items) {
            val o = e.jsonObject
            all += C(
                id = o.s("id") ?: continue,
                parent = o.s("in_reply_to_id") ?: continue,
                username = o.s("account") ?: continue,
                text = o.s("text") ?: "",
                createdAt = o.s("created_at")?.let { runCatching { Instant.parse(it).epochSecond }.getOrNull() } ?: 0L,
                likes = (o["favourites_count"] as? JsonPrimitive)?.longOrNull ?: 0L,
            )
        }
    }

    fun isComment(id: String): Boolean = all.any { it.id == id }

    /** The post a comment (or post) id belongs to. */
    fun rootOf(id: String): String {
        var cur = id
        repeat(64) { cur = all.firstOrNull { it.id == cur }?.parent ?: return cur }
        return cur
    }

    /** The pod's comments JSON for post [postId]: the thread depth first, siblings oldest first. */
    fun threadJson(postId: String): String {
        val out = mutableListOf<JsonObject>()
        fun walk(parent: String, depth: Int) {
            for (c in all.filter { it.parent == parent }) {
                out += json(c, depth)
                walk(c.id, (depth + 1).coerceAtMost(4))
            }
        }
        walk(postId, 0)
        return JsonObject(mapOf("post_id" to JsonPrimitive(postId), "comments" to JsonArray(out))).toString()
    }

    /** Adds a comment by [selfUsername] under [parentId]; the same [clientId] twice adds it once. */
    fun add(parentId: String, text: String, clientId: String?, now: Long = Instant.now().epochSecond) {
        if (clientId != null && all.any { it.clientId == clientId }) return
        all += C((nextId++).toString(), parentId, selfUsername, text, now, 0L, clientId)
    }

    private fun json(c: C, depth: Int): JsonObject {
        val user = users[c.username]
        val own = c.username == selfUsername
        return JsonObject(
            mapOf(
                "id" to JsonPrimitive(c.id),
                "in_reply_to_id" to JsonPrimitive(c.parent),
                "depth" to JsonPrimitive(depth),
                "author" to JsonObject(
                    mapOf(
                        "id" to (user?.get("id") ?: JsonPrimitive(c.username)),
                        "username" to JsonPrimitive(c.username),
                        "display_name" to (user?.get("display_name") ?: JsonPrimitive(if (own) "You" else c.username)),
                        "avatar_url" to (user?.get("avatar_url") ?: JsonNull),
                    ),
                ),
                "text" to JsonPrimitive(c.text),
                "created_at" to JsonPrimitive(c.createdAt),
                "like_count" to JsonPrimitive(c.likes),
                "is_liked" to JsonPrimitive(false),
                "is_own" to JsonPrimitive(own),
            ),
        )
    }

    private fun JsonObject.s(k: String): String? =
        (this[k] as? JsonPrimitive)?.takeIf { it.isString }?.contentOrNull?.takeIf { it.isNotBlank() }
}
