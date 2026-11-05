package org.theconstruct

import kotlin.test.Test
import kotlin.test.assertEquals

class NodeRowTest {
    private val text = UiNode.Text("x", TextStyle.Body)

    @Test
    fun textLikeChildrenFlexAndControlsStayFixed() {
        assertEquals(RowPart.Flexible, rowPart(text))
        assertEquals(RowPart.Flexible, rowPart(UiNode.Container(Orientation.Vertical, 0, listOf(text))))
        assertEquals(RowPart.Flexible, rowPart(UiNode.Column(listOf(text))))
        assertEquals(RowPart.Flexible, rowPart(UiNode.Conditional("\$item.x", text)))
        assertEquals(RowPart.Fill, rowPart(UiNode.Spacer()))
        assertEquals(RowPart.Fixed, rowPart(UiNode.Spacer(12)))
        assertEquals(RowPart.Fixed, rowPart(UiNode.Chip("Showing")))
        assertEquals(RowPart.Fixed, rowPart(UiNode.Conditional("\$item.x", UiNode.Chip("Showing"))))
    }

    @Test
    fun flexibleChildrenKeepTheirWidthWhenTheyFit() {
        assertEquals(listOf(40, 60), shareRowWidth(listOf(40, 60), 200))
    }

    @Test
    fun aShortValueBesideALongTitleKeepsItsWidth() {
        // 300 px to share: the 50 px value keeps 50, the long title gets the other 250 and wraps.
        assertEquals(listOf(250, 50), shareRowWidth(listOf(900, 50), 300))
    }

    @Test
    fun twoLongTextsSplitEvenly() {
        assertEquals(listOf(150, 150), shareRowWidth(listOf(900, 400), 300))
        assertEquals(listOf(0, 0), shareRowWidth(listOf(10, 10), -5))
    }

    @Test
    fun spacersSplitTheLeftoverLikeWeights() {
        assertEquals(listOf(100), shareFill(100, 1))
        assertEquals(listOf(34, 33, 33), shareFill(100, 3).sortedDescending())
        assertEquals(100, shareFill(100, 3).sum())
        assertEquals(listOf(0, 0), shareFill(0, 2))
        assertEquals(emptyList<Int>(), shareFill(10, 0))
    }
}
