package org.genui.canvas.desktop.sim

import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.builtins.MapSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.longOrNull
import org.genui.canvas.ActionIntent
import org.genui.canvas.CanvasEngineBridge
import org.genui.canvas.CanvasJson
import org.genui.canvas.EngineSnapshot
import org.genui.canvas.UiNode

/**
 * In-memory [CanvasEngineBridge] for simulation mode and tests: no Rust, no server.
 *
 * Loads a hydration bundle (`{version, workspace, entities, queries}`, DECISIONS D2) and emits snapshots
 * shaped like the real engine's (D6: workspace JSON = the layout node, entities JSON = the
 * URI-keyed map, queries JSON = the bundle's queries; `seq` increases with every state change).
 * [authJson] is passed through unchanged (default signed out) until [logout] sets it to signed out.
 * `OptimisticMutate` intents are applied
 * with the D4 expression subset (including the M6 ternary); other intents are logged and ignored.
 * The primary field and every `also` field are evaluated against the PRE-mutation entity and
 * applied together; a rollback restores all of them.
 *
 * Outbox model (mirrors the M5 sync worker; DECISIONS D8, D13): every mutation is applied locally and queued; while
 * online the queue is flushed at once against an in-memory "server" copy of the entities, so the
 * pending count stays 0. While offline rows stay queued (`pendingActions` = queue size). [setOnline]
 * flips connectivity; going online flushes FIFO. The fake server rejects a `.../decrement` whose
 * result would be below 0 (like crates/server's 422): the row is dropped and its fields rolled back to
 * their previous values (the primary field falls back to `rollback_value` if its previous value was
 * null/absent), and later queued rows for the same entity touching any of those fields are re-applied
 * on top. No 5xx/retry simulation.
 *
 * Layout agent (M7, DECISIONS D23): `PromptAgent` runs [FakeLayoutRules] on the current layout (no server): a
 * recognized prompt replaces the layout, bumps [layoutVersion] and sets `agent_json` to done (provider
 * `fake-rules`); an unrecognized one (or any prompt while offline) sets it to error and leaves the layout alone.
 */
