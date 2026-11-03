package org.theconstruct

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

class FeedTest {
    @Test
    fun controllerExposesQueriesAndAuth() {
        var listener: ((EngineSnapshot) -> Unit)? = null
        val controller = ConstructController(object : ConstructEngineBridge {
            override fun subscribe(onState: (EngineSnapshot) -> Unit) { listener = onState }
            override fun dispatch(intentJson: String) {}
        })
        listener!!(EngineSnapshot("null", "{}", true, 1, 0))
        assertEquals(emptyMap(), controller.state.value.queries)
        assertEquals(AuthInfo("signed_out"), controller.state.value.auth)
        listener!!(
            EngineSnapshot(
                "null", "{}", true, 2, 0,
                queriesJson = """{"timeline:home":["pixelfed://posts/2","pixelfed://posts/1"]}""",
                authJson = """{"state":"signed_in","username":"me"}""",
            ),
        )
        assertEquals(mapOf("timeline:home" to listOf("pixelfed://posts/2", "pixelfed://posts/1")), controller.state.value.queries)
        assertEquals(AuthInfo("signed_in", "me"), controller.state.value.auth)
    }

    private fun post(createdAt: Long) = buildJsonObject { put("created_at", createdAt) }

    private val entities: Map<String, JsonObject> = mapOf(
        "x://posts/1" to post(100),
        "x://posts/2" to post(300),
        "x://posts/3" to post(200),
        "x://users/9" to buildJsonObject { put("username", "u") },
    )

    private fun uris(items: List<JsonObject>?) = items?.map { (it["uri"] as JsonPrimitive).content }

    @Test
    fun listItemsFollowTheQuery() {
        val q = mapOf("k" to listOf("x://posts/1", "x://posts/3", "x://posts/2"))
        assertEquals(listOf("x://posts/1", "x://posts/3", "x://posts/2"), uris(listItems("k", q, entities)))
    }

    @Test
    fun listItemsSkipMissingEntitiesAndDuplicates() {
        val q = mapOf("k" to listOf("x://posts/3", "x://posts/404", "x://posts/3", "x://users/9"))
        // Any entity the query lists is an item: the query decides, not the URI shape.
        assertEquals(listOf("x://posts/3", "x://users/9"), uris(listItems("k", q, entities)))
        // The item keeps the entity's fields.
        assertEquals(JsonPrimitive(200), listItems("k", q, entities)!![0]["created_at"])
    }

    @Test
    fun anEmptyQueryHasNoItemsAndAnAbsentOneIsNotLoaded() {
        assertEquals(emptyList(), listItems("k", mapOf("k" to emptyList()), entities))
        assertNull(listItems("k", mapOf("other" to listOf("x://posts/1")), entities))
    }

    private fun media(aspect: Double?) = buildJsonObject { if (aspect != null) put("aspect_ratio", aspect) }

    @Test
    fun carouselAspectComesFromTheFirstItemClamped() {
        val ratio = { first: JsonObject? -> carouselAspectRatio(first, "aspect_ratio", 0.8, 1.91, 1.5) }
        assertEquals(1.0f, ratio(media(1.0)))
        assertEquals(1.7778f, ratio(media(1.7778)))
        assertEquals(0.8f, ratio(media(0.5))) // tall portrait -> the minimum
        assertEquals(1.91f, ratio(media(3.0))) // panorama -> the maximum
        // No bounds: unclamped.
        assertEquals(3.0f, carouselAspectRatio(media(3.0), "aspect_ratio", null, null, 1.0))
    }

    @Test
    fun carouselAspectFallsBackToTheNodeThenSquare() {
        assertEquals(1.5f, carouselAspectRatio(null, "aspect_ratio", 0.8, 1.91, 1.5))
        assertEquals(1.5f, carouselAspectRatio(media(null), "aspect_ratio", 0.8, 1.91, 1.5))
        assertEquals(1.5f, carouselAspectRatio(media(0.0), "aspect_ratio", 0.8, 1.91, 1.5))
        assertEquals(1.5f, carouselAspectRatio(buildJsonObject { put("aspect_ratio", "wide") }, "aspect_ratio", null, null, 1.5))
        assertEquals(1.5f, carouselAspectRatio(media(1.0), null, null, null, 1.5), "no aspect field: the node's")
        // The node's ratio is not clamped (it is the layout author's choice), but must be usable.
        assertEquals(2.5f, carouselAspectRatio(null, null, 0.8, 1.91, 2.5))
        assertEquals(1f, carouselAspectRatio(null, null, null, null, 0.0))
        assertEquals(1f, carouselAspectRatio(null, null, null, null, Double.NaN))
    }

