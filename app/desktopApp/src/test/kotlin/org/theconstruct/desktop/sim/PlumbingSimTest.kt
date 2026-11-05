package org.theconstruct.desktop.sim

import org.theconstruct.ConstructController
import java.io.File
import java.nio.file.Files
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertTrue

/**
 * Plumbing (decision `plumbing-and-porcelain`): a `core://Page` draws only its own nodes; its header row holds the
 * title and actions on one line; Back comes from `local://nav`; a page screen binds `$item` from its `item`; a list
 * can show only its groups.
 */
class PlumbingSimTest {
    private val outDir: File = Files.createTempDirectory("construct-plumbing-test").toFile()

    @AfterTest
    fun tearDown() {
        outDir.deleteRecursively()
    }

    private val home = """
{"version": "1.0.0", "workspace": {"type": "core://Page", "id": "home", "title": "Home", "root": {"type": "core://Column", "children": [
  {"type": "core://Row", "padding_h_dp": 12, "padding_v_dp": 4, "children": [
    {"type": "core://Text", "style": "Title", "value_expr": "Home"}, {"type": "core://Spacer"},
    {"type": "core://IconButton", "icon": "notifications", "content_description": "Notifications",
     "on_click": {"intent": "action.navigation.open", "layout": "inbox", "presentation": "Sheet"}},
    {"type": "core://IconButton", "icon": "settings", "content_description": "Settings",
     "on_click": {"intent": "action.navigation.open", "layout": "other", "presentation": "Push"}}]},
  {"type": "core://Scroll", "content": {"type": "core://Column", "children": [
    {"type": "core://Conditional", "value": "${'$'}{local://nav#can_go_back}",
     "content": {"type": "core://Text", "style": "Body", "value_expr": "Home can go back"}},
    {"type": "core://List", "query_key": "things",
     "group_by": {"field": "kind", "headers_only": true,
       "header": {"type": "core://Text", "style": "Label", "value_expr": "Kind ${'$'}item.kind"}},
     "item_template": {"type": "core://Text", "style": "Body", "value_expr": "Thing ${'$'}item.name"}},
    {"type": "core://Button", "label": "Open thing",
     "on_click": {"intent": "action.navigation.open", "target_uri": "x://things/2", "presentation": "Push"}}]}}]},
  "screens": [{"id": "thing", "route": "x://things/{id}", "title": "Thing", "item": "x://things/${'$'}route.id",
    "root": {"type": "core://Column", "children": [
      {"type": "core://Text", "style": "Title", "value_expr": "Name ${'$'}item.name"}]}}]},
 "entities": {"x://things/1": {"name": "one", "kind": "fruit"}, "x://things/2": {"name": "two", "kind": "fruit"},
   "x://things/3": {"name": "three", "kind": "stone"}},
 "queries": {"things": ["x://things/1", "x://things/2", "x://things/3"]}}"""

    private val other = """
{"version": "1.0.0", "workspace": {"type": "core://Page", "id": "other", "title": "Other", "root": {"type": "core://Column", "children": [
  {"type": "core://Row", "children": [
    {"type": "core://Conditional", "value": "${'$'}{local://nav#can_go_back}",
     "content": {"type": "core://IconButton", "icon": "arrow_back", "content_description": "Back",
       "on_click": {"intent": "action.navigation.back"}}},
    {"type": "core://Text", "style": "Title", "value_expr": "Other page"}]}]}},
 "entities": {}, "queries": {}}"""

    @Test
    fun aPageDrawsItsOwnHeaderBackScreensAndGroups() {
        val bridge = FakeEngineBridge(
            home, appId = "mine",
            layoutBundles = { id -> if (id == "other") other else layoutBundle(id, File("").absoluteFile) },
        )
        val r = SimRunner(ConstructController(bridge, "mine"), outDir, ScreenSpec.parse("phone")).run(
            parseScript(
                "assert-text Home; assert-text Notifications; assert-no-text Home can go back; " +
                    "assert-text Kind fruit; assert-text Kind stone; assert-no-text Thing one; " +
                    "click Open thing; assert-text Name two; back; " +
                    "click Settings; assert-text Other page; click Back; assert-no-text Other page; assert-text Kind fruit",
            ),
        )
        assertTrue(r.ok, r.steps.joinToString("\n") { it.logLine() })
    }
}
