package org.theconstruct.desktop.sim

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
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
 * photo and no action bar, v2 photo first and still no action bar, v3 the bundle's layout) and `pixelfed_demo`
 * "Pixelfed demo" (v1 no action bar, v2 the bundle's), each from its default fixture bundle; the newest version is
 * always the bundle's layout, so the screen starts unchanged. Any other bundle is added as a one-version app.
 * [publish] appends a visibly changed copy of the newest version (the developer's "deploy").
 */
class FakeAppCatalog(private val clock: () -> Long = { System.currentTimeMillis() / 1000 }) {
    /** [schema] and [incompatible] are what the pod says for this client (D36); null = not said / compatible. */
    class Version(
        val layout: UiNode,
        val note: String?,
        val createdAt: Long,
        val source: String = "file",
        val schema: Int? = null,
        val incompatible: String? = null,
    )

    private class App(val id: String, val title: String, val icon: String, val custom: Boolean = false) {
        val versions = mutableListOf<Version>()
        var hidden = false

        /** `(built-in app, its newer version)` this copy hasn't seen; null = no notice. */
        var updatesFrom: Pair<String, Long>? = null
    }

    private val apps = LinkedHashMap<String, App>()

    @Synchronized
    fun add(id: String, title: String, icon: String, vararg versions: Version) {
        apps[id] = App(id, title, icon).also { it.versions += versions }
    }

    /** Adds the pod user's own app ([AppSummary.custom]): it can be deleted. */
    @Synchronized
    fun addCustom(id: String, title: String, icon: String, vararg versions: Version) {
        apps[id] = App(id, title, icon, custom = true).also { it.versions += versions }
    }

    /** `DELETE /v1/custom-apps/{id}`: only a custom app can go. */
    @Synchronized
    fun delete(id: String) {
        val a = app(id)
        if (!a.custom) throw LayoutApiException("That app doesn't exist.", LayoutApiException.Kind.NotFound, 404)
        apps.remove(id)
    }

    /** `PUT /v1/apps/{id}/hidden`. */
    @Synchronized
    fun setHidden(id: String, hidden: Boolean) {
        app(id).hidden = hidden
    }

    @Synchronized
    fun isCustom(id: String): Boolean = apps[id]?.custom == true

    /** Says custom app [id] was copied from [builtIn], which has [version] since. */
    @Synchronized
    fun setUpdatesFrom(id: String, builtIn: String, version: Long) {
        app(id).updatesFrom = builtIn to version
    }

    /** `POST /v1/custom-apps/{id}/dismiss-update`. */
    @Synchronized
    fun dismissUpdate(id: String, version: Long) {
        val a = app(id)
        if (a.updatesFrom?.second?.let { it <= version } == true) a.updatesFrom = null
    }

    @Synchronized
    fun has(id: String): Boolean = id in apps

    @Synchronized
    fun latestVersion(id: String): Long? = apps[id]?.versions?.size?.toLong()?.takeIf { it > 0 }

    @Synchronized
    fun latestLayout(id: String): UiNode? = apps[id]?.versions?.lastOrNull()?.layout

    /** The newest version this build can draw (what a pod serves it), 1-based; null if none. */
    @Synchronized
    fun latestCompatibleVersion(id: String): Long? =
        apps[id]?.versions?.indexOfLast { it.incompatible == null }?.takeIf { it >= 0 }?.let { it + 1L }

    /** The layout of [latestCompatibleVersion]. */
    @Synchronized
    fun latestCompatibleLayout(id: String): UiNode? = apps[id]?.versions?.lastOrNull { it.incompatible == null }?.layout

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
                    "kind" to JsonPrimitive(if (a.custom) "custom" else "builtin"),
                    "hidden" to JsonPrimitive(a.hidden),
                    "updates_from" to (a.updatesFrom?.let { (b, v) ->
                        obj(
                            "app" to JsonPrimitive(b),
                            "title" to JsonPrimitive(apps[b]?.title ?: b),
                            "to_version" to JsonPrimitive(v),
                        )
                    } ?: JsonNull),
                    "latest_version" to JsonPrimitive(a.versions.size),
                    "latest_compatible_version" to (latestCompatibleVersion(a.id)?.let { JsonPrimitive(it) } ?: JsonNull),
                    "updated_at" to JsonPrimitive(rfc3339(a.versions.last().createdAt)),
                )
            },
        ),
    ).toString()

    private fun versionObject(n: Int, v: Version): LinkedHashMap<String, JsonElement> = linkedMapOf<String, JsonElement>(
        "version" to JsonPrimitive(n),
        "created_at" to JsonPrimitive(rfc3339(v.createdAt)),
        "source" to JsonPrimitive(v.source),
    ).also { m ->
        v.note?.let { m["note"] = JsonPrimitive(it) }
        v.schema?.let { m["schema"] = JsonPrimitive(it) }
        m["compatible"] = JsonPrimitive(v.incompatible == null)
        v.incompatible?.let { m["incompatible"] = JsonPrimitive(it) }
    }

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
            "latest_compatible_version" to (latestCompatibleVersion(id)?.let { JsonPrimitive(it) } ?: JsonNull),
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
        private val ID = Regex("[a-z0-9_-]{1,64}")

        /** The fixture's custom app (a "Kitchen" over the demo posts, `fixtures/custom_kitchen_bundle.json`). */
        const val KITCHEN = "c-kitchen2dm"
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

        private val KNOWN = listOf("pixelfed_home", "pixelfed_demo", KITCHEN)

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
            // The fixture's Kitchen is a copy of the demo, which has a newer version since (decision `layout-parts`).
            if (catalog.has(KITCHEN) && catalog.has("pixelfed_demo")) {
                catalog.setUpdatesFrom(KITCHEN, "pixelfed_demo", catalog.latestVersion("pixelfed_demo") ?: 1)
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
                KITCHEN -> addCustom(
                    id, "Kitchen", "recipe",
                    Version(base, "Recipes next to my feed", t - 4 * HOUR, source = "custom"),
                )
                "pixelfed_home" -> add(
                    id, "Pixelfed", "photo",
                    Version(captionFirst(base), "First feed: caption above the photo", t - 9 * DAY),
                    Version(withoutActionBar(base), "Photos first", t - 2 * DAY),
                    Version(base, "Like, bookmark and comment bar", t - 3 * HOUR),
                )
                else -> add(
                    id, "Pixelfed demo", "photo",
                    Version(withoutActionBar(base), "One-post demo", t - 5 * DAY),
                    Version(base, "Action bar", t - 26 * HOUR),
                )
            }
        }

        private val EMPTY = UiNode.Container(Orientation.Vertical, 0, emptyList())

        /** Every list item template in [node] that is a [UiNode.Column] (a post card) transformed by [f]. */
        private fun mapCards(node: UiNode, f: (UiNode.Column) -> UiNode.Column): UiNode = when (node) {
            is UiNode.Workspace -> node.copy(slots = node.slots.map { it.copy(content = mapCards(it.content, f)) })
            is UiNode.Container -> node.copy(children = node.children.map { mapCards(it, f) })
            is UiNode.Column -> node.copy(children = node.children.map { mapCards(it, f) })
            is UiNode.Conditional -> node.copy(content = mapCards(node.content, f))
            is UiNode.ItemList -> (node.itemTemplate as? UiNode.Column)?.let { node.copy(itemTemplate = f(it)) } ?: node
            else -> node
        }

        /** A row of icon buttons, also when shown only for some posts (a [UiNode.Conditional] around it). */
        private fun isActionBar(n: UiNode): Boolean = when (n) {
            is UiNode.Row -> n.children.any { it is UiNode.IconButton }
            is UiNode.Conditional -> isActionBar(n.content)
            else -> false
        }

        fun withoutActionBar(base: UiNode): UiNode = mapCards(base) { c -> c.copy(children = c.children.filterNot(::isActionBar)) }

        /** The caption where the photo was (right under the author) and the photo after it; no action bar. */
        fun captionFirst(base: UiNode): UiNode = mapCards(base) { c ->
            val kids = c.children.filterNot(::isActionBar).toMutableList()
            val media = kids.indexOfFirst { it is UiNode.Carousel }
            val caption = kids.indexOfFirst { it is UiNode.Conditional && it.value.endsWith(".caption") }
            if (media >= 0 && caption >= 0) kids[media] = kids[caption].also { kids[caption] = kids[media] }
            c.copy(children = kids)
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
