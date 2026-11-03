package org.theconstruct.desktop.sim

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.longOrNull
import org.theconstruct.AppNotification
import org.theconstruct.ConstructJson
import org.theconstruct.LayoutApiException
import org.theconstruct.NotificationChannelInfo

/**
 * In-memory stand-in for the pod's notifications (D52) with the engine's JSON shapes, shared by every
 * [FakeEngineBridge] of a simulator run (like [FakeAppCatalog]). Loaded from `fixtures/notifications.json`
 * ([fromFixture]: wire-shaped entries, except `age_seconds`, so `created_at` = now - age). [inject] adds a live arrival:
 * every subscribed bridge fires its listener and re-emits its snapshot (the unread count). Thread-safe.
 *
 * Paging: `before` is a notification id; the page holds the ones after it (older), `next_before` = the page's last id
 * while older ones remain.
 */
class FakeNotifications(
    notifications: List<AppNotification> = emptyList(),
    channels: List<NotificationChannelInfo> = emptyList(),
    private val clock: () -> Long = { System.currentTimeMillis() / 1000 },
) {
    /** Newest first. */
    private val items = notifications.sortedByDescending { it.createdAt }.toMutableList()
    private val channels = channels.toMutableList()
    private var nextId = 1

    /** Bridges to tell about changes: (live arrival or null, then re-emit). */
    private val watchers = mutableListOf<(AppNotification?) -> Unit>()

    /** Every `setChannelEnabled` call, for tests. */
    val channelChanges = mutableListOf<Triple<String, String, Boolean>>()

    @Synchronized
    internal fun watch(w: (AppNotification?) -> Unit) {
        watchers += w
    }

    @Synchronized
    internal fun unwatch(w: (AppNotification?) -> Unit) {
        watchers -= w
    }

    @Synchronized
    fun unreadJson(): String = unreadObject().toString()

    private fun unreadObject(): JsonObject {
        val unread = items.filter { !it.read }
        val byApp = unread.groupingBy { it.appId }.eachCount()
        return JsonObject(
            mapOf(
                "unread_total" to JsonPrimitive(unread.size),
                "unread_by_app" to JsonObject(byApp.mapValues { JsonPrimitive(it.value) }),
            ),
        )
    }

    @Synchronized
    fun unreadTotal(): Int = items.count { !it.read }

    @Synchronized
    fun listJson(before: String?, limit: Int, appId: String?): String {
        if (limit !in 0..100) throw LayoutApiException("The pod rejected the request.", LayoutApiException.Kind.Server, 400)
        val all = items.filter { appId == null || it.appId == appId }
        val start = before?.let { b -> all.indexOfFirst { it.id == b }.takeIf { it >= 0 }?.plus(1) ?: all.size } ?: 0
        val page = all.drop(start).take(limit)
        val more = start + page.size < all.size && page.isNotEmpty()
        val m = LinkedHashMap<String, kotlinx.serialization.json.JsonElement>()
        m["notifications"] = JsonArray(page.map { ConstructJson.json.encodeToJsonElement(AppNotification.serializer(), it) })
        m.putAll(unreadObject())
        if (more) m["next_before"] = JsonPrimitive(page.last().id)
        return JsonObject(m).toString()
    }

    fun markRead(ids: List<String>): String = change { items.replaceAll { if (it.id in ids) it.copy(read = true) else it } }

    fun markAllRead(appId: String?): String =
        change { items.replaceAll { if (appId == null || it.appId == appId) it.copy(read = true) else it } }

    @Synchronized
    fun channelsJson(): String = JsonObject(
        mapOf("channels" to JsonArray(channels.map { ConstructJson.json.encodeToJsonElement(NotificationChannelInfo.serializer(), it) })),
    ).toString()

    @Synchronized
    fun setChannelEnabled(appId: String, channel: String, enabled: Boolean) {
        val i = channels.indexOfFirst { it.appId == appId && it.id == channel }
        if (i < 0) throw LayoutApiException("That channel doesn't exist.", LayoutApiException.Kind.NotFound, 404)
        channels[i] = channels[i].copy(enabled = enabled)
        channelChanges += Triple(appId, channel, enabled)
        println("[sim] channel $appId.$channel enabled=$enabled")
    }

    @Synchronized
    fun channel(appId: String, id: String): NotificationChannelInfo? = channels.firstOrNull { it.appId == appId && it.id == id }

    /**
     * A live notification from [appId]'s [channel] (`notify`): newest, unread, no target. Fires the listeners of the
     * subscribed bridges. Returns it.
     */
    fun inject(appId: String, channel: String, title: String, body: String = "", targetUri: String? = null): AppNotification {
        val n: AppNotification
        val targets: List<(AppNotification?) -> Unit>
        synchronized(this) {
            n = AppNotification(
                id = "sim-${nextId++}", appId = appId, channel = channel, title = title, body = body,
                targetUri = targetUri, createdAt = clock(), read = false,
            )
            items.add(0, n)
            targets = watchers.toList()
        }
        println("[sim] notification ${n.id} from $appId.$channel: $title")
        targets.forEach { it(n) }
        return n
    }

    private fun change(block: () -> Unit): String {
        val (json, targets) = synchronized(this) {
            block()
            unreadJson() to watchers.toList()
        }
        targets.forEach { it(null) }
        return json
    }

    companion object {
        /** Parses `fixtures/notifications.json` (see the class docs). */
        fun fromFixture(json: String, clock: () -> Long = { System.currentTimeMillis() / 1000 }): FakeNotifications {
            val o = ConstructJson.json.parseToJsonElement(json).jsonObject
            val now = clock()
            val items = (o["notifications"] as? JsonArray).orEmpty().map { e ->
                val m = e.jsonObject.toMutableMap()
                val age = (m.remove("age_seconds") as? JsonPrimitive)?.longOrNull
                if (age != null) m["created_at"] = JsonPrimitive(now - age)
                ConstructJson.json.decodeFromJsonElement(AppNotification.serializer(), JsonObject(m))
            }
            val channels = (o["channels"] as? JsonArray).orEmpty().map { e ->
                val c = ConstructJson.json.decodeFromJsonElement(NotificationChannelInfo.serializer(), e)
                // `enabled` absent = the default.
                if ((e as? JsonObject)?.get("enabled")?.let { (it as? JsonPrimitive)?.booleanOrNull } == null) c.copy(enabled = c.defaultEnabled) else c
            }
            return FakeNotifications(items, channels, clock)
        }
    }
}
