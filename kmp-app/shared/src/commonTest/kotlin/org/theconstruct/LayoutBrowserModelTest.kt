package org.theconstruct

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

class LayoutBrowserModelTest {
    @Test
    fun navigationLevelsAndBackClosesTheInnermost() {
        var nav = LayoutNav()
        assertEquals(LayoutNav.Level.Closed, nav.level)
        nav = nav.openBrowser()
        assertEquals(LayoutNav.Level.List, nav.level)
        // A version needs a layout.
        assertEquals(nav, nav.selectVersion(3))
        nav = nav.selectLayout("pixelfed_demo.classic")
        assertEquals(LayoutNav.Level.Details, nav.level)
        nav = nav.selectVersion(3).confirm()
        assertEquals(LayoutNav(true, "pixelfed_demo.classic", 3, confirming = true), nav)
        // Dialog -> preview -> details -> list -> closed.
        nav = nav.back(); assertEquals(LayoutNav(true, "pixelfed_demo.classic", 3), nav)
        nav = nav.back(); assertEquals(LayoutNav(true, "pixelfed_demo.classic"), nav)
        nav = nav.back(); assertEquals(LayoutNav(true), nav)
        nav = nav.back(); assertEquals(LayoutNav(), nav)
        assertEquals(LayoutNav(), nav.back())
        // The list has nothing to confirm; opening an open browser keeps its level.
        assertEquals(LayoutNav(true), LayoutNav(true).confirm())
        assertEquals(LayoutNav(true, "a"), LayoutNav(true, "a").openBrowser())
        assertEquals(LayoutNav(), LayoutNav(true, "a", 2, true).close())
    }

    @Test
    fun navigationSurvivesASaveRoundTrip() {
        val nav = LayoutNav(true, "home.counter", 2, confirming = true)
        assertEquals(nav, LayoutNav.fromSaveable(nav.toSaveable()))
        assertEquals(LayoutNav(), LayoutNav.fromSaveable(emptyList()))
    }

    @Test
    fun undoIsOfferedOnlyWhileTheScreenShowsWhatTheChangeProduced() {
        val snack = LayoutSnack("Switched", LayoutChange.Activate("ws.classic", 4), 1, after = ActiveLayout("ws.editorial", 1))
        assertTrue(undoStillApplies(snack, ActiveLayout("ws.editorial", 1, 7)))
        // Unknown values never block it.
        assertTrue(undoStillApplies(snack, ActiveLayout()))
        assertTrue(undoStillApplies(snack.copy(after = null), ActiveLayout("ws.minimal", 1)))
        // Another device switched layouts, or an AI edit added a version: undo would revert that instead.
        assertFalse(undoStillApplies(snack, ActiveLayout("ws.minimal", 1)))
        assertFalse(undoStillApplies(snack, ActiveLayout("ws.editorial", 2)))
        assertFalse(undoStillApplies(snack.copy(undo = null), ActiveLayout("ws.editorial", 1)))
    }

    private val classic = LayoutSummary("ws.classic", "Classic feed", currentVersion = 4, active = true, activeVersion = 4)
    private val editorial = LayoutSummary("ws.editorial", "Editorial journal", currentVersion = 2)

    @Test
    fun offeredChangePerLevel() {
        assertNull(offeredChange(classic, null))
        assertEquals(LayoutChange.Activate("ws.editorial"), offeredChange(editorial, null))
        assertNull(offeredChange(classic, 4))
        assertEquals(LayoutChange.Restore("ws.classic", 2), offeredChange(classic, 2))
        assertEquals(LayoutChange.Activate("ws.editorial"), offeredChange(editorial, 2))
        assertEquals(LayoutChange.Activate("ws.editorial", 1), offeredChange(editorial, 1))
    }

    @Test
    fun undoBookkeeping() {
        val before = ActiveLayout("ws.classic", 4, 9)
        val switched = LayoutChangeOutcome("ws.editorial", 2, unchanged = false)
        assertEquals(LayoutChange.Activate("ws.classic", 4), undoFor(LayoutChange.Activate("ws.editorial"), before, switched))
        assertEquals(
            LayoutChange.Activate("ws.classic", 4),
            undoFor(LayoutChange.Activate("ws.editorial", 1), before, switched.copy(version = 3, restoredVersion = 1)),
        )
        // A restore is undone by restoring the version shown before (appended again, never deleted).
        val restored = LayoutChangeOutcome("ws.classic", 5, unchanged = false, restoredVersion = 2)
        assertEquals(LayoutChange.Restore("ws.classic", 4), undoFor(LayoutChange.Restore("ws.classic", 2), before, restored))
        // Nothing changed, or nothing known about the screen before: no undo.
        assertNull(undoFor(LayoutChange.Activate("ws.editorial"), before, switched.copy(unchanged = true)))
        assertNull(undoFor(LayoutChange.Activate("ws.editorial"), ActiveLayout(), switched))
        assertNull(undoFor(LayoutChange.Restore("ws.other", 1), before, restored))
    }

