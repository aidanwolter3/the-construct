package org.theconstruct.android

import androidx.test.ext.junit.runners.AndroidJUnit4
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import org.theconstruct.bindItemTokens
import org.theconstruct.interpolate
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Runs the shared interpolation regexes on Android's ICU regex engine, which is stricter than the JVM's
 * java.util.regex (an unescaped `}` crashed the app at class init). JVM host tests can't catch that.
 * Run with `./gradlew :androidApp:connectedDebugAndroidTest` on a booted emulator/device.
 */
@RunWith(AndroidJUnit4::class)
class InterpolateDeviceTest {
    private val entities = mapOf(
        "core://counters/home" to buildJsonObject { put("count", JsonPrimitive(3)) },
    )

    @Test
    fun placeholderIsReplaced() {
        assertEquals("Count: 3", interpolate("Count: \${core://counters/home#count}", entities))
    }

    @Test
    fun missingEntityRendersEmpty() {
        assertEquals("x=", interpolate("x=\${core://nope#count}", entities))
    }

    @Test
    fun itemTokensAreBound() {
        val item = buildJsonObject { put("id", JsonPrimitive("101")) }
        assertEquals("pixelfed://posts/101", bindItemTokens("pixelfed://posts/\$item.id", item))
    }
}
