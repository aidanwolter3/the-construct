package org.theconstruct

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith

class LayoutCatalogTest {
    @Test
    fun activeLayoutParsesLeniently() {
        assertEquals(
            ActiveLayout("home.counter", 2, 5),
            ActiveLayout.parse("""{"layout_id":"home.counter","layout_version":2,"revision":5}"""),
        )
        assertEquals(ActiveLayout(), ActiveLayout.parse("{}"))
        assertEquals(ActiveLayout(), ActiveLayout.parse("not json"))
        assertEquals(ActiveLayout(revision = 3), ActiveLayout.parse("""{"layout_id":7,"layout_version":"2","revision":3}"""))
    }

    @Test
    fun controllerMapsLayoutJsonIntoState() {
        var listener: ((EngineSnapshot) -> Unit)? = null
        val bridge = object : ConstructEngineBridge {
            override fun subscribe(onState: (EngineSnapshot) -> Unit) {
                listener = onState
            }

            override fun dispatch(intentJson: String) {}
        }
        val controller = ConstructController(bridge)
        assertEquals(ActiveLayout(), controller.state.value.activeLayout)
        listener!!(
            EngineSnapshot(
                "null", "{}", true, 1, 0,
                layoutJson = """{"layout_id":"home.big_counter","layout_version":1,"revision":4}""",
            ),
        )
        assertEquals(ActiveLayout("home.big_counter", 1, 4), controller.state.value.activeLayout)
        // A bridge without a catalog throws a readable LayoutApiException.
        val e = assertFailsWith<LayoutApiException> { bridge.listLayouts() }
        assertEquals("Layouts aren't available here.", e.message)
        assertFailsWith<LayoutApiException> { bridge.activateLayout("home.counter") }
    }
}
