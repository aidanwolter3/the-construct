package org.genui.canvas.desktop.sim

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import org.genui.canvas.CanvasJson
import org.genui.canvas.LayoutApiException
import org.genui.canvas.UiNode

/**
 * In-memory layout catalog for [FakeEngineBridge] (DECISIONS D26/D27): the same JSON shapes as the pod's
 * `/v1/layouts` endpoints, append-only per-layout histories, one active layout. Not thread-safe: the bridge calls it
 * under its lock. The revision (the bridge's `layoutVersion`) is passed in by the caller.
 *
 * Fixture catalog (the active layout's newest version is always the bundle's layout, so the screen starts unchanged):
 * `home` -> `home.counter` (v1 retitled "Counter", v2 the bundle's layout; active) and `home.big_counter`;
 * `pixelfed_demo` (any bundle with `pixelfed://` entities) -> `pixelfed_demo.classic` (v1 the bundle's layout, v2
 * captions hidden, v3 a restore of v1; active), `pixelfed_demo.editorial` (media at the top) and
 * `pixelfed_demo.compact` (captions hidden). Variants come from [FakeLayoutRules] or a retitle, so they render.
 */
class FakeLayoutCatalog private constructor(val workspaceId: String) {
    class Version(val layout: UiNode, val source: String, val prompt: String?, val createdAt: Long)

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
        "hash" to JsonPrimitive(CanvasJson.encodeNode(v.layout).hashCode().toUInt().toString(16)),
        "bytes" to JsonPrimitive(CanvasJson.encodeNode(v.layout).length),
    ).also { m -> v.prompt?.let { m["prompt"] = JsonPrimitive(it) } }

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
        m["layout"] = CanvasJson.json.parseToJsonElement(CanvasJson.encodeNode(version(e, n).layout))
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
        append(layouts.getValue(activeId), layout, "agent", prompt)
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
        private var clock = 1_760_000_000L

        private fun now(): Long = ++clock

        private fun obj(vararg pairs: Pair<String, JsonElement>) = JsonObject(linkedMapOf(*pairs))

        private fun retitle(node: UiNode, title: String): UiNode {
            val o = CanvasJson.json.parseToJsonElement(CanvasJson.encodeNode(node)).jsonObject
            return CanvasJson.decodeNode(JsonObject(o + ("title" to JsonPrimitive(title))).toString())
        }

        private fun rule(prompt: String, node: UiNode): UiNode =
            (FakeLayoutRules.run(prompt, node) as? FakeLayoutRules.Outcome.Applied)?.layout ?: node

        /** The fixture catalog for a bundle whose layout is [base]: `pixelfed_demo` if [pixelfed], else `home`. */
        fun fixture(base: UiNode, pixelfed: Boolean): FakeLayoutCatalog = if (pixelfed) {
            FakeLayoutCatalog("pixelfed_demo").apply {
                val hidden = rule("hide captions", base)
                add(
                    "pixelfed_demo.classic", "Classic", "The built-in feed: author, media, actions, caption.", "feed",
                    Version(base, "seed", null, now()),
                    Version(hidden, "agent", "hide captions", now()),
                    Version(base, "restore", "restore v1", now()),
                )
                add(
                    "pixelfed_demo.editorial", "Editorial", "Media first, then the author and the caption.", "media",
                    Version(rule("media at the top", base), "seed", null, now()),
                )
                add(
                    "pixelfed_demo.compact", "Compact", "Media and author only; captions hidden.", "compact",
                    Version(hidden, "seed", null, now()),
                )
                activeId = "pixelfed_demo.classic"
            }
        } else {
            FakeLayoutCatalog("home").apply {
                add(
                    "home.counter", "Counter", "Title, count and buttons.", "counter",
                    Version(retitle(base, "Counter"), "seed", null, now()),
                    Version(base, "manual", "rename to the bundle's title", now()),
                )
                add(
                    "home.big_counter", "Big counter", "The count first, then the buttons.", "counter",
                    Version(retitle(base, "Big counter"), "seed", null, now()),
                )
                activeId = "home.counter"
            }
        }
    }
}
