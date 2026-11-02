package org.theconstruct.desktop.sim

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import org.theconstruct.ConstructJson
import org.theconstruct.LayoutApiException
import org.theconstruct.UiNode

/**
 * In-memory layout catalog for [FakeEngineBridge] (DECISIONS D26/D27/D30): the same JSON shapes as the pod's
 * `/v1/layouts` endpoints, append-only per-layout histories, one active layout. Not thread-safe: the bridge calls it
 * under its lock. The revision (the bridge's `layoutVersion`) is passed in by the caller.
 *
 * Fixture catalog: the pod's built-ins with the pod's titles, descriptions and preview hints, loaded from the repo's
 * `fixtures/layouts/<workspace>.<name>.json` (the files the server `include_str!`s; found by walking up from the
 * working directory like the bundles); if a file is missing, a [FakeLayoutRules] variant stands in so it still
 * renders. The active layout's newest version is always the bundle's layout, so the screen starts unchanged.
 * - `home`: `home.counter` (v1 seed, v2 an AI prompt adding a weather widget, v3 a rollback to v1; active) and
 *   `home.big_counter`.
 * - `pixelfed_demo` (a bundle with `pixelfed://` entities; `pixelfed_home` when the bundle's root id is
 *   `pixelfed_home`): `<ws>.classic` (v1 seed, v2 AI "Hide post captions", v3 AI "Add a weather widget", v4 a
 *   rollback to v1; active), `<ws>.editorial`, `<ws>.compact`, `<ws>.minimal`.
 */
class FakeLayoutCatalog private constructor(val workspaceId: String) {
    class Version(
        val layout: UiNode,
        val source: String,
        val prompt: String?,
        val createdAt: Long,
        val provider: String? = null,
    )

    private class Entry(val id: String, val title: String, val description: String, val previewHint: String) {
        val versions = mutableListOf<Version>()
    }

    private val layouts = LinkedHashMap<String, Entry>()
    var activeId: String = ""
        private set

    /** Active layout id and its per-layout version (the newest; activation always shows the newest). */
    val activeVersion: Int get() = layouts.getValue(activeId).versions.size

    val activeLayout: UiNode get() = layouts.getValue(activeId).versions.last().layout

    /** `{"layout_id","layout_version","revision"}` for the snapshot. */
    fun layoutJson(revision: Int): String = obj(
        "layout_id" to JsonPrimitive(activeId),
        "layout_version" to JsonPrimitive(activeVersion),
        "revision" to JsonPrimitive(revision),
    ).toString()

    private fun add(id: String, title: String, description: String, hint: String, vararg versions: Version) {
        layouts[id] = Entry(id, title, description, hint).also { it.versions += versions }
    }

    private fun entry(id: String): Entry =
        layouts[id] ?: throw LayoutApiException("That layout doesn't exist.", LayoutApiException.Kind.NotFound)

    private fun version(e: Entry, n: Int): Version = e.versions.getOrNull(n - 1)
        ?: throw LayoutApiException("That version doesn't exist.", LayoutApiException.Kind.NotFound)

    private fun layoutObject(e: Entry): JsonObject {
        val m = linkedMapOf<String, JsonElement>(
            "id" to JsonPrimitive(e.id),
            "workspace_id" to JsonPrimitive(workspaceId),
            "title" to JsonPrimitive(e.title),
            "description" to JsonPrimitive(e.description),
            "preview_hint" to JsonPrimitive(e.previewHint),
            "builtin" to JsonPrimitive(true),
            "current_version" to JsonPrimitive(e.versions.size),
            "created_at" to JsonPrimitive(e.versions.first().createdAt),
            "updated_at" to JsonPrimitive(e.versions.last().createdAt),
            "active" to JsonPrimitive(e.id == activeId),
        )
        if (e.id == activeId) m["active_version"] = JsonPrimitive(e.versions.size)
        return JsonObject(m)
    }

