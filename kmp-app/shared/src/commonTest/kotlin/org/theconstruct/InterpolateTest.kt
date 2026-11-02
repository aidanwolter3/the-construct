package org.theconstruct

import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

class InterpolateTest {
    private val entities: Map<String, JsonObject> = mapOf(
        "core://counters/home" to buildJsonObject {
            put("id", "home")
            put("count", 7)
        },
        "pixelfed://posts/101" to buildJsonObject {
            put("caption", "Sunrise over Boulder")
            put("is_liked", false)
            put("ratio", 1.5)
            put("gone", JsonNull)
        },
    )

    private val item = buildJsonObject {
        put("id", "101")
        put("caption", "Sunrise over Boulder")
        put("like_count", 89)
        put("media_uris", buildJsonArray { add(kotlinx.serialization.json.JsonPrimitive("pixelfed://media/501")) })
    }

    @Test
    fun substitutesEntityFieldPlaceholders() {
        assertEquals("Count: 7", interpolate("Count: \${core://counters/home#count}", entities))
        assertEquals(
            "home=7 liked=false r=1.5",
            interpolate(
                "\${core://counters/home#id}=\${core://counters/home#count} liked=\${pixelfed://posts/101#is_liked} r=\${pixelfed://posts/101#ratio}",
                entities,
            ),
        )
    }

    @Test
    fun stringsRenderWithoutQuotes() {
        assertEquals("Sunrise over Boulder", interpolate("\${pixelfed://posts/101#caption}", entities))
    }

    @Test
    fun missingEntityOrFieldRendersEmpty() {
        assertEquals("Count: ", interpolate("Count: \${core://counters/nope#count}", entities))
        assertEquals("[]", interpolate("[\${core://counters/home#missing}]", entities))
        assertEquals("", interpolate("\${pixelfed://posts/101#gone}", entities))
    }

    @Test
    fun plainTextRendersAsIs() {
        assertEquals("The Construct Counter", interpolate("The Construct Counter", entities))
        assertEquals("entity.caption", interpolate("entity.caption", entities)) // no item: as-is
        assertEquals("cost \$5", interpolate("cost \$5", entities))
    }

    @Test
    fun itemExpressionsResolveAgainstItem() {
        assertEquals("Sunrise over Boulder", interpolate("entity.caption", entities, item))
        assertEquals("89", interpolate("\$item.like_count", entities, item))
        assertEquals("", interpolate("entity.missing", entities, item))
        // Not exactly a field reference: falls through to placeholder substitution / as-is.
        assertEquals("!entity.is_liked", interpolate("!entity.is_liked", entities, item))
        assertEquals("Count: 7", interpolate("Count: \${core://counters/home#count}", entities, item))
    }

    @Test
    fun resolveItemRefHelper() {
        assertEquals("101", resolveItemRef("\$item.id", item))
        assertEquals("[\"pixelfed://media/501\"]", resolveItemRef("\$item.media_uris", item))
        assertNull(resolveItemRef("\$item.nope", item))
        assertNull(resolveItemRef("pixelfed://posts/101", item))
        assertNull(resolveItemRef("entity.caption", item))
    }
}
