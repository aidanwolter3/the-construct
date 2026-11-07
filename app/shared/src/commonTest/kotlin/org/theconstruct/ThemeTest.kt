package org.theconstruct

import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlin.test.Test
import kotlin.test.assertEquals

/** `local://theme` from the pod's `core://theme` (decision `dark-palette`). */
class ThemeTest {
    private val pod = buildJsonObject {
        put("card", "#FAFAFA")
        put("brand_new", "#010203")
        put("brand_new_dark", "#040506")
        put("accent_dark", "not a colour")
    }

    @Test
    fun eachModeTakesOnlyItsOwnValues() {
        val light = themeEntity(pod, dark = false)
        val dark = themeEntity(pod, dark = true)
        assertEquals("\"#FAFAFA\"", light["card"].toString())
        // A light-only token keeps the dark default.
        assertEquals("\"#242428\"", dark["card"].toString())
        // A value that isn't a colour is ignored.
        assertEquals("\"#D9C8FF\"", dark["accent"].toString())
    }

    @Test
    fun tokensThisBuildDoesNotKnowStillBind() {
        assertEquals("\"#010203\"", themeEntity(pod, dark = false)["brand_new"].toString())
        assertEquals("\"#040506\"", themeEntity(pod, dark = true)["brand_new"].toString())
        assertEquals("\"#3B3320\"", themeEntity(null, dark = true)["highlight"].toString())
    }
}