    private fun versionObject(n: Int, v: Version): MutableMap<String, JsonElement> = linkedMapOf<String, JsonElement>(
        "version" to JsonPrimitive(n),
        "created_at" to JsonPrimitive(v.createdAt),
        "source" to JsonPrimitive(v.source),
        "hash" to JsonPrimitive(ConstructJson.encodeNode(v.layout).hashCode().toUInt().toString(16)),
        "bytes" to JsonPrimitive(ConstructJson.encodeNode(v.layout).length),
    ).also { m ->
        v.prompt?.let { m["prompt"] = JsonPrimitive(it) }
        v.provider?.let { m["provider"] = JsonPrimitive(it) }
    }

    fun listJson(revision: Int): String = obj(
        "layouts" to JsonArray(layouts.values.map(::layoutObject)),
        "workspaces" to JsonArray(
            listOf(
                obj(
                    "workspace_id" to JsonPrimitive(workspaceId),
                    "revision" to JsonPrimitive(revision),
                    "layout_id" to JsonPrimitive(activeId),
                    "layout_version" to JsonPrimitive(activeVersion),
                ),
            ),
        ),
        "workspace_id" to JsonPrimitive(workspaceId),
        "workspace_revision" to JsonPrimitive(revision),
    ).toString()

    fun versionsJson(id: String, before: Int?, limit: Int?): String {
        val e = entry(id)
        val lim = (limit ?: 100).coerceIn(1, 1000)
        val newestFirst = e.versions.withIndex().reversed().filter { before == null || it.index + 1 < before }
        val page = newestFirst.take(lim)
        val m = linkedMapOf<String, JsonElement>(
            "layout_id" to JsonPrimitive(id),
            "workspace_id" to JsonPrimitive(workspaceId),
            "current_version" to JsonPrimitive(e.versions.size),
            "versions" to JsonArray(page.map { JsonObject(versionObject(it.index + 1, it.value)) }),
        )
        if (newestFirst.size > page.size) m["next_before"] = JsonPrimitive(page.last().index + 1)
        return JsonObject(m).toString()
    }

    fun versionJson(id: String, n: Int): String {
        val e = entry(id)
        val m = versionObject(n, version(e, n))
        m["layout_id"] = JsonPrimitive(id)
        m["workspace_id"] = JsonPrimitive(workspaceId)
        m["layout"] = ConstructJson.json.parseToJsonElement(ConstructJson.encodeNode(version(e, n).layout))
        return JsonObject(m).toString()
    }

    /** Appends [layout] to [e] unless it equals the newest; true if appended. */
    private fun append(e: Entry, layout: UiNode, source: String, prompt: String?): Boolean {
        if (e.versions.last().layout == layout) return false
        e.versions += Version(layout, source, prompt, now())
        return true
    }

    /** An agent change to the active layout (the bridge already bumped the revision). */
    fun appendActive(layout: UiNode, prompt: String) {
        val e = layouts.getValue(activeId)
        if (e.versions.last().layout != layout) e.versions += Version(layout, "agent", prompt, now(), FakeLayoutRules.PROVIDER)
    }

    /** Result of a write: the response body and whether the screen (active layout) changed. */
    class Write(val body: String, val screenChanged: Boolean)

    /** `POST /activate`; [nextRevision] is what the revision becomes if the screen changes. */
    fun activate(id: String, version: Int?, nextRevision: Int, currentRevision: Int): Write {
        val e = entry(id)
        var restored: Int? = null
        if (version != null) {
            val v = version(e, version)
            if (append(e, v.layout, "restore", "restore v$version")) restored = version
        }
        val changed = id != activeId || restored != null
        activeId = id
        val m = linkedMapOf<String, JsonElement>(
            "workspace_id" to JsonPrimitive(workspaceId),
            "revision" to JsonPrimitive(if (changed) nextRevision else currentRevision),
            "layout_id" to JsonPrimitive(id),
            "layout_version" to JsonPrimitive(e.versions.size),
            "unchanged" to JsonPrimitive(!changed),
        )
        restored?.let { m["restored_version"] = JsonPrimitive(it) }
        return Write(JsonObject(m).toString(), changed)
    }

