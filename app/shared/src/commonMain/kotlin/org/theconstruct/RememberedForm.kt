package org.theconstruct

import androidx.compose.runtime.ProvidableCompositionLocal
import androidx.compose.runtime.staticCompositionLocalOf

// Remembered form values (schema 10, docs/decisions/remembered-form-values.md). The layout marks which form values
// the device keeps (`core://ChoiceChips` `remember`); this file knows no app and no field.

/** The form scope of an app's own layout (its slots); a screen's form uses the screen's id. */
const val ROOT_FORM_SCOPE = ""

/**
 * The remembered fields among [nodes] and their subtrees: field -> the option values a stored value may take. Only a
 * stored value that is still one of these is restored, so a layout that drops an option never shows an empty view.
 */
fun rememberedChoices(nodes: Iterable<UiNode>): Map<String, Set<String>> {
    val out = LinkedHashMap<String, MutableSet<String>>()
    fun walk(node: UiNode) {
        when (node) {
            is UiNode.ChoiceChips -> if (node.remember) {
                out.getOrPut(node.field) { LinkedHashSet() }.addAll(node.options.map { it.value })
            }
            is UiNode.Workspace -> node.slots.forEach { walk(it.content) }
            is UiNode.Page -> walk(node.root)
            is UiNode.Scroll -> walk(node.content)
            is UiNode.Sticky -> walk(node.content)
            is UiNode.Container -> node.children.forEach(::walk)
            is UiNode.Row -> node.children.forEach(::walk)
            is UiNode.Column -> node.children.forEach(::walk)
            is UiNode.Conditional -> walk(node.content)
            is UiNode.Tappable -> walk(node.content)
            is UiNode.Gesture -> walk(node.content)
            is UiNode.Box -> walk(node.content)
            is UiNode.ItemList -> {
                walk(node.itemTemplate)
                node.groupBy?.let { walk(it.header) }
            }
            is UiNode.Carousel -> walk(node.itemTemplate)
            else -> {}
        }
    }
    nodes.forEach(::walk)
    return out
}

/**
 * Fills [form] with the remembered values of [choices] that [stored] holds and that are still allowed options; a
 * field already in [form] keeps its value. An empty stored value is the default option (the field stays unset).
 */
internal fun applyRemembered(
    form: MutableMap<String, String>,
    choices: Map<String, Set<String>>,
    stored: Map<String, String>,
) {
    for ((field, allowed) in choices) {
        val v = stored[field] ?: continue
        if (v.isEmpty() || v !in allowed || field in form) continue
        form[field] = v
    }
}

/** Saves a remembered form value of the form being drawn (null = back to the default option). */
fun interface FormMemory {
    fun save(field: String, value: String?)
}

/** The [FormMemory] of the form being drawn; null where nothing is remembered (e.g. previews). */
val LocalFormMemory: ProvidableCompositionLocal<FormMemory?> = staticCompositionLocalOf { null }
