package org.genui.canvas.desktop.sim

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.genui.canvas.ActiveLayout
import org.genui.canvas.CanvasController
import org.genui.canvas.LayoutApiException
import org.genui.canvas.UiNode
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertIs
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
        val controller = CanvasController(fake)
        val before = controller.state.value
        assertEquals(ActiveLayout("home.counter", 2, 1), before.activeLayout)

        val list = json(fake.listLayouts())
        assertEquals("home", list["workspace_id"]!!.jsonPrimitive.content)
        val ids = list["layouts"]!!.jsonArray.map { it.jsonObject["id"]!!.jsonPrimitive.content }
        assertEquals(listOf("home.counter", "home.big_counter"), ids)

        val out = json(fake.activateLayout("home.big_counter"))
        assertEquals("false", out["unchanged"]!!.jsonPrimitive.content)
        val after = controller.state.value
        assertEquals(ActiveLayout("home.big_counter", 1, 2), after.activeLayout)
        assertEquals("Big counter", assertIs<UiNode.Workspace>(after.workspace).title)

        // Activating counter at v1 restores v1 (retitled "Counter") as v3.
        val back = json(fake.activateLayout("home.counter", 1))
        assertEquals(1, back["restored_version"]!!.jsonPrimitive.int)
        assertEquals(ActiveLayout("home.counter", 3, 3), controller.state.value.activeLayout)
        assertEquals("Counter", assertIs<UiNode.Workspace>(controller.state.value.workspace).title)
    }

    @Test
    fun pixelfedVersionsPageAndRestore() {
        val fake = FakeEngineBridge(fixture("pixelfed_home_feed_bundle.json"))
        val page = json(fake.layoutVersions("pixelfed_demo.classic", limit = 2))
        assertEquals(listOf(3, 2), page["versions"]!!.jsonArray.map { it.jsonObject["version"]!!.jsonPrimitive.int })
        assertEquals(2, page["next_before"]!!.jsonPrimitive.int)
        val rest = json(fake.layoutVersions("pixelfed_demo.classic", before = 2))
        assertEquals(listOf(1), rest["versions"]!!.jsonArray.map { it.jsonObject["version"]!!.jsonPrimitive.int })
        assertNull(rest["next_before"])

        val v2 = json(fake.layoutVersion("pixelfed_demo.classic", 2))
        assertEquals("core://Workspace", v2["layout"]!!.jsonObject["type"]!!.jsonPrimitive.content)

        val controller = CanvasController(fake)
        val before = controller.state.value.workspace
        val restored = json(fake.restoreLayoutVersion("pixelfed_demo.classic", 2))
        assertEquals(4, restored["version"]!!.jsonPrimitive.int)
        assertNotEquals(before, controller.state.value.workspace)
        assertEquals(2, fake.layoutVersion)

        val e = assertFailsWith<LayoutApiException> { fake.layoutVersion("pixelfed_demo.classic", 99) }
        assertEquals(LayoutApiException.Kind.NotFound, e.kind)
        fake.setOnline(false)
        val offline = assertFailsWith<LayoutApiException> { fake.listLayouts() }
        assertEquals(LayoutApiException.Kind.Offline, offline.kind)
        assertTrue(offline.message.isNotBlank())
    }
}
