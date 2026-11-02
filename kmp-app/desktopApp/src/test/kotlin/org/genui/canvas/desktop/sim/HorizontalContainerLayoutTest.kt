package org.genui.canvas.desktop.sim

import androidx.compose.ui.geometry.Size
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.getBoundsInRoot
import androidx.compose.ui.test.hasClickAction
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.runSkikoComposeUiTest
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.height
import androidx.compose.ui.unit.width
import org.genui.canvas.CanvasApp
import org.genui.canvas.CanvasController
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/**
 * A horizontal Container whose children do not fit (the counter's three buttons on a 390dp phone) wraps
 * them onto the next line instead of squeezing the last one (which made "Reset" a tall pill with one
 * letter-pair per line).
 */
@OptIn(ExperimentalTestApi::class)
class HorizontalContainerLayoutTest {
    @Test
    fun buttonsWrapInsteadOfSqueezingOnPhone() {
        val bundle = assertNotNull(findFixture("counter_home_bundle.json")).readText()
        val controller = CanvasController(FakeEngineBridge(bundle))
        runSkikoComposeUiTest(size = Size(390f, 844f), density = Density(1f)) {
            setContent { CanvasApp(controller) }
            waitForIdle()
            fun bounds(label: String) = onNode(hasText(label) and hasClickAction()).getBoundsInRoot()
            val inc = bounds("Increment")
            val reset = bounds("Reset")
            // Same single-line height as its siblings, and still inside the screen.
            assertEquals(inc.height.value, reset.height.value, 0.5f, "Reset $reset vs Increment $inc")
            assertTrue(reset.right.value <= 390f, "Reset overflows: $reset")
            assertTrue(reset.width > reset.height, "Reset is not a horizontal pill: $reset")
        }
    }
}