    /** `POST /restore`. */
    fun restore(id: String, version: Int, nextRevision: Int, currentRevision: Int): Write {
        val e = entry(id)
        val appended = append(e, version(e, version).layout, "restore", "restore v$version")
        val active = id == activeId
        val m = linkedMapOf<String, JsonElement>(
            "layout_id" to JsonPrimitive(id),
            "workspace_id" to JsonPrimitive(workspaceId),
            "version" to JsonPrimitive(e.versions.size),
            "restored_from" to JsonPrimitive(version),
            "unchanged" to JsonPrimitive(!appended),
            "active" to JsonPrimitive(active),
        )
        if (active) m["revision"] = JsonPrimitive(if (appended) nextRevision else currentRevision)
        return Write(JsonObject(m).toString(), active && appended)
    }

    companion object {
        private fun now(): Long = System.currentTimeMillis() / 1000

        private const val HOUR = 3_600L
        private const val DAY = 86_400L

        private fun obj(vararg pairs: Pair<String, JsonElement>) = JsonObject(linkedMapOf(*pairs))

        private fun rule(prompt: String, node: UiNode): UiNode =
            (FakeLayoutRules.run(prompt, node) as? FakeLayoutRules.Outcome.Applied)?.layout ?: node

        /** `fixtures/layouts/<id>.json` (the pod's built-in), or null when the repo's fixtures are not reachable. */
        private fun builtin(id: String): UiNode? = findFixture("layouts/$id.json")?.let {
            try {
                ConstructJson.decodeNode(it.readText())
            } catch (e: Exception) {
                println("[sim] layouts: cannot read ${it.path}: $e")
                null
            }
        }

        /** The fixture catalog for a bundle whose layout is [base]: a Pixelfed workspace if [pixelfed], else `home`. */
        fun fixture(base: UiNode, pixelfed: Boolean): FakeLayoutCatalog {
            val t = now()
            return if (pixelfed) {
                val ws = if ((base as? UiNode.Workspace)?.id == "pixelfed_home") "pixelfed_home" else "pixelfed_demo"
                FakeLayoutCatalog(ws).apply {
                    val hidden = rule("hide captions", base)
                    add(
                        "$ws.classic", "Classic feed", "Author, square photo, like/bookmark/comment bar, caption.", "feed-classic",
                        Version(base, "seed", null, t - 6 * DAY),
                        Version(hidden, "agent", "Hide post captions", t - 3 * DAY, "rules"),
                        Version(rule("add a weather widget", hidden), "agent", "Add a weather widget", t - 26 * HOUR, "rules"),
                        Version(base, "rollback", "rollback to v1", t - 2 * HOUR, "rollback"),
                    )
                    add(
                        "$ws.editorial", "Editorial journal",
                        "Large portrait photo first, then a byline and the caption; no action bar.", "feed-editorial",
                        Version(builtin("$ws.editorial") ?: rule("media at the top", base), "seed", null, t - 6 * DAY),
                    )
                    add(
                        "$ws.compact", "Compact", "Smaller inset wide photos and small captions for a denser list.", "feed-compact",
                        Version(builtin("$ws.compact") ?: hidden, "seed", null, t - 6 * DAY),
                    )
                    add(
                        "$ws.minimal", "Minimal", "Author, photo and caption only; no action bar.", "feed-minimal",
                        Version(builtin("$ws.minimal") ?: hidden, "seed", null, t - 6 * DAY),
                    )
                    activeId = "$ws.classic"
                }
            } else {
                FakeLayoutCatalog("home").apply {
                    add(
                        "home.counter", "Counter", "Title, count and the increment / decrement / reset buttons.", "counter",
                        Version(base, "seed", null, t - 5 * DAY),
                        Version(rule("add a weather widget", base), "agent", "Add a weather widget", t - 4 * HOUR, "rules"),
                        Version(base, "rollback", "rollback to v1", t - 40 * 60, "rollback"),
                    )
                    add(
                        "home.big_counter", "Big counter",
                        "The count first, the buttons below it and a small title at the bottom.", "counter-big",
                        Version(builtin("home.big_counter") ?: base, "seed", null, t - 5 * DAY),
                    )
                    activeId = "home.counter"
                }
            }
        }
    }
}
