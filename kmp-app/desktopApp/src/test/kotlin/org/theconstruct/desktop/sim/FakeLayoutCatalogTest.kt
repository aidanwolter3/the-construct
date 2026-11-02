package org.theconstruct.desktop.sim

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.theconstruct.ActiveLayout
import org.theconstruct.ConstructController
import org.theconstruct.LayoutApiException
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNotEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class FakeLayoutCatalogTest {
    private fun fixture(name: String) = assertNotNull(findFixture(name)).readText()

    private fun json(s: String): JsonObject = Json.parseToJsonElement(s).jsonObject

    @Test
    fun homeCatalogListsLayoutsAndActivateSwitchesTheScreen() {
        val fake = FakeEngineBridge(fixture("counter_home_bundle.json"))
        val controller = ConstructController(fake)
        val before = controller.state.value
        assertEquals(ActiveLayout("home.counter", 3, 1), before.activeLayout)

        val list = json(fake.listLayouts())
        assertEquals("home", list["workspace_id"]!!.jsonPrimitive.content)
        val ids = list["layouts"]!!.jsonArray.map { it.jsonObject["id"]!!.jsonPrimitive.content }
        assertEquals(listOf("home.counter", "home.big_counter"), ids)

        val out = json(fake.activateLayout("home.big_counter"))
        assertEquals("false", out["unchanged"]!!.jsonPrimitive.content)
        val after = controller.state.value
        assertEquals(ActiveLayout("home.big_counter", 1, 2), after.activeLayout)
        // The real built-in (fixtures/layouts/home.big_counter.json): the count first, a caption title at the bottom.
        assertEquals(findFixture("layouts/home.big_counter.json")!!.readText().let { org.theconstruct.ConstructJson.decodeNode(it) }, after.workspace)

        // Activating counter at v2 (the AI weather widget) restores v2 as v4.
        val back = json(fake.activateLayout("home.counter", 2))
        assertEquals(2, back["restored_version"]!!.jsonPrimitive.int)
        assertEquals(ActiveLayout("home.counter", 4, 3), controller.state.value.activeLayout)
        // At v1 (equal to the newest content) nothing is appended.
        json(fake.activateLayout("home.counter", 4)).let { assertEquals(null, it["restored_version"]) }
    }

    @Test
    fun pixelfedVersionsPageAndRestore() {
        val fake = FakeEngineBridge(fixture("pixelfed_home_feed_bundle.json"))
        // The feed bundle (root id pixelfed_home) gets the pixelfed_home built-ins.
        val ids = json(fake.listLayouts())["layouts"]!!.jsonArray.map { it.jsonObject["id"]!!.jsonPrimitive.content }
        assertEquals(listOf("pixelfed_home.classic", "pixelfed_home.editorial", "pixelfed_home.compact", "pixelfed_home.minimal"), ids)
        val page = json(fake.layoutVersions("pixelfed_home.classic", limit = 2))
        assertEquals(listOf(4, 3), page["versions"]!!.jsonArray.map { it.jsonObject["version"]!!.jsonPrimitive.int })
        assertEquals(3, page["next_before"]!!.jsonPrimitive.int)
        val rest = json(fake.layoutVersions("pixelfed_home.classic", before = 3))
        assertEquals(listOf(2, 1), rest["versions"]!!.jsonArray.map { it.jsonObject["version"]!!.jsonPrimitive.int })
        assertEquals(listOf("rollback", "agent", "agent", "seed"), json(fake.layoutVersions("pixelfed_home.classic"))["versions"]!!.jsonArray.map { it.jsonObject["source"]!!.jsonPrimitive.content })
        assertNull(rest["next_before"])
        // The editorial built-in is the real fixture file.
        val editorial = json(fake.layoutVersion("pixelfed_home.editorial", 1))["layout"].toString()
        assertEquals(org.theconstruct.ConstructJson.decodeNode(findFixture("layouts/pixelfed_home.editorial.json")!!.readText()), org.theconstruct.ConstructJson.decodeNode(editorial))
        // The one-post demo bundle gets the pixelfed_demo ones.
        assertTrue(FakeEngineBridge(fixture("pixelfed_home_bundle.json")).listLayouts().contains("pixelfed_demo.editorial"))

        val v2 = json(fake.layoutVersion("pixelfed_home.classic", 2))
        assertEquals("core://Workspace", v2["layout"]!!.jsonObject["type"]!!.jsonPrimitive.content)

        val controller = ConstructController(fake)
        val before = controller.state.value.workspace
        val restored = json(fake.restoreLayoutVersion("pixelfed_home.classic", 2))
        assertEquals(5, restored["version"]!!.jsonPrimitive.int)
        assertNotEquals(before, controller.state.value.workspace)
        assertEquals(2, fake.layoutVersion)

        val e = assertFailsWith<LayoutApiException> { fake.layoutVersion("pixelfed_home.classic", 99) }
        assertEquals(LayoutApiException.Kind.NotFound, e.kind)
        fake.setOnline(false)
        val offline = assertFailsWith<LayoutApiException> { fake.listLayouts() }
        assertEquals(LayoutApiException.Kind.Offline, offline.kind)
        assertTrue(offline.message.isNotBlank())
    }
}
