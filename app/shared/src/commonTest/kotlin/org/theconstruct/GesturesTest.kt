package org.theconstruct

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** `core://Gesture` and edge overlays (issue #123, decision `edge-overlays-and-gestures`). */
class GesturesTest {
    @Test
    fun aGestureAndAnOverlayFromTheTopDecodeFromTheWire() {
        val raw = """{"type":"core://Gesture","content":{"type":"core://Divider"},
            "on_swipe_down":{"intent":"action.navigation.open","layout":"inbox","presentation":"Overlay","edge":"top","size_dp":400},
            "on_long_press":{"intent":"action.navigation.back"}}"""
        val g = assertIs<UiNode.Gesture>(ConstructJson.decodeNode(raw))
        val open = assertIs<ActionIntent.Navigate>(g.onSwipeDown)
        assertEquals(PresentationMode.Overlay, open.presentation)
        assertEquals(Edge.Top, open.edge)
        assertEquals(400, open.sizeDp)
        assertEquals(ActionIntent.NavigateBack, g.onLongPress)
        assertNull(g.onTap)
        assertEquals(g.onSwipeDown, swipeHandler(g, SwipeDirection.Down))
        assertNull(swipeHandler(g, SwipeDirection.Up))
        val again = ConstructJson.encodeNode(g)
        assertTrue("\"edge\":\"top\"" in again && "on_tap" !in again, again)

        val layout = assertIs<UiNode.Layout>(
            ConstructJson.decodeNode("""{"type":"core://Layout","layout":"x","presentation":"Overlay","edge":"end"}"""),
        )
        assertEquals(EmbedPresentation.Overlay, layout.presentation)
        assertEquals(Edge.End, layout.edge)
    }

    @Test
    fun sheetsAndPanelsAreOverlaysFromTheBottomAndTheStart() {
        assertEquals(Edge.Bottom, overlayEdgeOf(PresentationMode.Sheet, null))
        assertEquals(Edge.Start, overlayEdgeOf(PresentationMode.Panel, null))
        assertEquals(Edge.Bottom, overlayEdgeOf(PresentationMode.Overlay, null))
        assertEquals(Edge.Top, overlayEdgeOf(PresentationMode.Overlay, Edge.Top))
        assertNull(overlayEdgeOf(PresentationMode.Push, null))
    }

    @Test
    fun aSwipeGoesFarEnoughMostlyOneWay() {
        assertNull(swipeDirection(0f, 40f, 48f), "too short")
        assertEquals(SwipeDirection.Down, swipeDirection(10f, 60f, 48f))
        assertEquals(SwipeDirection.Up, swipeDirection(0f, -80f, 48f))
        assertEquals(SwipeDirection.Left, swipeDirection(-90f, 20f, 48f))
        assertEquals(SwipeDirection.Right, swipeDirection(70f, -5f, 48f))
        assertNull(swipeDirection(60f, 60f, 48f), "diagonal is neither")
    }
}
