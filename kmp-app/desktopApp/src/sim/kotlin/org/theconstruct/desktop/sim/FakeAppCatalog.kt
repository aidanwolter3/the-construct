package org.theconstruct.desktop.sim

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import org.theconstruct.ConstructJson
import org.theconstruct.LayoutApiException
import org.theconstruct.Orientation
import org.theconstruct.SlotSpec
import org.theconstruct.TextStyle
import org.theconstruct.UiNode
import java.io.File
import java.time.Instant

/**
 * In-memory stand-in for the pod's apps and their linear, append-only version history (`GET /v1/apps`,
 * `/v1/apps/{id}/versions[/{n}]`), with the pod's JSON shapes. Shared by every [FakeEngineBridge] of a simulator run
 * (an `open-app` creates a new bridge on the same catalog). Thread-safe (synchronized).
 *
 * Fixture catalog ([fixture]), as with `CONSTRUCT_SHOW_DEV_APPS=1`: `pixelfed_home` "Pixelfed" (v1 caption above the
 * photo and no action bar, v2 photo first and still no action bar, v3 the bundle's layout), `pixelfed_demo` "Pixelfed
 * demo" (v1 no action bar, v2 the bundle's) and `home` "Counter" (v1 an Increment button only, v2 the bundle's), each
 * from its default fixture bundle; the newest version is always the bundle's layout, so the screen starts unchanged.
 * [publish] appends a visibly changed copy of the newest version (the developer's "deploy").
 */
class FakeAppCatalog(private val clock: () -> Long = { System.currentTimeMillis() / 1000 }) {
    class Version(val layout: UiNode, val note: String?, val createdAt: Long, val source: String = "file")

    private class App(val id: String, val title: String, val icon: String) {
        val versions = mutableListOf<Version>()
    }

    private val apps = LinkedHashMap<String, App>()

    @Synchronized
    fun add(id: String, title: String, icon: String, vararg versions: Version) {
        apps[id] = App(id, title, icon).also { it.versions += versions }
    }

    @Synchronized
    fun has(id: String): Boolean = id in apps

    @Synchronized
    fun latestVersion(id: String): Long? = apps[id]?.versions?.size?.toLong()?.takeIf { it > 0 }

    @Synchronized
    fun latestLayout(id: String): UiNode? = apps[id]?.versions?.lastOrNull()?.layout

    @Synchronized
    fun ids(): List<String> = apps.keys.toList()

    @Synchronized
    fun title(id: String): String? = apps[id]?.title

    private fun app(id: String): App {
        if (!ID.matches(id)) throw LayoutApiException("That isn't a valid app id.", LayoutApiException.Kind.Server, 400)
        return apps[id] ?: throw LayoutApiException("That app doesn't exist.", LayoutApiException.Kind.NotFound, 404)
    }

    @Synchronized
    fun appsJson(): String = obj(
        "apps" to JsonArray(
            apps.values.map { a ->
                obj(
                    "id" to JsonPrimitive(a.id),
                    "title" to JsonPrimitive(a.title),
                    "icon" to JsonPrimitive(a.icon),
                    "latest_version" to JsonPrimitive(a.versions.size),
                    "updated_at" to JsonPrimitive(rfc3339(a.versions.last().createdAt)),
                )
            },
        ),
    ).toString()

    private fun versionObject(n: Int, v: Version): LinkedHashMap<String, JsonElement> = linkedMapOf<String, JsonElement>(
        "version" to JsonPrimitive(n),
        "created_at" to JsonPrimitive(rfc3339(v.createdAt)),
        "source" to JsonPrimitive(v.source),
    ).also { m -> v.note?.let { m["note"] = JsonPrimitive(it) } }

    @Synchronized
    fun versionsJson(id: String, before: Long?, limit: Int?): String {
        val a = app(id)
        if (limit != null && limit !in 1..100 || before != null && before < 1) {
            throw LayoutApiException("The pod rejected the request.", LayoutApiException.Kind.Server, 400)
        }
        val lim = limit ?: 20
        val newestFirst = a.versions.withIndex().reversed().filter { before == null || it.index + 1 < before }
        val page = newestFirst.take(lim)
        val m = linkedMapOf<String, JsonElement>(
            "app_id" to JsonPrimitive(id),
            "latest_version" to JsonPrimitive(a.versions.size),
            "versions" to JsonArray(page.map { JsonObject(versionObject(it.index + 1, it.value)) }),
        )
        if (newestFirst.size > page.size) m["next_before"] = JsonPrimitive(page.last().index + 1)
        return JsonObject(m).toString()
    }

    @Synchronized
    fun versionJson(id: String, n: Long): String {
        val a = app(id)
        val v = a.versions.getOrNull((n - 1).toInt().takeIf { n in 1..a.versions.size } ?: -1)
            ?: throw LayoutApiException("That version doesn't exist.", LayoutApiException.Kind.NotFound, 404)
        val m = linkedMapOf<String, JsonElement>("app_id" to JsonPrimitive(id))
        m += versionObject(n.toInt(), v)
        m["layout"] = ConstructJson.json.parseToJsonElement(ConstructJson.encodeNode(v.layout))
        return JsonObject(m).toString()
    }

    /**
     * Appends a new newest version of [id]: the newest layout with a "What's new: [note]" title above its slots (the
     * slot is replaced on later publishes). Returns the new version number.
     */
    @Synchronized
    fun publish(id: String, note: String): Long {
        val a = app(id)
        val layout = withBanner(a.versions.last().layout, "What's new: $note")
        a.versions += Version(layout, note, clock())
        return a.versions.size.toLong()
    }