    @Test
    fun messagesAndCopy() {
        val restore = LayoutChange.Restore("ws.classic", 2)
        assertEquals("Restored v2 as v5", changeMessage(restore, LayoutChangeOutcome("ws.classic", 5, false, 2), "Classic feed"))
        assertEquals("Undone. Restored v2 as v5", changeMessage(restore, LayoutChangeOutcome("ws.classic", 5, false, 2), "Classic feed", isUndo = true))
        assertEquals("v2 is already the newest version", changeMessage(restore, LayoutChangeOutcome("ws.classic", 4, true), "Classic feed"))
        val use = LayoutChange.Activate("ws.editorial")
        assertEquals("Switched to Editorial journal", changeMessage(use, LayoutChangeOutcome("ws.editorial", 2, false), "Editorial journal"))
        assertEquals("Undone. Back to Classic feed", changeMessage(use, LayoutChangeOutcome("ws.classic", 4, false), "Classic feed", isUndo = true))
        assertEquals(
            "Switched to Editorial journal (v1 restored as v3)",
            changeMessage(LayoutChange.Activate("ws.editorial", 1), LayoutChangeOutcome("ws.editorial", 3, false, 1), "Editorial journal"),
        )
        assertEquals("Switch to Editorial journal?", confirmCopy(use, editorial).title)
        assertTrue(confirmCopy(restore, classic).body.contains("(v5)"))
        assertEquals("Use this version", confirmCopy(LayoutChange.Activate("ws.editorial", 1), editorial).confirmLabel)
    }

    @Test
    fun versionLabels() {
        assertEquals("AI prompt", versionSourceLabel("agent"))
        assertEquals("Built-in", versionSourceLabel("seed"))
        assertEquals("Custom", versionSourceLabel("custom"))
        assertEquals("Copy of v1", versionDetail(LayoutVersionInfo(4, source = "rollback", prompt = "rollback to v1")))
        assertEquals("Copy of v12", versionDetail(LayoutVersionInfo(4, source = "restore", prompt = "restore v12")))
        assertEquals("“Hide captions”", versionDetail(LayoutVersionInfo(2, source = "agent", prompt = "Hide captions")))
        assertNull(versionDetail(LayoutVersionInfo(1, source = "seed")))
        val now = 1_000_000_000L
        assertEquals("just now", relativeTime(now - 5, now))
        assertEquals("5 min ago", relativeTime(now - 300, now))
        assertEquals("3 h ago", relativeTime(now - 3 * 3600 - 10, now))
        assertEquals("yesterday", relativeTime(now - 30 * 3600, now))
        assertEquals("4 days ago", relativeTime(now - 4 * 86400, now))
        assertNull(relativeTime(now - 40 * 86400, now))
        assertEquals("just now", relativeTime(now + 100, now)) // clock skew
    }

    @Test
    fun parsersAreLenient() {
        val list = LayoutCatalogJson.parseList(
            """{"layouts":[{"id":"home.counter","title":"Counter","current_version":3,"active":true,"active_version":3,"preview_hint":"counter"},
               {"id":"home.big_counter","current_version":"x"},{"title":"no id"},7]}""",
        )
        assertEquals(
            listOf(
                LayoutSummary("home.counter", "Counter", "", "counter", 3, true, 3),
                LayoutSummary("home.big_counter", "big_counter", currentVersion = 1),
            ),
            list,
        )
        assertEquals(emptyList(), LayoutCatalogJson.parseList("{}"))
        val page = LayoutCatalogJson.parseVersions(
            """{"versions":[{"version":2,"created_at":17,"source":"agent","prompt":"Hide captions","provider":"rules"},{"source":"seed"}],"next_before":2}""",
        )
        assertEquals(LayoutVersionPage(listOf(LayoutVersionInfo(2, 17, "agent", "Hide captions", "rules")), 2), page)
        assertEquals(
            LayoutChangeOutcome("home.big_counter", 1, false, null),
            LayoutCatalogJson.parseActivate("""{"workspace_id":"home","revision":2,"layout_id":"home.big_counter","layout_version":1,"unchanged":false}""", "x"),
        )
        assertEquals(
            LayoutChangeOutcome("home.counter", 5, false, 2),
            LayoutCatalogJson.parseRestore("""{"layout_id":"home.counter","version":5,"restored_from":2,"unchanged":false,"active":true}""", "x"),
        )
        val node = LayoutCatalogJson.parseVersionLayout(
            """{"version":1,"layout":{"type":"core://Workspace","id":"home","title":"Home","slots":[]}}""",
        )
        assertEquals(UiNode.Workspace("home", "Home", emptyList()), node)
    }
}
