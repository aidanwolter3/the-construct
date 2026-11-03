package org.theconstruct.android

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationChannelGroup
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.util.Log
import org.json.JSONObject
import org.theconstruct.AppNotification
import org.theconstruct.NotificationChannelInfo

private const val TAG = "ConstructNotify"

/** Intent extras of a tapped notification (DECISIONS D47): [MainActivity] opens the app and its target. */
internal const val EXTRA_NOTIFICATION_APP = "notificationApp"
internal const val EXTRA_NOTIFICATION_TARGET = "notificationTarget"
internal const val EXTRA_NOTIFICATION_ID = "notificationId"

/**
 * OS notifications for notifications from apps (DECISIONS D47). Generic: the pod declares the channels (one Android
 * channel per app channel, id `"<app_id>.<channel>"`, grouped per app) and writes the text; nothing here knows an app.
 */
internal object AppNotifications {
    private const val PREFS = "notifications"
    private const val KEY_CHANNELS = "channels"
    private const val GROUP_PREFIX = "app:"

    /** Whether the activity is in the foreground (then the in-app bell shows arrivals; no OS notification). */
    @Volatile
    var foreground: Boolean = false

    fun permitted(context: Context): Boolean =
        Build.VERSION.SDK_INT < 33 ||
            context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED

    private fun manager(context: Context) = context.getSystemService(NotificationManager::class.java)

    private fun importance(level: String): Int = when (level) {
        "low" -> NotificationManager.IMPORTANCE_LOW
        "high" -> NotificationManager.IMPORTANCE_HIGH
        else -> NotificationManager.IMPORTANCE_DEFAULT
    }

    /**
     * Make the Android channels match the pod's list: create or rename each, delete ours the pod no longer lists.
     * The user's per-channel choices live in Android's settings; the pod's per-device switches (in-app settings)
     * decide what is pushed at all. Also remembers the list for [show] while the app is closed.
     */
    fun syncChannels(context: Context, channels: List<NotificationChannelInfo>, appTitle: (String) -> String?) {
        if (channels.isEmpty()) return
        val nm = manager(context) ?: return
        val apps = channels.map { it.appId }.distinct()
        nm.createNotificationChannelGroups(apps.map { NotificationChannelGroup(GROUP_PREFIX + it, appTitle(it) ?: it) })
        nm.createNotificationChannels(
            channels.map { c ->
                NotificationChannel(androidId(c.appId, c.id), c.title, importance(c.importance)).apply {
                    description = c.description
                    group = GROUP_PREFIX + c.appId
                }
            },
        )
        val wanted = channels.map { androidId(it.appId, it.id) }.toSet()
        nm.notificationChannels
            .filter { it.group?.startsWith(GROUP_PREFIX) == true && it.id !in wanted }
            .forEach { nm.deleteNotificationChannel(it.id) }
        val saved = JSONObject()
        channels.forEach { saved.put(androidId(it.appId, it.id), it.title) }
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putString(KEY_CHANNELS, saved.toString()).apply()
    }

    fun androidId(appId: String, channel: String) = "$appId.$channel"

    /** A channel for [n] when [syncChannels] hasn't made it yet (a push before the app ever ran with this pod). */
    private fun ensureChannel(context: Context, n: AppNotification): String {
        val nm = manager(context)
        val id = androidId(n.appId, n.channel)
        if (nm != null && nm.getNotificationChannel(id) == null) {
            val saved = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(KEY_CHANNELS, null)
            val title = runCatching { JSONObject(saved ?: "{}").optString(id) }.getOrNull()?.takeIf { it.isNotBlank() }
                ?: n.channel.replaceFirstChar { it.uppercase() }
            nm.createNotificationChannelGroup(NotificationChannelGroup(GROUP_PREFIX + n.appId, n.appId))
            nm.createNotificationChannel(
                NotificationChannel(id, title, NotificationManager.IMPORTANCE_DEFAULT).apply { group = GROUP_PREFIX + n.appId },
            )
        }
        return id
    }

    /** Post [n] (tag = its id, so a second arrival of the same notification replaces it). */
    fun show(context: Context, n: AppNotification) {
        if (!permitted(context)) {
            Log.i(TAG, "notification ${n.id} not shown: no permission")
            return
        }
        val nm = manager(context) ?: return
        val open = Intent(context, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
            .putExtra(EXTRA_NOTIFICATION_APP, n.appId)
            .putExtra(EXTRA_NOTIFICATION_ID, n.id)
            .apply { n.targetUri?.let { putExtra(EXTRA_NOTIFICATION_TARGET, it) } }
        val pending = PendingIntent.getActivity(
            context, n.id.hashCode(), open, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val builder = Notification.Builder(context, ensureChannel(context, n))
            .setSmallIcon(R.drawable.ic_stat_notification)
            .setContentTitle(n.title)
            .setContentIntent(pending)
            .setAutoCancel(true)
            .setWhen(n.createdAt * 1000)
            .setShowWhen(true)
        if (n.body.isNotBlank()) {
            builder.setContentText(n.body).setStyle(Notification.BigTextStyle().bigText(n.body))
        }
        n.groupKey?.let { builder.setGroup("${n.appId}:$it") }
        try {
            nm.notify(n.id, 1, builder.build())
        } catch (e: SecurityException) {
            Log.w(TAG, "cannot post notification: $e")
        }
    }

    /** Remove a posted notification (it was read elsewhere). */
    fun cancel(context: Context, id: String) {
        manager(context)?.cancel(id, 1)
    }

    /**
     * A decrypted Web Push body from the pod (`{"v":1,"notification":{...},"unread_total":N}`), parsed without the
     * engine (the app may be closed). Null when malformed.
     */
    fun parsePush(json: String): AppNotification? = try {
        val n = JSONObject(json).getJSONObject("notification")
        fun opt(k: String) = if (n.isNull(k)) null else n.optString(k).takeIf { it.isNotEmpty() }
        AppNotification(
            id = n.getString("id"),
            appId = n.getString("app_id"),
            channel = n.getString("channel"),
            title = n.getString("title"),
            body = n.optString("body", ""),
            imageUrl = opt("image_url"),
            targetUri = opt("target_uri"),
            groupKey = opt("group_key"),
            createdAt = n.optLong("created_at", System.currentTimeMillis() / 1000),
            read = n.optBoolean("read", false),
        )
    } catch (e: Exception) {
        Log.w(TAG, "malformed push payload: $e")
        null
    }
}
