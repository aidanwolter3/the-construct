package org.theconstruct.android

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import android.util.Base64
import java.security.SecureRandom

/**
 * A tapped OS notification lands here first (DECISIONS D52). [MainActivity] is exported (launcher, OAuth redirect), so
 * any installed app can start it with any extras; this activity is not exported, so only this app's own notification
 * PendingIntents reach it. It forwards the tap to [MainActivity] with [NotificationTaps.nonce], a secret that lives
 * only in this process, and [MainActivity] ignores notification extras without it.
 */
class NotificationTapActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val tap = intent
        val open = Intent(this, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
            .putExtra(EXTRA_NOTIFICATION_NONCE, NotificationTaps.nonce)
        for (key in listOf(EXTRA_NOTIFICATION_APP, EXTRA_NOTIFICATION_ID, EXTRA_NOTIFICATION_TARGET, EXTRA_NOTIFICATION_LAYOUT, EXTRA_NOTIFICATION_SERVER)) {
            tap?.getStringExtra(key)?.let { open.putExtra(key, it) }
        }
        startActivity(open)
        finish()
    }
}

internal object NotificationTaps {
    /** 128 random bits per process: another app cannot read it, and it is useless after the process ends. */
    val nonce: String by lazy {
        val bytes = ByteArray(16).also { SecureRandom().nextBytes(it) }
        Base64.encodeToString(bytes, Base64.NO_WRAP or Base64.URL_SAFE)
    }

    /** Whether [intent] came from [NotificationTapActivity] in this process. */
    fun trusted(intent: Intent): Boolean {
        val given = intent.getStringExtra(EXTRA_NOTIFICATION_NONCE) ?: return false
        return java.security.MessageDigest.isEqual(given.toByteArray(), nonce.toByteArray())
    }
}
