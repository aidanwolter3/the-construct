package org.theconstruct.android

import android.content.Context
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The AndroidKeyStore-backed session store round trip on a real device/emulator:
 * `./gradlew :androidApp:connectedDebugAndroidTest`.
 */
@RunWith(AndroidJUnit4::class)
class KeystoreSecureStoreTest {
    private val context: Context = InstrumentationRegistry.getInstrumentation().targetContext
    private val key = "pod_session:http://test.invalid"

    @Before
    @After
    fun clean() {
        KeystoreSecureStore(context).delete(key)
    }

    @Test
    fun setGetDeleteRoundTrip() {
        val store = KeystoreSecureStore(context)
        assertNull(store.get(key))
        val value = """{"token":"abc123","username":"constructdemo"}"""
        store.set(key, value)
        assertEquals(value, store.get(key))
        store.set(key, "second")
        assertEquals("second", store.get(key))
        store.delete(key)
        assertNull(store.get(key))
    }

    @Test
    fun valueSurvivesANewInstanceAndIsEncryptedAtRest() {
        val value = "session-token-é中"
        KeystoreSecureStore(context).set(key, value)
        assertEquals(value, KeystoreSecureStore(context).get(key))
        val raw = context.getSharedPreferences(KeystoreSecureStore.PREFS_NAME, Context.MODE_PRIVATE).getString(key, null)!!
        assertFalse("stored in plaintext: $raw", raw.contains("session-token"))
    }

    @Test
    fun undecryptableValueReadsAsAbsent() {
        context.getSharedPreferences(KeystoreSecureStore.PREFS_NAME, Context.MODE_PRIVATE)
            .edit().putString(key, "AAAA:BBBB").commit()
        assertNull(KeystoreSecureStore(context).get(key))
    }
}
