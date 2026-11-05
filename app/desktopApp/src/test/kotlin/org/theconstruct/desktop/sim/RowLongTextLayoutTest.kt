package org.theconstruct.desktop.sim

import androidx.compose.ui.geometry.Size
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.SkikoComposeUiTest
import androidx.compose.ui.test.getBoundsInRoot
import androidx.compose.ui.test.hasContentDescription
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.runSkikoComposeUiTest
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.DpRect
import androidx.compose.ui.unit.height
import androidx.compose.ui.unit.width
import org.theconstruct.ConstructApp
import org.theconstruct.ConstructController
import kotlin.math.abs
import kotlin.test.Test
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/**
 * Issue #117: in a `core://Row`, long text followed by a Switch, Chip or Button wraps, and the control stays whole
 * and on screen at phone width (390 dp). Rows with short text look as before: the Spacer still pushes the control
 * to the end, and a chip right after its text sits beside it.
 */
@OptIn(ExperimentalTestApi::class)
class RowLongTextLayoutTest {
    private val phoneWidth = 390f

    private fun onPhone(block: SkikoComposeUiTest.() -> Unit) =
        runSkikoComposeUiTest(size = Size(phoneWidth, 844f), density = Density(1f)) {
            val bundle = assertNotNull(findFixture("row_long_text_bundle.json")).readText()
            val controller = ConstructController(FakeEngineBridge(bundle))
            setContent { ConstructApp(controller) }
            waitForIdle()
            block()
        }

    private fun SkikoComposeUiTest.text(t: String): DpRect = onNode(hasText(t, substring = true)).getBoundsInRoot()

    private fun SkikoComposeUiTest.described(d: String): DpRect = onNode(hasContentDescription(d)).getBoundsInRoot()

    /** The two "Showing" chip labels: [0] after the long note, [1] after the short one. */
    private fun SkikoComposeUiTest.chip(index: Int): DpRect = onAllNodes(hasText("Showing"))[index].getBoundsInRoot()

    private fun assertWhole(control: DpRect, text: DpRect, name: String) {
        assertTrue(control.right.value <= phoneWidth, "$name overflows the screen: $control")
        assertTrue(control.left.value >= text.right.value, "$name $control overlaps the text $text")
    }

    @Test
    fun longTextWrapsAndTheSwitchStaysOnScreen() = onPhone {
        val detail = text("A summary of everything")
        val switch = described("Weekly digest")
        assertWhole(switch, detail, "Switch")
        assertTrue(switch.width.value >= 40f, "Switch squeezed: $switch")
        assertTrue(detail.height.value > 25f, "the long text did not wrap: $detail")
    }

    @Test
    fun longTextWrapsAndTheChipStaysOnOneLine() = onPhone {
        val detail = text("Moved the watering schedule")
        val chip = chip(0)
        assertTrue(chip.width > chip.height, "chip label squeezed: $chip")
        assertTrue(chip.height.value < 30f, "chip label wrapped: $chip")
        assertWhole(chip, detail, "Chip")
        assertTrue(detail.height.value > 25f, "the long text did not wrap: $detail")
    }

    @Test
    fun longTextWrapsAndTheButtonStaysWhole() = onPhone {
        val label = text("Ask before lending")
        val button = onNode(hasText("Ask")).getBoundsInRoot()
        assertWhole(button, label, "Button")
        assertTrue(button.width > button.height, "button squeezed: $button")
        assertTrue(label.height.value > 25f, "the long text did not wrap: $label")
    }

    @Test
    fun shortTextKeepsTheControlAtTheEnd() = onPhone {
        val longRowSwitch = described("Weekly digest")
        val switch = described("Replies")
        val count = onNode(hasText("12")).getBoundsInRoot()
        // Same row padding: a Spacer pushes the switch and the trailing text to the same end edge as before.
        assertTrue(abs(switch.right.value - longRowSwitch.right.value) < 1f, "switch not at the end: $switch")
        assertTrue(abs(count.right.value - switch.right.value) < 1f, "trailing text not at the end: $count")
        // A chip right after short text (no Spacer) sits beside it: 8 dp spacing plus the chip's own padding.
        val version = text("Version 2")
        val gap = chip(1).left.value - version.right.value
        assertTrue(gap in 0f..40f, "chip not beside its text (gap $gap)")
    }
}
