package org.genui.canvas

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

// Text interpolation subset from docs/DECISIONS.md D4. Not a CEL evaluator.

private val PLACEHOLDER = Regex("""\$\{([^#}]+)#([^}]+)}""")
private val FIELD = Regex("""[A-Za-z_][A-Za-z0-9_]*""")

/**
 * Renders a text `value_expr`:
 * - Inside an item template ([item] != null), an expression that is exactly `entity.<field>`
 *   or `$item.<field>` resolves to that item field ("" when missing).
 * - Every `${<entity_uri>#<field>}` is replaced by that entity field's value, or "" when the
 *   entity or field is missing.
 * - Anything else renders as-is.
 *
 * Primitives render without JSON quotes; null renders as ""; arrays/objects render as JSON.
 */
fun interpolate(expr: String, entities: Map<String, JsonObject>, item: JsonObject? = null): String {
    if (item != null) {
        itemFieldName(expr, allowEntityPrefix = true)?.let { field ->
            return item[field]?.let(::renderJsonValue) ?: ""
        }
    }
    if (!expr.contains("\${")) return expr
    return PLACEHOLDER.replace(expr) { m ->
        val (uri, field) = m.destructured
        entities[uri]?.get(field)?.let(::renderJsonValue) ?: ""
    }
}

/**
 * Resolves a `$item.<field>` reference against [item].
 * Returns null when [expr] is not exactly such a reference or the field is missing,
 * so callers can fall back (e.g. to treating [expr] as a literal URI).
 */
fun resolveItemRef(expr: String, item: JsonObject): String? {
    val field = itemFieldName(expr, allowEntityPrefix = false) ?: return null
    return item[field]?.let(::renderJsonValue)
}

/** Renders a JSON value for display: strings unquoted, numbers/booleans as literals, null as "". */
fun renderJsonValue(value: JsonElement): String = when (value) {
    JsonNull -> ""
    is JsonPrimitive -> value.content
    else -> value.toString()
}

private fun itemFieldName(expr: String, allowEntityPrefix: Boolean): String? {
    val e = expr.trim()
    val rest = when {
        e.startsWith("\$item.") -> e.removePrefix("\$item.")
        allowEntityPrefix && e.startsWith("entity.") -> e.removePrefix("entity.")
        else -> return null
    }
    return rest.takeIf { FIELD.matches(it) }
}
