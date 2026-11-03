package org.theconstruct.android

import android.app.Activity
import android.content.Context
import android.util.Log
import org.theconstruct.ConstructController
import org.unifiedpush.android.connector.FailedReason
import org.unifiedpush.android.connector.UnifiedPush
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PushMessage
import org.unifiedpush.android.connector.PushService as UnifiedPushService

private const val TAG = "ConstructPush"

/**
 * Closed-app notifications (DECISIONS D52). The pod sends standard Web Push (RFC 8291, VAPID) to an endpoint a
 * UnifiedPush distributor gives this app: a distributor app such as ntfy, or, on phones without one, the embedded FCM
 * distributor (Play services, through the UnifiedPush project's public Web Push -> FCM gateway). The connector
 * library keeps the Web Push keys and decrypts; this app only hands the endpoint to the pod and shows messages.
 */
internal object Push {
    /** The live UI's session: its push [instance] and [controller] (the service may run while the app is closed). */
    class Live(val instance: String, val controller: ConstructController)

    @Volatile
    var live: Live? = null

    /** The live controller if it is [instance]'s server, else null (that server's session isn't open). */
    fun controllerFor(instance: String): ConstructController? = live?.takeIf { it.instance == instance }?.controller

    private const val PREFS = "push"
    private const val KEY_ENDPOINT = "endpoint"
    private const val KEY_P256DH = "p256dh"
    private const val KEY_AUTH = "auth"

    /** The connector's instance name before registrations were per server (one shared by every server). */
    private const val LEGACY_INSTANCE = "default"

    /**
     * The UnifiedPush instance for the pod at [baseUrl]: one registration (endpoint and Web Push keys) per server, so
     * a server never learns the endpoint another server pushes to (the same device could be linked across servers).
     */
    fun instanceFor(baseUrl: String): String = "server:" + baseUrl.trimEnd('/')

    private fun key(instance: String, field: String) = "$instance|$field"

    /**
     * Ask a distributor for an endpoint for [instance] with its pod's VAPID key ([vapidKey], from `GET /v1/push/key`):
     * the saved or default distributor, else the embedded FCM one; a no-op without any. The endpoint arrives in
     * [PushService.onNewEndpoint]. Call on the main thread (it may show a distributor picker).
     */
    fun register(activity: Activity, instance: String, vapidKey: String) {
        UnifiedPush.tryUseCurrentOrDefaultDistributor(activity) { ok ->
            if (ok) {
                try {
                    UnifiedPush.register(activity, instance = instance, vapid = vapidKey)
                } catch (e: Exception) {
                    Log.w(TAG, "push registration refused: $e")
                }
            } else {
                Log.i(TAG, "no UnifiedPush distributor: notifications only while the app runs")
            }
        }
    }

    /** Hand [instance]'s saved endpoint (and only that one) to [c], its server's new session (e.g. after a restart). */
    fun resend(context: Context, instance: String, c: ConstructController) {
        val p = prefs(context)
        val endpoint = p.getString(key(instance, KEY_ENDPOINT), null) ?: return
        c.registerPush(endpoint, p.getString(key(instance, KEY_P256DH), "")!!, p.getString(key(instance, KEY_AUTH), "")!!)
    }

    fun save(context: Context, instance: String, endpoint: String, p256dh: String, auth: String) {
        prefs(context).edit()
            .putString(key(instance, KEY_ENDPOINT), endpoint)
            .putString(key(instance, KEY_P256DH), p256dh)
            .putString(key(instance, KEY_AUTH), auth)
            .apply()
    }

    fun forget(context: Context, instance: String) {
        prefs(context).edit()
            .remove(key(instance, KEY_ENDPOINT)).remove(key(instance, KEY_P256DH)).remove(key(instance, KEY_AUTH))
            .apply()
    }

    fun isLegacy(instance: String) = instance == LEGACY_INSTANCE

    /**
     * Older builds kept one registration (the connector's default instance) and handed it to every server picked. Drop
     * it once: its saved keys, and the registration itself, so the distributor retires that shared endpoint; each
     * server then gets its own instance the next time its session sets up notifications.
     */
    fun dropLegacyRegistration(context: Context) {
        val p = prefs(context)
        if (!p.contains(KEY_ENDPOINT)) return
        p.edit().remove(KEY_ENDPOINT).remove(KEY_P256DH).remove(KEY_AUTH).apply()
        try {
            UnifiedPush.unregister(context, LEGACY_INSTANCE)
            Log.i(TAG, "dropped the shared push registration of an older build")
        } catch (e: Exception) {
            Log.w(TAG, "cannot unregister the old shared push registration: $e")
        }
    }

    private fun prefs(context: Context) = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
}

/** Receives endpoints and messages from the UnifiedPush distributor (manifest: `PUSH_EVENT`). */
class PushService : UnifiedPushService() {
    override fun onNewEndpoint(endpoint: PushEndpoint, instance: String) {
        if (Push.isLegacy(instance)) {
            Log.i(TAG, "endpoint for the old shared registration ignored")
            return
        }
        val keys = endpoint.pubKeySet
        if (keys == null) {
            Log.w(TAG, "endpoint without Web Push keys ignored")
            return
        }
        Log.i(TAG, "new push endpoint for $instance (temporary=${endpoint.temporary})")
        Push.save(applicationContext, instance, endpoint.url, keys.pubKey, keys.auth)
        Push.controllerFor(instance)?.registerPush(endpoint.url, keys.pubKey, keys.auth)
    }

    override fun onMessage(message: PushMessage, instance: String) {
        if (!message.decrypted) {
            Log.w(TAG, "undecrypted push message dropped")
            return
        }
        val json = message.content.toString(Charsets.UTF_8)
        // With this server's session open, let its engine update the inbox and counts too.
        val live = Push.controllerFor(instance)
        val n = live?.handlePushMessage(json) ?: AppNotifications.parsePush(json) ?: return
        // The in-app bell only shows the open server's arrivals: another server's always become OS notifications.
        if (live == null || !AppNotifications.foreground) AppNotifications.show(applicationContext, n, instance)
    }

    override fun onRegistrationFailed(reason: FailedReason, instance: String) {
        Log.w(TAG, "push registration failed for $instance: $reason")
    }

    override fun onUnregistered(instance: String) {
        Log.i(TAG, "push unregistered: $instance")
        if (Push.isLegacy(instance)) return
        Push.forget(applicationContext, instance)
        Push.controllerFor(instance)?.unregisterPush()
    }
}
