package org.theconstruct

import kotlin.test.Test
import kotlin.test.assertEquals

class StickyTest {
    // A 100 px sticky node at y = 300 in a parent that ends at y = 1000; pinned 50 px below the top.
    private fun shift(scroll: Int) = stickyShift(scroll, inset = 50, top = 300, height = 100, parentBottom = 1000)

    @Test
    fun scrollsNormallyUntilItReachesTheInset() {
        assertEquals(0, shift(0))
        assertEquals(0, shift(250))
    }

    @Test
    fun staysPinnedBelowTheInsetWhileTheParentScrolls() {
        assertEquals(1, shift(251))
        assertEquals(350, shift(600)) // drawn at 650 = 600 (view top) + 50
    }

    @Test
    fun scrollsAwayWithTheEndOfItsParent() {
        assertEquals(600, shift(850)) // its bottom at the parent's bottom
        assertEquals(600, shift(5000))
    }

    @Test
    fun aParentNoTallerThanItNeverMovesIt() {
        assertEquals(0, stickyShift(500, inset = 0, top = 300, height = 100, parentBottom = 380))
    }
}