    companion object {
        private val ID = Regex("[a-z0-9_]{1,64}")
        private const val HOUR = 3_600L
        private const val DAY = 86_400L
        const val BANNER_SLOT = "whats_new"

        private fun obj(vararg pairs: Pair<String, JsonElement>) = JsonObject(linkedMapOf(*pairs))

        fun rfc3339(epochSeconds: Long): String = Instant.ofEpochSecond(epochSeconds).toString()

        /** The app a bundle belongs to when not given: its root id, or `pixelfed_demo` for the one-post demo bundle. */
        fun appIdFor(workspace: UiNode, pixelfed: Boolean): String {
            val id = (workspace as? UiNode.Workspace)?.id ?: "home"
            return when {
                id in KNOWN -> id
                pixelfed -> "pixelfed_demo"
                else -> id
            }
        }

        private val KNOWN = listOf("pixelfed_home", "pixelfed_demo", "home")

        /**
         * The fixture catalog: [appId]'s history ends with [base] (the running bundle's layout); the other known apps
         * come from their default fixture bundles when found ([findFixture]).
         */
        fun fixture(appId: String, base: UiNode, from: File = File("").absoluteFile): FakeAppCatalog {
            val catalog = FakeAppCatalog()
            for (id in KNOWN) {
                val layout = if (id == appId) base else defaultLayout(id, from) ?: continue
                catalog.addFixtureApp(id, layout)
            }
            if (!catalog.has(appId)) {
                catalog.add(
                    appId, (base as? UiNode.Workspace)?.title ?: appId, "",
                    Version(base, "First version", catalog.clock() - 2 * DAY),
                )
            }
            return catalog
        }

        private fun defaultLayout(id: String, from: File): UiNode? = try {
            ConstructJson.decodeBundle(defaultFixture(id, from).readText()).workspace
        } catch (e: Exception) {
            null
        }

        private fun FakeAppCatalog.addFixtureApp(id: String, base: UiNode) {
            val t = clock()
            when (id) {
                "pixelfed_home" -> add(
                    id, "Pixelfed", "photo",
                    Version(captionFirst(base), "First feed: caption above the photo", t - 9 * DAY),
                    Version(withoutActionBar(base), "Photos first", t - 2 * DAY),
                    Version(base, "Like, bookmark and comment bar", t - 3 * HOUR),
                )
                "pixelfed_demo" -> add(
                    id, "Pixelfed demo", "photo",
                    Version(withoutActionBar(base), "One-post demo", t - 5 * DAY),
                    Version(base, "Action bar", t - 26 * HOUR),
                )
                else -> add(
                    id, "Counter", "counter",
                    Version(incrementOnly(base), "Increment button", t - 6 * DAY),
                    Version(base, "Decrement and reset buttons", t - 40 * 60),
                )
            }
        }

        private val EMPTY = UiNode.Container(Orientation.Vertical, 0, emptyList())

        /** Every list item template in [node] that is a [UiNode.Column] (a post card) transformed by [f]. */
        private fun mapCards(node: UiNode, f: (UiNode.Column) -> UiNode.Column): UiNode = when (node) {
            is UiNode.Workspace -> node.copy(slots = node.slots.map { it.copy(content = mapCards(it.content, f)) })
            is UiNode.Container -> node.copy(children = node.children.map { mapCards(it, f) })
            is UiNode.ItemList -> (node.itemTemplate as? UiNode.Column)?.let { node.copy(itemTemplate = f(it)) } ?: node
            else -> node
        }

        private fun isActionBar(n: UiNode) = n is UiNode.Row && n.children.any { it is UiNode.IconButton }

        fun withoutActionBar(base: UiNode): UiNode = mapCards(base) { c -> c.copy(children = c.children.filterNot(::isActionBar)) }

        /** The caption where the photo was (right under the author) and the photo after it; no action bar. */
        fun captionFirst(base: UiNode): UiNode = mapCards(base) { c ->
            val kids = c.children.filterNot(::isActionBar).toMutableList()
            val media = kids.indexOfFirst { it is UiNode.Carousel }
            val caption = kids.indexOfFirst { it is UiNode.Conditional && it.value.endsWith(".caption") }
            if (media >= 0 && caption >= 0) kids[media] = kids[caption].also { kids[caption] = kids[media] }
            c.copy(children = kids)
        }

        /** The counter with only its first button. */
        fun incrementOnly(base: UiNode): UiNode {
            fun walk(n: UiNode): UiNode = when (n) {
                is UiNode.Workspace -> n.copy(slots = n.slots.map { it.copy(content = walk(it.content)) })
                is UiNode.Container -> if (n.orientation == Orientation.Horizontal && n.children.all { it is UiNode.Button }) {
                    n.copy(children = n.children.take(1))
                } else {
                    n.copy(children = n.children.map(::walk))
                }
                else -> n
            }
            return walk(base)
        }

        /** [base] with a [text] title in a first slot ([BANNER_SLOT], replaced if present). */
        fun withBanner(base: UiNode, text: String): UiNode {
            val ws = base as? UiNode.Workspace ?: return base
            val banner = SlotSpec(
                BANNER_SLOT, "core://text",
                UiNode.Container(Orientation.Vertical, 16, listOf(UiNode.Text(text, TextStyle.Title))),
            )
            return ws.copy(slots = listOf(banner) + ws.slots.filter { it.slotId != BANNER_SLOT })
        }
    }
}