    @Test
    fun blurhashAverageColour() {
        // DC "Ojn}" of the fixture carousel-1 hash.
        assertEquals(((('O'.code - 'A'.code + 10) * 83 + ('j'.code - 'a'.code + 36)) * 83 + 49) * 83 + 81, blurhashAverageRgb("00Ojn}"))
        assertEquals(0, blurhashAverageRgb("000000"))
        assertNull(blurhashAverageRgb(null))
        assertNull(blurhashAverageRgb("00Oj"))
        assertNull(blurhashAverageRgb("00O\"n}"))
        assertNull(blurhashAverageRgb("00~~~~")) // above 0xFFFFFF
    }

    @Test
    fun decodesQueriesJson() {
        assertEquals(emptyMap(), decodeQueries("{}"))
        assertEquals(emptyMap(), decodeQueries("null"))
        assertEquals(
            mapOf("timeline:home" to listOf("a", "b")),
            decodeQueries("""{"timeline:home":["a",1,"b"],"bad":"x"}"""),
        )
    }

    @Test
    fun parsesAuthJson() {
        assertEquals(AuthInfo("signed_out"), AuthInfo.parse("""{"state":"signed_out"}"""))
        assertEquals(AuthInfo("signed_in", "alice"), AuthInfo.parse("""{"state":"signed_in","username":"alice"}"""))
        assertEquals(AuthInfo("error", error = "denied"), AuthInfo.parse("""{"state":"error","error":"denied"}"""))
        assertEquals(
            AuthInfo("pending", message = "Loading your feed…"),
            AuthInfo.parse("""{"state":"pending","message":"Loading your feed…"}"""),
        )
        assertEquals(AuthInfo(), AuthInfo.parse("garbage"))
        assertEquals(
            AuthInfo("signed_out", session = true),
            AuthInfo.parse("""{"state":"signed_out","session":true}"""),
        )
        assertTrue(AuthInfo.parse("{}").state == "signed_out")
    }

    @Test
    fun conditionalTruthiness() {
        val item = buildJsonObject {
            put("s", "hi"); put("empty", ""); put("zero", 0); put("zs", "0"); put("n", 4); put("t", true); put("f", false)
            put("arr", kotlinx.serialization.json.JsonArray(emptyList()))
            put("nul", kotlinx.serialization.json.JsonNull)
        }
        for (truthy in listOf("\$item.s", "\$item.n", "\$item.t", "entity.s", "literal")) assertTrue(isTruthy(truthy, item), truthy)
        for (falsy in listOf("\$item.empty", "\$item.zero", "\$item.zs", "\$item.f", "\$item.arr", "\$item.nul", "\$item.missing", ""))
            assertTrue(!isTruthy(falsy, item), falsy)
        assertTrue(!isTruthy("\$item.s", null), "no item")
    }

    @Test
    fun hexColoursAndPageDescriptions() {
        assertEquals(androidx.compose.ui.graphics.Color(0xFFED4956), parseHexColor("#ED4956"))
        assertNull(parseHexColor("ED495"))
        assertNull(parseHexColor("#GGGGGG"))
        assertNull(parseHexColor(null))
        assertEquals("A snake", pageContentDescription("A snake", 0, 3))
        assertEquals("Photo 2 of 3", pageContentDescription(null, 1, 3))
        assertEquals("Photo 3 of 3", pageContentDescription("  ", 2, 3))
        assertEquals("Photo", pageContentDescription(null, 0, 1))
    }

    @Test
    fun textInterpolatesEmbeddedItemTokens() {
        val item = buildJsonObject { put("name", "Bob"); put("n", 3) }
        assertEquals("Bob boosted", interpolate("\$item.name boosted", emptyMap(), item))
        assertEquals("3 likes", interpolate("\$item.n likes", emptyMap(), item))
        assertEquals("\$item.nope here", interpolate("\$item.nope here", emptyMap(), item))
    }
}
