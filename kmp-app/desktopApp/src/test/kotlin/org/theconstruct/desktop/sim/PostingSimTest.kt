package org.theconstruct.desktop.sim

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import org.theconstruct.ActionIntent
import org.theconstruct.ConstructController
import org.theconstruct.ConstructJson
import org.theconstruct.CreatePosition
import org.theconstruct.encodeFormList
import java.io.File
import java.nio.file.Files
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * Photo posting (DECISIONS D49–D51, docs/POSTING.md) through the generic pieces: `core://MediaPicker`, the simulator's
 * `pick`, a create with `attachments` / `position: start` followed by `action.navigation.back` in an `action.sequence`,
 * and the fake pod finishing the post on `wait`. The layout mirrors the compose screen of layouts/pixelfed_home/0009.json
 * on a minimal feed.
 */
class PostingSimTest {
    private val outDir: File = Files.createTempDirectory("construct-posting-test").toFile()

    @AfterTest
    fun tearDown() {
        outDir.deleteRecursively()
    }

    private fun media(name: String): String = File(assertNotNull(findPixelfedMedia()), name).absolutePath

    private fun fake() = FakeEngineBridge(BUNDLE, appId = "posting_test")

    private fun run(fake: FakeEngineBridge, script: String, picker: SimMediaPicker = SimMediaPicker()): SimResult {
        val r = SimRunner(
            ConstructController(fake), outDir, ScreenSpec.parse("phone"), setOnline = fake::setOnline,
            mediaPicker = picker, settle = fake::settle,
        ).run(parseScript(script))
        assertTrue(r.ok, r.steps.joinToString("\n") { it.logLine() })
        return r
    }

    @Test
    fun pickingSharingAndFinishingAPost() {
        val fake = fake()
        val picker = SimMediaPicker()
        run(
            fake,
            "click tag:new-post; assert-text New post; assert-text Add photos; " +
                "pick ${media("carousel-1.png")} ${media("carousel-2.png")}; click tag:media-picker-add:photos; " +
                "assert-text Remove photo 1; assert-text Remove photo 2; assert-text Photo 2 of 2; " +
                // Removing one discards it.
                "click tag:media-picker-remove:sim-2; assert-no-text Photo 2 of 2; " +
                "type tag:textfield:caption Golden hour; click Share; " +
                "assert-no-text Add photos; assert-text Golden hour; assert-text Posting…; shot posting; " +
                "wait 10; assert-no-text Posting…; assert-text Golden hour; assert-text 0 likes",
            picker,
        )
        assertEquals(listOf(4), picker.requests)
        assertEquals(0, fake.pendingActions)
        assertEquals(listOf("sim-1"), fake.stagedAttachments)
    }

    @Test
    fun pickParsesPathsAndQuotedPaths() {
        val step = parseScript("pick a.png \"my photo.jpg\"  b.webp").single() as SimStep.Pick
        assertEquals(listOf("a.png", "my photo.jpg", "b.webp"), step.paths)
        for (bad in listOf("pick", "pick \"open.jpg")) {
            kotlin.test.assertFailsWith<ScriptParseException>(bad) { parseScript(bad) }
        }
    }

    @Test
    fun backDiscardsThePickedPhotos() {
        val fake = fake()
        run(
            fake,
            "click tag:new-post; pick ${media("carousel-1.png")}; click tag:media-picker-add:photos; assert-text Remove photo 1; " +
                "back; assert-no-text Add photos",
        )
        assertEquals(emptyList(), fake.stagedAttachments)
    }

    @Test
    fun aFileThatIsNotAPhotoShowsWhy() {
        val notAPhoto = File(outDir, "notes.txt").apply { writeText("hello") }
        run(
            fake(),
            "click tag:new-post; pick ${notAPhoto.absolutePath}; click tag:media-picker-add:photos; " +
                "assert-text That file isn't a photo this app can read.; assert-text Add photos",
        )
    }

    @Test
    fun offlineThePostStaysPendingUntilOnline() {
        val fake = fake()
        run(
            fake,
            "click tag:new-post; pick ${media("carousel-1.png")}; click tag:media-picker-add:photos; " +
                "type tag:textfield:caption Tunnel; offline; click Share; wait 10; assert-text Posting…; " +
                "online; idle; assert-no-text Posting…; assert-text Tunnel",
        )
    }

