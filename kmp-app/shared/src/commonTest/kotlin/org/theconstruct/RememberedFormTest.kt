package org.theconstruct

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

/** A bridge with a device store for remembered form values (scope -> field -> value). */
private class RememberingBridge(val stored: MutableMap<String, MutableMap<String, String>> = mutableMapOf()) :
    ConstructEngineBridge {
    var listener: ((EngineSnapshot) -> Unit)? = null
    var reads = 0
    override fun subscribe(onState: (EngineSnapshot) -> Unit) {
        listener = onState
    }
    override fun dispatch(intentJson: String) {}
    override fun rememberedFormValues(scope: String): Map<String, String> {
        reads++
        return stored[scope].orEmpty().toMap()
    }
    override fun rememberFormValue(scope: String, field: String, value: String?) {
        val form = stored.getOrPut(scope) { mutableMapOf() }
        if (value == null) form.remove(field) else form[field] = value
    }
}

private fun chips(field: String, remember: Boolean, vararg values: String) =
    """{"type":"core://ChoiceChips","field":"$field","remember":$remember,
        "options":[${values.joinToString(",") { """{"label":"L$it","value":"$it"}""" }}]}"""

private fun workspace(slot: String, screen: String = """{"type":"core://Divider"}""") = """
{"type":"core://Workspace","id":"app","title":"App",
 "slots":[{"slot_id":"s","provider_uri":"p","content":{"type":"core://Column","children":[$slot]}}],
 "screens":[{"id":"post","route":"x://posts/{id}","title":"Post","content":$screen}]}
"""

class RememberedFormTest {
    @Test
    fun rememberedChoicesFindsMarkedChipsAnywhere() {
        val node = ConstructJson.decodeNode(
            workspace(
                """{"type":"core://Conditional","value":"x","content":{"type":"core://Box","content":${chips("view", true, "", "cards")}}},
                   ${chips("cat", false, "", "a")}""",
            ),
        )
        assertEquals(mapOf("view" to setOf("", "cards")), rememberedChoices(listOf(node)))
        val decoded = ConstructJson.decodeNode(chips("view", true, ""))
        assertEquals(true, (decoded as UiNode.ChoiceChips).remember)
        // Not remembered: the field is omitted on the wire, like serde's skip_serializing_if.
        val plain = ConstructJson.encodeNode(UiNode.ChoiceChips("v", listOf(ChoiceOption("All"))))
        assertEquals(false, "remember" in plain)
    }

    @Test
    fun onlyAllowedStoredValuesAreApplied() {
        val form = mutableMapOf("kept" to "mine")
        applyRemembered(
            form,
            mapOf("view" to setOf("", "cards"), "kept" to setOf("x", "mine"), "gone" to setOf("", "a"), "unset" to setOf("")),
            mapOf("view" to "cards", "kept" to "x", "gone" to "removed-option", "unset" to ""),
        )
        assertEquals(mapOf("kept" to "mine", "view" to "cards"), form)
    }

    @Test
    fun theRootFormIsFilledBeforeTheLayoutShowsAndChoicesAreSaved() {
        val bridge = RememberingBridge(mutableMapOf(ROOT_FORM_SCOPE to mutableMapOf("view" to "cards")))
        val controller = ConstructController(bridge)
        bridge.listener!!(EngineSnapshot(workspace(chips("view", true, "", "cards")), "{}", true, 1, 0))
        assertEquals("cards", controller.navigation.rootForm["view"])
        assertEquals(1, bridge.reads)
        // Later snapshots of the same layout don't read again or undo the user's choice.
        controller.navigation.rootForm.remove("view")
        bridge.listener!!(EngineSnapshot(workspace(chips("view", true, "", "cards")), "{}", true, 2, 0))
        assertNull(controller.navigation.rootForm["view"])
        assertEquals(1, bridge.reads)
        controller.navigation.remember(ROOT_FORM_SCOPE, "view", null)
        assertEquals(0, bridge.stored.getValue(ROOT_FORM_SCOPE).size)
        // A layout without remembered fields never reads the device.
        val other = RememberingBridge()
        ConstructController(other)
        other.listener!!(EngineSnapshot(workspace(chips("view", false, "", "cards")), "{}", true, 1, 0))
        assertEquals(0, other.reads)
    }

    @Test
    fun aScreenFormIsFilledFromItsOwnScopeWhenItOpens() {
        val bridge = RememberingBridge(
            mutableMapOf("post" to mutableMapOf("size" to "big"), ROOT_FORM_SCOPE to mutableMapOf("size" to "small")),
        )
        val controller = ConstructController(bridge)
        bridge.listener!!(EngineSnapshot(workspace("""{"type":"core://Divider"}""", chips("size", true, "", "big", "small")), "{}", true, 1, 0))
        assertNull(controller.navigation.rootForm["size"])
        controller.dispatch(ActionIntent.Navigate("x://posts/1", PresentationMode.Push))
        assertEquals("big", controller.navigation.top!!.form["size"])
    }
}
