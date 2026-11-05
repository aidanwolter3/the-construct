package org.theconstruct

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

// Text interpolation subset from docs/DECISIONS.md D4. Not a CEL evaluator.

// Every literal brace is escaped, inside character classes too: Android's ICU regex engine rejects a
// bare `}` that java.util.regex accepts (it crashed the app at class init;
// AndroidRegexSyntaxTest checks every shared Regex field for this).
private val PLACEHOLDER = Regex("""\$\{([^#\}]+)#([^\}]+)\}""")
private val FIELD = Regex("""[A-Za-z_][A-Za-z0-9_]*""")

/**
 * Renders a text `value_expr`:
 * - Inside an item template ([item] != null), an expression that is exactly `entity.<field>`
 *   or `$item.<field>` resolves to that item field ("" when missing), and `$item.<field>` tokens
 *   embedded in text are replaced (`"$item.name boosted"`; a missing field is left as written).
 * - Every `${<entity_uri>#<field>}` is replaced by that entity field's value, or "" when the
 *   entity or field is missing.
 * - Anything else renders as-is.
 *
 * Primitives render without JSON quotes; null renders as ""; arrays/objects render as JSON.
 */
fun interpolate(
    expr: String,
    entities: Map<String, JsonObject>,
    item: JsonObject? = null,
    render: (JsonElement) -> String = ::renderJsonValue,
): String {
    if (item != null) {
        itemFieldName(expr, allowEntityPrefix = true)?.let { field ->
            return item[field]?.let(render) ?: ""
        }
    }
    val text = bindItemTokens(expr, item, render)
    if (!text.contains("\${")) return text
    return PLACEHOLDER.replace(text) { m ->
        val (uri, field) = m.destructured
        entities[uri]?.get(field)?.let(render) ?: ""
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

// ---------------------------------------------------------------------------
// Item binding for references and intents declared inside item templates.
// ---------------------------------------------------------------------------

private val ITEM_TOKEN = Regex("""\${'$'}item\.([A-Za-z_][A-Za-z0-9_]*)""")

/**
 * Resolves a reference string: an exact `$item.<field>` binding resolves against [item]
 * (null if no item or the field is missing/null); anything else is a URI in which embedded
 * `$item.<field>` tokens are substituted (e.g. `pixelfed://posts/$item.id`, docs/layouts.md).
 */
fun resolveRef(ref: String, item: JsonObject?): String? {
    val field = itemFieldName(ref, allowEntityPrefix = false) ?: return bindItemTokens(ref, item)
    val v = item?.get(field) ?: return null
    return if (v is JsonPrimitive && v !is JsonNull) v.content else null
}

/**
 * Truthiness of a `core://Conditional` value: `$item.<field>` (or `entity.<field>`) resolves against [item]; anything
 * else is a literal. False for missing, null, false, 0, "", "0", "false" and an empty array; true otherwise.
 */
fun isTruthy(value: String, item: JsonObject?): Boolean {
    val field = itemFieldName(value, allowEntityPrefix = true)
    val v: JsonElement? = if (field != null) item?.get(field) else JsonPrimitive(value)
    return when (v) {
        null, JsonNull -> false
        is JsonArray -> v.isNotEmpty()
        is JsonObject -> true
        is JsonPrimitive -> {
            val c = v.content.trim()
            !(c.isEmpty() || c == "0" || c == "false" || c.toDoubleOrNull() == 0.0)
        }
    }
}

/**
 * Whether a `core://Conditional` shows (D48): [value] is `$form.<field>` (the form value, "" while unset) or as for
 * [isTruthy]. Without [equals], the value must be truthy; with it, the value as text (null and missing are "") must
 * equal [equals].
 */
fun conditionShows(
    value: String,
    rawEquals: String?,
    item: JsonObject?,
    form: Map<String, String>?,
    entities: Map<String, JsonObject> = emptyMap(),
): Boolean {
    // `equals` may name the item (`$item.id`), e.g. to mark the open page in a list of pages.
    val equals = rawEquals?.let { bindItemTokens(it, item) }
    // `${<uri>#<field>}` (an entity field, e.g. device data `${local://apps#count}`) reads as the field's text.
    if (value.contains("\${")) {
        val text = interpolate(value, entities, item)
        return if (equals == null) isTruthy(text, null) else text == equals
    }
    val formValue = if (value.startsWith("\$form.")) form?.get(value.removePrefix("\$form.")).orEmpty() else null
    if (equals == null) return if (formValue != null) isTruthy(formValue, null) else isTruthy(value, item)
    val text = formValue ?: run {
        val field = itemFieldName(value, allowEntityPrefix = true)
        if (field == null) {
            value
        } else {
            when (val v = item?.get(field)) {
                null, JsonNull -> ""
                is JsonPrimitive -> v.content
                else -> v.toString()
            }
        }
    }
    return text == equals
}

/** Resolves a [ListBinding] (DECISIONS D1) to a list of entity URIs. */
fun resolveList(refs: ListBinding, item: JsonObject?): List<String> = when (refs) {
    is ListBinding.Items -> refs.items
    is ListBinding.Expr -> {
        val field = itemFieldName(refs.expr, allowEntityPrefix = false)
        when (val v = field?.let { item?.get(it) }) {
            is JsonArray -> v.mapNotNull { (it as? JsonPrimitive)?.takeIf { p -> p !is JsonNull }?.content }
            is JsonNull, null -> emptyList()
            is JsonPrimitive -> listOf(v.content)
            else -> emptyList()
        }
    }
}

/**
 * Substitutes every `$item.<field>` token inside [s] with the item's field value.
 * Tokens whose field is missing or null are left untouched. [render] draws a value (default [renderJsonValue]).
 */
fun bindItemTokens(s: String, item: JsonObject?, render: (JsonElement) -> String = ::renderJsonValue): String {
    if (item == null || !s.contains("\$item.")) return s
    return ITEM_TOKEN.replace(s) { m ->
        val v = item[m.groupValues[1]]
        if (v == null || v is JsonNull) m.value else render(v)
    }
}

private fun bindJson(e: JsonElement, item: JsonObject): JsonElement = when (e) {
    is JsonObject -> JsonObject(e.mapValues { bindJson(it.value, item) })
    is JsonArray -> JsonArray(e.map { bindJson(it, item) })
    is JsonPrimitive -> if (e.isString) JsonPrimitive(bindItemTokens(e.content, item)) else e
}

/**
 * Binds an intent declared inside an item template to the current [item], replacing
 * `$item.<field>` tokens in its string fields and JSON payload so the engine receives
 * concrete URIs (e.g. `entity_uri: "pixelfed://posts/101"`). `value_expr` is left alone:
 * it is evaluated by the engine against the target entity. Unknown intents are bound
 * through their raw JSON (except the `intent` tag).
 */
fun bindIntent(intent: ActionIntent, item: JsonObject?): ActionIntent {
    if (item == null) return intent
    return when (intent) {
        is ActionIntent.Navigate -> intent.copy(targetUri = bindItemTokens(intent.targetUri, item))
        is ActionIntent.OptimisticMutate -> intent.copy(
            entityUri = bindItemTokens(intent.entityUri, item),
            endpoint = bindItemTokens(intent.endpoint, item),
            payload = bindJson(intent.payload, item),
        )
        is ActionIntent.AuthLogin -> intent.copy(instanceUrl = bindItemTokens(intent.instanceUrl, item))
        is ActionIntent.LoadData -> intent.copy(path = bindItemTokens(intent.path, item))
        is ActionIntent.OpenSession -> intent.copy(payload = bindJson(intent.payload, item))
        is ActionIntent.Create -> intent.copy(
            queryKey = bindItemTokens(intent.queryKey, item),
            entityUri = bindItemTokens(intent.entityUri, item),
            entity = bindJson(intent.entity, item),
            endpoint = bindItemTokens(intent.endpoint, item),
            payload = bindJson(intent.payload, item),
        )
        is ActionIntent.Sequence -> intent.copy(steps = intent.steps.map { bindIntent(it, item) })
        is ActionIntent.LocalSet -> intent.copy(entityUri = bindItemTokens(intent.entityUri, item))
        is ActionIntent.OpenLink -> intent.copy(url = bindItemTokens(intent.url, item))
        is ActionIntent.Device -> intent.copy(
            app = intent.app?.let { bindItemTokens(it, item) },
            id = intent.id?.let { bindItemTokens(it, item) },
            value = intent.value?.let { bindItemTokens(it, item) },
        )
        ActionIntent.NavigateBack -> intent
        is ActionIntent.Unknown -> intent.copy(
            raw = JsonObject(intent.raw.mapValues { (k, v) -> if (k == "intent") v else bindJson(v, item) })
        )
    }
}