    @Test
    fun theFakeSubstitutesAttachmentsAndListsThePostFirst() {
        val fake = fake()
        val a = assertNotNull(fake.stageAttachment(media("carousel-1.png")).getOrNull())
        assertEquals("construct://attachments/${a.id}", a.uri)
        assertTrue(a.url.startsWith("file://"), a.url)
        val photos = listOf(a.id)
        val create = ActionIntent.Create(
            queryKey = "timeline:home", entityUri = "x://posts/pending-1",
            entity = ConstructJson.json.parseToJsonElement(
                """{"caption":"Hi","media_uris":"${'$'}attachments.photos","pending_label":"Posting…"}""",
            ),
            endpoint = "/v1/pixelfed/posts/create",
            payload = ConstructJson.json.parseToJsonElement("""{"media":"${'$'}attachments.photos"}"""),
            attachments = mapOf("photos" to JsonArray(photos.map(::JsonPrimitive))),
            position = CreatePosition.Start,
        )
        fake.dispatch(ConstructJson.encodeIntent(create))
        assertEquals(JsonArray(listOf(JsonPrimitive(a.uri))), fake.entityField("x://posts/pending-1", "media_uris"))
        assertEquals(1, fake.pendingActions)
        var snapshot: org.theconstruct.EngineSnapshot? = null
        fake.subscribe { snapshot = it }
        val queries = ConstructJson.json.parseToJsonElement(snapshot!!.queriesJson).jsonObject
        assertEquals(JsonPrimitive("x://posts/pending-1"), (queries["timeline:home"] as JsonArray).first())
        assertEquals(1, fake.settle())
        assertNull(fake.entityField("x://posts/pending-1", "pending_label"))
        assertEquals(JsonPrimitive(0), fake.entityField("x://posts/pending-1", "like_count"))
        assertNotNull(fake.entityField("x://posts/pending-1", "id"))
        // Unstaged attachments: ignored.
        fake.dispatch(ConstructJson.encodeIntent(create.copy(entityUri = "x://posts/2", attachments = mapOf("photos" to JsonArray(listOf(JsonPrimitive("nope")))))))
        assertNull(fake.entityField("x://posts/2", "caption"))
        // Form list values are what the picker stores.
        assertEquals("""["${a.id}"]""", encodeFormList(listOf(a.id)))
    }

    companion object {
        private val BUNDLE = """
{"version":"1","workspace":{"type":"core://Workspace","id":"posting_test","title":"Feed","slots":[
  {"slot_id":"top","provider_uri":"p","content":{"type":"core://IconButton","icon":"add_photo","content_description":"New post",
    "tag":"new-post","on_click":{"intent":"action.navigation.open","target_uri":"pixelfed://compose","presentation":"Modal"}}},
  {"slot_id":"feed","provider_uri":"p","content":{"type":"core://List","query_key":"timeline:home","empty_text":"No posts",
    "item_template":{"type":"core://Column","spacing_dp":4,"children":[
      {"type":"core://Text","value_expr":"${'$'}item.caption","style":"Body"},
      {"type":"core://Conditional","value":"${'$'}item.pending_label","content":{"type":"core://Text","value_expr":"${'$'}item.pending_label","style":"Secondary"}},
      {"type":"core://Conditional","value":"${'$'}item.id","content":{"type":"core://Text","value_expr":"${'$'}item.like_count likes","style":"Secondary"}},
      {"type":"core://Carousel","items":"${'$'}item.media_uris","item_template":{"type":"core://Image","url":"${'$'}item.url"}}]}}}],
 "screens":[{"id":"compose","route":"pixelfed://compose","title":"New post",
   "content":{"type":"core://Column","spacing_dp":12,"padding_h_dp":16,"padding_v_dp":12,"children":[
     {"type":"core://MediaPicker","field":"photos","max_items":4,"label":"Add photos"},
     {"type":"core://TextField","field":"caption","placeholder":"Write a caption…","max_length":2000,"lines":4}]},
   "footer":{"type":"core://Button","label":"Share","on_click":{"intent":"action.sequence","steps":[{"intent":"action.mutation.create","query_key":"timeline:home",
     "position":"start","entity_uri":"pixelfed://posts/pending-${'$'}nonce",
     "entity":{"author_name":"You","caption":"${'$'}form?.caption","media_uris":"${'$'}attachments.photos","pending_label":"Posting…"},
     "attachments":{"photos":"${'$'}form.photos"},"endpoint":"/v1/pixelfed/posts/create",
     "payload":{"caption":"${'$'}form?.caption","media":"${'$'}attachments.photos","client_id":"${'$'}nonce"},
     "refresh_workspace":true},{"intent":"action.navigation.back"}]}}}]},
 "entities":{"x://posts/old":{"id":"1","caption":"An older post","like_count":3,"media_uris":[]}},
 "queries":{"timeline:home":["x://posts/old"]}}
"""
    }
}
