package org.theconstruct

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonObject

// Notifications from apps (DECISIONS D52, docs/notifications.md): the wire model, lenient parsers and the pure rules the
// inbox follows (time labels, day groups). Nothing here knows any app. The Compose UI is in Inbox.kt.

/**
 * One notification (the pod's `Notification`). [createdAt] is in epoch seconds. [imageUrl] may be a pod path starting
 * with `/` (prefix the engine's base URL). [targetUri] is an `action.navigation.open` target the client routes against
 * the app's layout screens (D35); none = just open the app.
 */
@Serializable
data class AppNotification(
    val id: String,
    @SerialName("app_id") val appId: String,
    val channel: String,
    val title: String,
    val body: String = "",
    @SerialName("image_url") val imageUrl: String? = null,
    @SerialName("target_uri") val targetUri: String? = null,
    @SerialName("group_key") val groupKey: String? = null,
    @SerialName("created_at") val createdAt: Long,
    val read: Boolean = false,
)

/** One of an app's notification channels with this device's choice ([enabled]). [importance]: low, default or high. */
@Serializable
data class NotificationChannelInfo(
    @SerialName("app_id") val appId: String,
    val id: String,
    val title: String,
    val description: String = "",
    @SerialName("default_enabled") val defaultEnabled: Boolean = true,
    val importance: String = "default",
    val enabled: Boolean = defaultEnabled,
)

/** Unread notifications: in all ([total]) and per app ([byApp]). */
data class UnreadCounts(val total: Int = 0, val byApp: Map<String, Int> = emptyMap())

/** One page of `listNotifications`, newest first; pass [nextBefore] as `before` for the next (older) page. */
data class NotificationPage(
    val notifications: List<AppNotification>,
    val unread: UnreadCounts = UnreadCounts(),
    val nextBefore: String? = null,
)

/** Channel ids as platforms name them. */
object NotificationChannels {
    /** The OS channel id of [appId]'s [channel] (Android: `"<app_id>.<channel>"`). */
    fun androidId(appId: String, channel: String): String = "$appId.$channel"
}

/** Lenient parsers for the engine's notification JSON: unknown keys are ignored, malformed entries skipped. */
object NotificationsJson {
    private val json = ConstructJson.json

    private fun obj(s: String): JsonObject = json.parseToJsonElement(s).jsonObject

    /** A `Notification` object, or null when it lacks an id, app, channel or title. */
    fun parseNotification(s: String): AppNotification? = try {
        notification(json.parseToJsonElement(s))
    } catch (e: Exception) {
        null
    }

    private fun notification(e: JsonElement): AppNotification? = try {
        val o = e as? JsonObject ?: return null
        val n = json.decodeFromJsonElement(AppNotification.serializer(), o)
        n.takeIf { it.id.isNotBlank() && it.appId.isNotBlank() }
    } catch (e: Exception) {
        null
    }

    /** `{notifications, unread_total, unread_by_app, next_before?}`. Throws on a body that isn't a JSON object. */
    fun parsePage(s: String): NotificationPage {
        val o = obj(s)
        val items = (o["notifications"] as? JsonArray).orEmpty().mapNotNull(::notification)
        val next = (o["next_before"] as? JsonPrimitive)?.takeIf { it !is JsonNull }?.contentOrNull?.takeIf { it.isNotBlank() }
        return NotificationPage(items, unread(o), next)
    }

    /** `{unread_total, unread_by_app}`; zero counts when unreadable. */
    fun parseUnread(s: String): UnreadCounts = try {
        unread(obj(s))
    } catch (e: Exception) {
        UnreadCounts()
    }

    private fun unread(o: JsonObject): UnreadCounts {
        val byApp = (o["unread_by_app"] as? JsonObject).orEmpty().mapNotNull { (k, v) ->
            (v as? JsonPrimitive)?.takeIf { !it.isString }?.intOrNull?.let { k to it }
        }.toMap()
        val total = (o["unread_total"] as? JsonPrimitive)?.takeIf { !it.isString }?.intOrNull ?: byApp.values.sum()
        return UnreadCounts(total.coerceAtLeast(0), byApp)
    }

    /** `{"channels":[...]}`; entries without an app or id are skipped. */
    fun parseChannels(s: String): List<NotificationChannelInfo> =
        (obj(s)["channels"] as? JsonArray).orEmpty().mapNotNull { e ->
            try {
                json.decodeFromJsonElement(NotificationChannelInfo.serializer(), e as? JsonObject ?: return@mapNotNull null)
                    .takeIf { it.appId.isNotBlank() && it.id.isNotBlank() }
            } catch (x: Exception) {
                null
            }
        }
}

/** The badge's text: "" for none, "99+" past 99. */
fun unreadBadgeText(count: Int): String = when {
    count <= 0 -> ""
    count > 99 -> "99+"
    else -> count.toString()
}

/** An inbox row's time: "now", "3 min", "2 h", a weekday within the week ("Mon"), else the date ("Nov 2"). */
fun notificationTimeLabel(epochSeconds: Long, nowSeconds: Long): String {
    val d = (nowSeconds - epochSeconds).coerceAtLeast(0)
    return when {
        d < 60 -> "now"
        d < 3_600 -> "${d / 60} min"
        d < 86_400 -> "${d / 3_600} h"
        d < 7 * 86_400 -> formatWeekdayShort(epochSeconds)
        else -> formatShortDate(epochSeconds)
    }
}

/** The inbox's day header: "Today", "Yesterday", else the local date ("Monday, Nov 2"). */
fun notificationDayLabel(epochSeconds: Long, nowSeconds: Long): String =
    when (localEpochDay(nowSeconds) - localEpochDay(epochSeconds)) {
        0L -> "Today"
        1L -> "Yesterday"
        else -> formatDayHeader(epochSeconds)
    }

/** [items] (newest first) split into consecutive day groups, each with its [notificationDayLabel]. */
fun groupNotificationsByDay(items: List<AppNotification>, nowSeconds: Long): List<Pair<String, List<AppNotification>>> {
    val out = mutableListOf<Pair<String, MutableList<AppNotification>>>()
    for (n in items) {
        val label = notificationDayLabel(n.createdAt, nowSeconds)
        if (out.lastOrNull()?.first == label) out.last().second += n else out += label to mutableListOf(n)
    }
    return out
}

/** [url] as an absolute URL: a pod path (`/...`) is prefixed with [baseUrl] when known. */
fun notificationImageUrl(url: String?, baseUrl: String?): String? = when {
    url.isNullOrBlank() -> null
    url.startsWith("/") -> baseUrl?.trimEnd('/')?.let { it + url }
    else -> url
}

/** Days since 1970-01-01 in the local time zone. */
internal expect fun localEpochDay(epochSeconds: Long): Long

/** The local weekday, short ("Mon"). */
internal expect fun formatWeekdayShort(epochSeconds: Long): String

/** The local date, short ("Nov 2"; with the year when not this year). */
internal expect fun formatShortDate(epochSeconds: Long): String

/** The local date as a day header ("Monday, Nov 2"; with the year when not this year). */
internal expect fun formatDayHeader(epochSeconds: Long): String