class FakeEngineBridge(
    bundleJson: String,
    online: Boolean = true,
    private var authJson: String = "{\"state\":\"signed_out\"}",
) : CanvasEngineBridge {
    /** [prevValues]: field -> value before this row was applied (null = absent). */
    private class Row(val intent: ActionIntent.OptimisticMutate, var prevValues: Map<String, JsonElement?>)

    private var workspace: UiNode
    private var agentJson = "{\"state\":\"idle\"}"

    /** Layout version: 1 for the bundle's layout, +1 per applied prompt. */
    var layoutVersion = 1
        private set
    private val queriesJson: String
    private val entities: MutableMap<String, JsonObject>
    private val server: MutableMap<String, JsonObject>
    private val outbox = ArrayDeque<Row>()
    private val listeners = mutableListOf<(EngineSnapshot) -> Unit>()
    private var isOnline = online
    private var seq = 0L
    private val lock = Any()

    init {
        val bundle = CanvasJson.decodeBundle(bundleJson)
        workspace = bundle.workspace
        queriesJson = CanvasJson.json.encodeToString(queriesSerializer, bundle.queries)
        entities = LinkedHashMap(bundle.entities)
        server = LinkedHashMap(bundle.entities)
    }

    override fun subscribe(onState: (EngineSnapshot) -> Unit) {
        val snapshot = synchronized(lock) {
            listeners += onState
            snapshotLocked()
        }
        onState(snapshot)
    }

    override fun logout() {
        println("[sim] logout")
        synchronized(lock) {
            authJson = "{\"state\":\"signed_out\"}"
            seq++
        }
        emit()
    }

    override fun dispatch(intentJson: String) {
        println("[sim] dispatch: $intentJson")
        val intent = CanvasJson.decodeIntent(intentJson)
        if (intent is ActionIntent.PromptAgent) {
            promptAgent(intent.promptText)
            return
        }
        if (intent !is ActionIntent.OptimisticMutate) {
            println("[sim] ignoring non-mutation intent")
            return
        }
        synchronized(lock) {
            val entity = entities[intent.entityUri]
            if (entity == null) {
                println("[sim] unknown entity ${intent.entityUri}")
                return
            }
            val updates = try {
                evalUpdates(intent, entity)
            } catch (e: RuntimeException) { // IllegalArgumentException or ArithmeticException (overflow)
                println("[sim] ${e.message}; nothing changed")
                return
            }
            entities[intent.entityUri] = JsonObject(entity + updates)
            outbox.addLast(Row(intent, updates.keys.associateWith { entity[it] }))
            println("[sim] ${intent.entityUri} ${updates.entries.joinToString { "#${it.key} = ${it.value}" }} (queued; ${outbox.size} pending)")
            if (isOnline) flushLocked()
            seq++
        }
        emit()
    }

    private fun promptAgent(prompt: String) {
        synchronized(lock) {
            agentJson = agentJson(state = "running")
            seq++
        }
        emit()
        synchronized(lock) {
            val result = if (isOnline) FakeLayoutRules.apply(prompt, workspace) else null
            agentJson = when {
                !isOnline -> agentJson(state = "error", message = "offline: the layout agent needs a connection")
                result == null -> agentJson(state = "error", message = FakeLayoutRules.UNSUPPORTED)
                else -> {
                    workspace = result
                    layoutVersion++
                    agentJson(state = "done", provider = FakeLayoutRules.PROVIDER, layoutVersion = layoutVersion)
                }
            }
            println("[sim] agent prompt '$prompt' -> $agentJson")
            seq++
        }
        emit()
    }

    fun setOnline(online: Boolean) {
        synchronized(lock) {
            isOnline = online
            if (online) flushLocked()
            seq++
        }
        emit()
    }

    /** Current value of [field] on entity [uri], for tests. */
    fun entityField(uri: String, field: String): JsonElement? = synchronized(lock) { entities[uri]?.get(field) }

    /** Value of [field] on the fake server's copy of entity [uri], for tests. */
    fun serverField(uri: String, field: String): JsonElement? = synchronized(lock) { server[uri]?.get(field) }

    /** Number of queued (unsent) mutations. */
    val pendingActions: Int get() = synchronized(lock) { outbox.size }

    /** Sends queued rows FIFO to the fake server. */
    private fun flushLocked() {
        while (outbox.isNotEmpty()) {
            val row = outbox.removeFirst()
            val i = row.intent
            val serverEntity = server[i.entityUri] ?: JsonObject(emptyMap())
            val result = try {
                evalUpdates(i, serverEntity)
            } catch (e: RuntimeException) {
                null
            }
            val belowZero = i.endpoint.endsWith("/decrement") &&
                ((result?.get(i.field) as? JsonPrimitive)?.longOrNull ?: 0L) < 0
            if (result != null && !belowZero) {
                server[i.entityUri] = JsonObject(serverEntity + result)
                println("[sim] sent ${i.endpoint}: server ${i.entityUri} = $result")
                continue
            }
            println("[sim] server rejected ${i.endpoint} (${if (belowZero) "count_below_zero" else "bad expression"}); rolling back")
            rollbackLocked(row)
        }
    }

    /**
     * Restores every field [row] changed to its previous value, then re-applies (re-evaluating all
     * of their fields) later rows for the same entity that touch any of those fields.
     */
    private fun rollbackLocked(row: Row) {
        val i = row.intent
        var entity = entities[i.entityUri] ?: JsonObject(emptyMap())
        val restored = LinkedHashMap<String, JsonElement?>(row.prevValues)
        if (restored[i.field] == null || restored[i.field] is JsonNull) restored[i.field] = i.rollbackValue
        entity = withFields(entity, restored)
        val touched = restored.keys.toMutableSet()
        for (later in outbox) {
            val li = later.intent
            if (li.entityUri != i.entityUri) continue
            val laterFields = listOf(li.field) + li.also.map { it.field }
            if (laterFields.none { it in touched }) continue
            val updates = try {
                evalUpdates(li, entity)
            } catch (e: RuntimeException) {
                continue
            }
            later.prevValues = updates.keys.associateWith { entity[it] }
            entity = JsonObject(entity + updates)
            touched += updates.keys
        }
        entities[i.entityUri] = entity
        println("[sim] ${i.entityUri} rolled back to $restored (now ${touched.associateWith { entity[it] }})")
    }

    private fun withFields(entity: JsonObject, fields: Map<String, JsonElement?>): JsonObject {
        val m = LinkedHashMap(entity)
        for ((k, v) in fields) if (v == null) m.remove(k) else m[k] = v
        return JsonObject(m)
    }

    private fun snapshotLocked(): EngineSnapshot = EngineSnapshot(
        workspaceJson = CanvasJson.encodeNode(workspace),
        entitiesJson = CanvasJson.json.encodeToString(JsonObject.serializer(), JsonObject(entities)),
        isOnline = isOnline,
        seq = seq,
        pendingActions = outbox.size,
        queriesJson = queriesJson,
        authJson = authJson,
        agentJson = agentJson,
    )

    private fun emit() {
        val (snapshot, targets) = synchronized(lock) { snapshotLocked() to listeners.toList() }
        targets.forEach { it(snapshot) }
    }

    companion object {
        private fun agentJson(state: String, message: String? = null, provider: String? = null, layoutVersion: Int? = null): String =
            JsonObject(
                buildMap {
                    put("state", JsonPrimitive(state))
                    message?.let { put("message", JsonPrimitive(it)) }
                    provider?.let { put("provider", JsonPrimitive(it)) }
                    layoutVersion?.let { put("layout_version", JsonPrimitive(it)) }
                },
            ).toString()

        private val queriesSerializer = MapSerializer(String.serializer(), ListSerializer(String.serializer()))
        private val FIELD = Regex("""entity\.([A-Za-z0-9_]+)""")

        /**
         * Evaluates the primary field and every `also` field of [intent] against the same
         * PRE-mutation [entity]. Returns field -> new value (primary first). Throws like
         * [evalMutation].
         */
        fun evalUpdates(intent: ActionIntent.OptimisticMutate, entity: JsonObject): Map<String, JsonElement> {
            val out = LinkedHashMap<String, JsonElement>()
            out[intent.field] = evalMutation(intent.valueExpr, entity)
            for (u in intent.also) out[u.field] = evalMutation(u.valueExpr, entity)
            return out
        }
        private val BINARY = Regex("""entity\.([A-Za-z0-9_]+)\s*([+-])\s*(\d+)""")

        /**
         * D4 mutation subset, mirroring crates/client-core/src/expr.rs: a JSON literal,
         * `entity.f`, `!entity.f` (bool), `entity.f + N` / `entity.f - N` (integer; missing = 0),
         * and `<cond> ? <expr> : <expr>` with cond `entity.f` / `!entity.f` (a bool field) and
         * non-ternary branches. Throws [IllegalArgumentException] for anything else.
         */
        fun evalMutation(expr: String, entity: JsonObject): JsonElement {
            val e = expr.trim()
            val q = e.indexOf('?')
            if (q >= 0) {
                val cond = e.substring(0, q).trim()
                val negate = cond.startsWith("!")
                val condField = FIELD.matchEntire(if (negate) cond.substring(1).trim() else cond)?.groupValues?.get(1)
                if (condField != null) {
                    val (thenBranch, elseBranch) = splitTopLevelColon(e.substring(q + 1))
                        ?: throw IllegalArgumentException("unsupported expression: $e")
                    val b = when (val v = entity[condField]) {
                        null -> throw IllegalArgumentException("missing field: $condField")
                        else -> (v as? JsonPrimitive)?.takeIf { !it.isString }?.booleanOrNull
                            ?: throw IllegalArgumentException("field $condField is not a bool")
                    }
                    return evalSimple(if (b != negate) thenBranch else elseBranch, entity)
                }
            }
            return evalSimple(e, entity)
        }

        /** First `:` outside JSON strings/arrays/objects; null if absent, a side is empty, or a nested `?`. */
        private fun splitTopLevelColon(s: String): Pair<String, String>? {
            var depth = 0
            var inStr = false
            var escaped = false
            var split = -1
            var hasQuestion = false
            for ((i, c) in s.withIndex()) {
                if (inStr) {
                    when {
                        escaped -> escaped = false
                        c == '\\' -> escaped = true
                        c == '"' -> inStr = false
                    }
                    continue
                }
                when (c) {
                    '"' -> inStr = true
                    '[', '{' -> depth++
                    ']', '}' -> depth--
                    ':' -> if (depth == 0 && split < 0) split = i
                    '?' -> if (depth == 0) hasQuestion = true
                }
            }
            if (split < 0 || hasQuestion) return null
            val a = s.substring(0, split).trim()
            val b = s.substring(split + 1).trim()
            return if (a.isEmpty() || b.isEmpty()) null else a to b
        }

        private fun evalSimple(expr: String, entity: JsonObject): JsonElement {
            val e = expr.trim()
            if (e.startsWith("!")) {
                val field = FIELD.matchEntire(e.substring(1).trim())?.groupValues?.get(1)
                    ?: throw IllegalArgumentException("unsupported expression: $e")
                val b = (entity[field] as? JsonPrimitive)?.takeIf { !it.isString }?.booleanOrNull
                    ?: throw IllegalArgumentException("field $field is missing or not a bool")
                return JsonPrimitive(!b)
            }
            if (e.startsWith("entity.")) {
                FIELD.matchEntire(e)?.let { m ->
                    val field = m.groupValues[1]
                    return entity[field] ?: throw IllegalArgumentException("missing field: $field")
                }
                val m = BINARY.matchEntire(e) ?: throw IllegalArgumentException("unsupported expression: $e")
                val (field, op, n) = m.destructured
                val current = when (val v = entity[field]) {
                    null, JsonNull -> 0L
                    else -> (v as? JsonPrimitive)?.takeIf { !it.isString }?.longOrNull
                        ?: throw IllegalArgumentException("field $field is not an integer")
                }
                val delta = n.toLong()
                return JsonPrimitive(if (op == "+") Math.addExact(current, delta) else Math.subtractExact(current, delta))
            }
            val literal = try {
                CanvasJson.json.parseToJsonElement(e)
            } catch (t: Exception) {
                throw IllegalArgumentException("unsupported expression: $e")
            }
            // parseToJsonElement accepts unquoted tokens (`foo.count`, `size(x)`, `NaN`) as
            // non-string primitives; serde_json (and so expr.rs) rejects them.
            if (!isStrictJson(literal)) throw IllegalArgumentException("unsupported expression: $e")
            return literal
        }

        private val JSON_NUMBER = Regex("""-?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?""")

        private fun isStrictJson(v: JsonElement): Boolean = when (v) {
            is JsonObject -> v.values.all(::isStrictJson)
            is kotlinx.serialization.json.JsonArray -> v.all(::isStrictJson)
            is JsonPrimitive -> v.isString || v is JsonNull || v.content == "true" || v.content == "false" ||
                JSON_NUMBER.matches(v.content)
        }
    }
}
