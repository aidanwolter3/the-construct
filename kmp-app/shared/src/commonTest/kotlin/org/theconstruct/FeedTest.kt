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
        "pixelfed://posts/1" to post(100),
        "pixelfed://posts/2" to post(300),
        "pixelfed://posts/3" to post(200),
        "pixelfed://users/9" to buildJsonObject { put("username", "u") },
    )

    private fun uris(items: List<JsonObject>) = items.map { (it["uri"] as JsonPrimitive).content }

    @Test
    fun queriesOrderWinsOverCreatedAt() {
        val q = mapOf("timeline:home" to listOf("pixelfed://posts/1", "pixelfed://posts/3", "pixelfed://posts/2"))
        assertEquals(
            listOf("pixelfed://posts/1", "pixelfed://posts/3", "pixelfed://posts/2"),
            uris(feedItems("timeline:home", q, entities)),
        )
    }

    @Test
    fun queriesSkipMissingEntitiesAndDuplicates() {
        val q = mapOf("k" to listOf("pixelfed://posts/3", "pixelfed://posts/404", "pixelfed://posts/3", "pixelfed://users/9"))
        // A listed non-post entity is still rendered: the query decides, not the URI shape.
        assertEquals(listOf("pixelfed://posts/3", "pixelfed://users/9"), uris(feedItems("k", q, entities)))
        // The item keeps the entity's fields.
        assertEquals(JsonPrimitive(200), feedItems("k", q, entities)[0]["created_at"])
    }

    @Test
    fun emptyQueryRendersNothing() {
        assertEquals(emptyList(), feedItems("k", mapOf("k" to emptyList()), entities))
    }

    @Test
    fun absentQueryKeyFallsBackToPostsNewestFirst() {
        val q = mapOf("other" to listOf("pixelfed://posts/1"))
        assertEquals(
            listOf("pixelfed://posts/2", "pixelfed://posts/3", "pixelfed://posts/1"),
            uris(feedItems("timeline:home", q, entities)),
        )
    }

    private fun media(aspect: Double?) = buildJsonObject { if (aspect != null) put("aspect_ratio", aspect) }

    @Test
    fun carouselAspectIsClampedToInstagramRange() {
        assertEquals(1.0f, carouselAspectRatio(media(1.0), 1.5f))
        assertEquals(1.7778f, carouselAspectRatio(media(1.7778), 1f))
        assertEquals(0.8f, carouselAspectRatio(media(0.5), 1f)) // tall portrait -> 4:5
        assertEquals(1.91f, carouselAspectRatio(media(3.0), 1f)) // panorama -> 1.91:1
    }

    @Test
    fun carouselAspectFallsBackToNodeThenSquare() {
        assertEquals(1.5f, carouselAspectRatio(null, 1.5f))
        assertEquals(1.5f, carouselAspectRatio(media(null), 1.5f))
        assertEquals(1.5f, carouselAspectRatio(media(0.0), 1.5f))
        assertEquals(1.5f, carouselAspectRatio(buildJsonObject { put("aspect_ratio", "wide") }, 1.5f))
        // The node's ratio is not clamped (it is the layout author's choice), but must be usable.
        assertEquals(2.5f, carouselAspectRatio(null, 2.5f))
        assertEquals(1f, carouselAspectRatio(null, 0f))
        assertEquals(1f, carouselAspectRatio(null, Float.NaN))
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
        assertTrue(AuthInfo.parse("{}").state == "signed_out")
    }

    @Test
    fun contentWarningLabelDoesNotDoubleThePrefix() {
        assertEquals("CW: Snake photo", contentWarningText("Snake photo"))
        assertEquals("CW: orange", contentWarningText("CW: orange"))
        assertEquals("cw orange", contentWarningText("cw orange"))
        assertEquals("CW", contentWarningText("CW"))
        assertEquals("CW: Cwazy cats", contentWarningText("Cwazy cats"))
    }

    @Test
    fun carouselImagesWithoutAltTextGetAPositionLabel() {
        assertEquals("A snake", mediaContentDescription("A snake", 0, 3))
        assertEquals("Photo 2 of 3", mediaContentDescription(null, 1, 3))
        assertEquals("Photo 3 of 3", mediaContentDescription("  ", 2, 3))
        assertEquals("Photo", mediaContentDescription(null, 0, 1))
    }
}
