package org.genui.canvas.desktop.sim

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.longOrNull
import org.genui.canvas.ActionIntent
import org.genui.canvas.CanvasEngineBridge
import org.genui.canvas.CanvasJson
import org.genui.canvas.UiNode

/**
 * In-memory [CanvasEngineBridge] for simulation mode and tests: no Rust, no server.
 *
 * Loads a hydration bundle (`{version, workspace, entities}`, DECISIONS D2) and emits snapshots
 * shaped like the real engine's (D6: workspace JSON = the layout node, entities JSON = the
 * URI-keyed map). `OptimisticMutate` intents are applied with the D4 expression subset; other
 * intents are logged and ignored. [setOnline] flips connectivity and re-emits.
 */
class FakeEngineBridge(bundleJson: String, online: Boolean = true) : CanvasEngineBridge {
    private val workspace: UiNode
    private val entities: MutableMap<String, JsonObject>
    private val listeners = mutableListOf<(String, String, Boolean) -> Unit>()
    private var isOnline = online
    private val lock = Any()

    init {
        val bundle = CanvasJson.decodeBundle(bundleJson)
        workspace = bundle.workspace
        entities = LinkedHashMap(bundle.entities)
    }

    override fun subscribe(onState: (workspaceJson: String, entitiesJson: String, isOnline: Boolean) -> Unit) {
        val snapshot = synchronized(lock) {
            listeners += onState
            snapshotLocked()
        }
        onState(snapshot.first, snapshot.second, snapshot.third)
    }

    override fun dispatch(intentJson: String) {
        println("[sim] dispatch: $intentJson")
        val intent = CanvasJson.decodeIntent(intentJson)
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
            val value = try {
                evalMutation(intent.valueExpr, entity)
            } catch (e: RuntimeException) { // IllegalArgumentException or ArithmeticException (overflow)
                println("[sim] ${e.message}; nothing changed")
                return
            }
            entities[intent.entityUri] = JsonObject(entity + (intent.field to value))
            println("[sim] ${intent.entityUri}#${intent.field} = $value")
        }
        emit()
    }

    fun setOnline(online: Boolean) {
        synchronized(lock) { isOnline = online }
        emit()
    }

    /** Current value of [field] on entity [uri], for tests. */
    fun entityField(uri: String, field: String): JsonElement? = synchronized(lock) { entities[uri]?.get(field) }

    private fun snapshotLocked(): Triple<String, String, Boolean> = Triple(
        CanvasJson.encodeNode(workspace),
        CanvasJson.json.encodeToString(JsonObject.serializer(), JsonObject(entities)),
        isOnline,
    )

    private fun emit() {
        val (snapshot, targets) = synchronized(lock) { snapshotLocked() to listeners.toList() }
        targets.forEach { it(snapshot.first, snapshot.second, snapshot.third) }
    }

    companion object {
        private val FIELD = Regex("""entity\.([A-Za-z0-9_]+)""")
        private val BINARY = Regex("""entity\.([A-Za-z0-9_]+)\s*([+-])\s*(\d+)""")

        /**
         * D4 mutation subset, mirroring crates/client-core/src/expr.rs: a JSON literal,
         * `entity.f`, `!entity.f` (bool), `entity.f + N` / `entity.f - N` (integer; missing = 0).
         * Throws [IllegalArgumentException] for anything else.
         */
        fun evalMutation(expr: String, entity: JsonObject): JsonElement {
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
            return try {
                CanvasJson.json.parseToJsonElement(e)
            } catch (t: Exception) {
                throw IllegalArgumentException("unsupported expression: $e")
            }
        }
    }
}
