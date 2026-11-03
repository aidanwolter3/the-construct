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
    /** The live UI's controller, if any (the service may run while the app is closed). */
    @Volatile
    var controller: ConstructController? = null

    private const val PREFS = "push"
    private const val KEY_ENDPOINT = "endpoint"
    private const val KEY_P256DH = "p256dh"
    private const val KEY_AUTH = "auth"

    /**
     * Ask a distributor for an endpoint with the pod's VAPID key ([vapidKey], from `GET /v1/push/key`): the saved
     * or default distributor, else the embedded FCM one; a no-op without any. The endpoint arrives in
     * [PushService.onNewEndpoint]. Call on the main thread (it may show a distributor picker).
     */
    fun register(activity: Activity, vapidKey: String) {
        UnifiedPush.tryUseCurrentOrDefaultDistributor(activity) { ok ->
            if (ok) {
                try {
                    UnifiedPush.register(activity, vapid = vapidKey)
                } catch (e: Exception) {
                    Log.w(TAG, "push registration refused: $e")
                }
            } else {
                Log.i(TAG, "no UnifiedPush distributor: notifications only while the app runs")
            }
        }
    }

    /** Hand a saved endpoint to [c] (a new session on another server, or after a restart). */
    fun resend(context: Context, c: ConstructController) {
        val p = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val endpoint = p.getString(KEY_ENDPOINT, null) ?: return
        c.registerPush(endpoint, p.getString(KEY_P256DH, "")!!, p.getString(KEY_AUTH, "")!!)
    }

    fun save(context: Context, endpoint: String, p256dh: String, auth: String) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
            .putString(KEY_ENDPOINT, endpoint).putString(KEY_P256DH, p256dh).putString(KEY_AUTH, auth).apply()
    }

    fun forget(context: Context) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().clear().apply()
    }
}

/** Receives endpoints and messages from the UnifiedPush distributor (manifest: `PUSH_EVENT`). */
class PushService : UnifiedPushService() {
    override fun onNewEndpoint(endpoint: PushEndpoint, instance: String) {
        val keys = endpoint.pubKeySet
        if (keys == null) {
            Log.w(TAG, "endpoint without Web Push keys ignored")
            return
        }
        Log.i(TAG, "new push endpoint (temporary=${endpoint.temporary})")
        Push.save(applicationContext, endpoint.url, keys.pubKey, keys.auth)
        Push.controller?.registerPush(endpoint.url, keys.pubKey, keys.auth)
    }

    override fun onMessage(message: PushMessage, instance: String) {
        if (!message.decrypted) {
            Log.w(TAG, "undecrypted push message dropped")
            return
        }
        val json = message.content.toString(Charsets.UTF_8)
        // With the app running, let the engine update its inbox and counts too.
        val n = Push.controller?.handlePushMessage(json) ?: AppNotifications.parsePush(json) ?: return
        if (!AppNotifications.foreground) AppNotifications.show(applicationContext, n)
    }

    override fun onRegistrationFailed(reason: FailedReason, instance: String) {
        Log.w(TAG, "push registration failed: $reason")
    }

    override fun onUnregistered(instance: String) {
        Log.i(TAG, "push unregistered")
        Push.forget(applicationContext)
        Push.controller?.unregisterPush()
    }
}
