package org.genui.canvas

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.longOrNull

// Layout browser model (DECISIONS D30): the pod's catalog JSON (D26) parsed into small records, the browser's
// navigation (a pure, saveable value), the change/undo bookkeeping and the repository that runs the blocking
// bridge calls (D27) off the UI thread. The Compose UI is in LayoutBrowser.kt.

/** One entry of `GET /v1/layouts`. [activeVersion] is set only on the active layout. */
data class LayoutSummary(
    val id: String,
    val title: String,
    val description: String = "",
    val previewHint: String = "",
    val currentVersion: Int = 1,
    val active: Boolean = false,
    val activeVersion: Int? = null,
)

/** One history row (`GET /v1/layouts/:id/versions`). [createdAt] is in epoch seconds. */
data class LayoutVersionInfo(
    val version: Int,
    val createdAt: Long = 0,
    val source: String = "",
    val prompt: String? = null,
    val provider: String? = null,
)

/** A page of history, newest first; pass [nextBefore] as `before` for the next (older) page. */
data class LayoutVersionPage(val versions: List<LayoutVersionInfo>, val nextBefore: Int? = null)

/** What an activate / restore answered: [version] = the layout's version now on top. */
data class LayoutChangeOutcome(
    val layoutId: String,
    val version: Int?,
    val unchanged: Boolean,
    val restoredVersion: Int? = null,
)

/** Lenient parsers for the pod's catalog JSON; a missing or mistyped field takes its default. */
object LayoutCatalogJson {
    private fun obj(json: String): JsonObject = CanvasJson.json.parseToJsonElement(json).jsonObject

    private fun JsonObject.prim(k: String): JsonPrimitive? = (this[k] as? JsonPrimitive)?.takeIf { it !is JsonNull }
    private fun JsonObject.str(k: String): String? = prim(k)?.takeIf { it.isString }?.contentOrNull
    private fun JsonObject.int(k: String): Int? = prim(k)?.takeIf { !it.isString }?.intOrNull
    private fun JsonObject.long(k: String): Long? = prim(k)?.takeIf { !it.isString }?.longOrNull
    private fun JsonObject.bool(k: String): Boolean? = prim(k)?.takeIf { !it.isString }?.booleanOrNull

    fun parseList(json: String): List<LayoutSummary> =
        ((obj(json)["layouts"] as? JsonArray) ?: JsonArray(emptyList())).mapNotNull { e ->
            val o = e as? JsonObject ?: return@mapNotNull null
            val id = o.str("id")?.takeIf { it.isNotBlank() } ?: return@mapNotNull null
            LayoutSummary(
                id = id,
                title = o.str("title")?.takeIf { it.isNotBlank() } ?: id.substringAfterLast('.'),
                description = o.str("description").orEmpty(),
                previewHint = o.str("preview_hint").orEmpty(),
                currentVersion = o.int("current_version") ?: 1,
                active = o.bool("active") == true,
                activeVersion = o.int("active_version"),
            )
        }

    fun parseVersions(json: String): LayoutVersionPage {
        val o = obj(json)
        val versions = ((o["versions"] as? JsonArray) ?: JsonArray(emptyList())).mapNotNull { e ->
            val v = e as? JsonObject ?: return@mapNotNull null
            LayoutVersionInfo(
                version = v.int("version") ?: return@mapNotNull null,
                createdAt = v.long("created_at") ?: 0,
                source = v.str("source").orEmpty(),
                prompt = v.str("prompt")?.takeIf { it.isNotBlank() },
                provider = v.str("provider")?.takeIf { it.isNotBlank() },
            )
        }
        return LayoutVersionPage(versions, o.int("next_before"))
    }

    /** The version's full layout (`layout`, a workspace [UiNode]). */
    fun parseVersionLayout(json: String): UiNode {
        val layout: JsonElement = obj(json)["layout"] ?: throw IllegalArgumentException("the version has no layout")
        return CanvasJson.decodeNode(layout.toString())
    }

    /** `POST /activate` -> `{layout_id, layout_version, unchanged, restored_version?}`. */
    fun parseActivate(json: String, layoutId: String): LayoutChangeOutcome {
        val o = obj(json)
        return LayoutChangeOutcome(
            layoutId = o.str("layout_id") ?: layoutId,
            version = o.int("layout_version"),
            unchanged = o.bool("unchanged") == true,
            restoredVersion = o.int("restored_version"),
        )
    }

    /** `POST /restore` -> `{layout_id, version, restored_from, unchanged, active}`. */
    fun parseRestore(json: String, layoutId: String): LayoutChangeOutcome {
        val o = obj(json)
        return LayoutChangeOutcome(
            layoutId = o.str("layout_id") ?: layoutId,
            version = o.int("version"),
            unchanged = o.bool("unchanged") == true,
            restoredVersion = o.int("restored_from"),
        )
    }
}

/**
 * The browser's navigation, innermost level last: closed -> list -> details of [layoutId] -> preview of [version];
 * [confirming] = the confirmation dialog for the current level's change is open. A plain value (saved across
 * rotation by the UI), so every transition is a pure function and testable without Compose.
 */
data class LayoutNav(
    val open: Boolean = false,
    val layoutId: String? = null,
    val version: Int? = null,
    val confirming: Boolean = false,
) {
    enum class Level { Closed, List, Details, Preview }

    val level: Level
        get() = when {
            !open -> Level.Closed
            layoutId == null -> Level.List
            version == null -> Level.Details
            else -> Level.Preview
        }

    fun openBrowser(): LayoutNav = if (open) this else LayoutNav(open = true)

    fun selectLayout(id: String): LayoutNav = LayoutNav(open = true, layoutId = id)

    /** Ignored unless a layout is selected. */
    fun selectVersion(v: Int): LayoutNav = if (layoutId == null) this else copy(open = true, version = v, confirming = false)

    /** Opens the confirmation for the current level (details: use this layout; preview: use / restore). */
    fun confirm(): LayoutNav = if (level == Level.Details || level == Level.Preview) copy(confirming = true) else this

    /** Back closes the innermost level: the dialog, then the preview, the details, the list. */
    fun back(): LayoutNav = when {
        confirming -> copy(confirming = false)
        level == Level.Preview -> copy(version = null)
        level == Level.Details -> copy(layoutId = null)
        else -> LayoutNav()
    }

    fun close(): LayoutNav = LayoutNav()

    /** For `rememberSaveable` (a list of primitives). */
    fun toSaveable(): List<Any?> = listOf(open, layoutId, version, confirming)

    companion object {
        fun fromSaveable(l: List<Any?>): LayoutNav = LayoutNav(
            open = l.getOrNull(0) as? Boolean ?: false,
            layoutId = l.getOrNull(1) as? String,
            version = l.getOrNull(2) as? Int,
            confirming = l.getOrNull(3) as? Boolean ?: false,
        )
    }
}

/** A change the browser can make. Every one is append-only on the pod, undo included. */
sealed class LayoutChange {
    abstract val layoutId: String

    /** Make [layoutId] active; with [version], restore that version first (D26 activate). */
    data class Activate(override val layoutId: String, val version: Int? = null) : LayoutChange()

    /** Append a copy of [version] as [layoutId]'s newest version. */
    data class Restore(override val layoutId: String, val version: Int) : LayoutChange()
}

/**
 * The change the browser offers at a level: details -> activate [layout] (null when it is already active); preview of
 * [version] -> on the active layout restore it (null when it is the current version), on an inactive one activate it
 * (at that version unless it is the newest).
 */
fun offeredChange(layout: LayoutSummary, version: Int?): LayoutChange? = when {
    version == null -> if (layout.active) null else LayoutChange.Activate(layout.id)
    layout.active -> if (version == (layout.activeVersion ?: layout.currentVersion)) null else LayoutChange.Restore(layout.id, version)
    version == layout.currentVersion -> LayoutChange.Activate(layout.id)
    else -> LayoutChange.Activate(layout.id, version)
}

/**
 * The change that undoes [change], given what was on screen [before] it: a switch is undone by activating the
 * previous layout at its previous version (no new version when that is still its newest content); a restore by
 * restoring the previous version (appended again: undo never deletes). Null when [before] is unknown or nothing
 * changed.
 */
fun undoFor(change: LayoutChange, before: ActiveLayout, outcome: LayoutChangeOutcome): LayoutChange? {
    if (outcome.unchanged) return null
    val prevId = before.layoutId ?: return null
    val prevVersion = before.layoutVersion ?: return null
    return when (change) {
        is LayoutChange.Activate -> LayoutChange.Activate(prevId, prevVersion)
        is LayoutChange.Restore -> if (prevId == change.layoutId) LayoutChange.Restore(prevId, prevVersion) else null
    }
}

/** The snackbar text after [change] (or its undo when [isUndo]); [title] = the layout's display name. */
fun changeMessage(change: LayoutChange, outcome: LayoutChangeOutcome, title: String, isUndo: Boolean = false): String {
    val text = when (change) {
        is LayoutChange.Activate -> when {
            outcome.unchanged -> "$title is already on screen"
            isUndo -> "Back to $title"
            outcome.restoredVersion != null && outcome.version != null ->
                "Switched to $title (v${outcome.restoredVersion} restored as v${outcome.version})"
            else -> "Switched to $title"
        }
        is LayoutChange.Restore -> when {
            outcome.unchanged -> "v${change.version} is already the newest version"
            outcome.version != null -> "Restored v${change.version} as v${outcome.version}"
            else -> "Restored v${change.version}"
        }
    }
    return if (isUndo && !outcome.unchanged) "Undone. $text" else text
}

/** Confirmation dialog copy: title, body and the confirm button's label. */
data class ConfirmCopy(val title: String, val body: String, val confirmLabel: String)

fun confirmCopy(change: LayoutChange, layout: LayoutSummary): ConfirmCopy = when (change) {
    is LayoutChange.Activate -> if (change.version == null) {
        ConfirmCopy(
            "Switch to ${layout.title}?",
            "Your screen changes to this layout right away. You can undo it afterwards.",
            "Switch",
        )
    } else {
        ConfirmCopy(
            "Use v${change.version} of ${layout.title}?",
            "A copy of v${change.version} becomes the newest version (v${layout.currentVersion + 1}) and is shown " +
                "right away. Nothing is deleted, and you can undo it afterwards.",
            "Use this version",
        )
    }
    is LayoutChange.Restore -> ConfirmCopy(
        "Restore v${change.version}?",
        "A copy of v${change.version} is added as the newest version (v${layout.currentVersion + 1}) and shown right " +
            "away. Nothing is deleted, and you can undo it afterwards.",
        "Restore",
    )
}

/** Display label for a version's `source`. */
fun versionSourceLabel(source: String): String = when (source) {
    "seed" -> "Built-in"
    "agent" -> "AI prompt"
    "rollback" -> "Rollback"
    "restore" -> "Restore"
    "manual" -> "Manual"
    "" -> "Unknown"
    else -> source.replaceFirstChar { it.uppercase() }
}

private val COPY_OF = Regex("""^(?:restore|rollback to) v(\d+)$""")

/**
 * The line under a history row: the AI prompt in quotes, "Copy of v1" for a restore / rollback ("restore v1",
 * "rollback to v1"), else the prompt as is (manual edits) or null.
 */
fun versionDetail(v: LayoutVersionInfo): String? {
    val p = v.prompt ?: return null
    COPY_OF.matchEntire(p.trim())?.let { return "Copy of v${it.groupValues[1]}" }
    return if (v.source == "agent") "“$p”" else p
}

/** "just now", "5 min ago", "3 h ago", "yesterday", "4 days ago"; null for a month or more (show the date only). */
fun relativeTime(epochSeconds: Long, nowSeconds: Long): String? {
    val d = (nowSeconds - epochSeconds).coerceAtLeast(0)
    return when {
        d < 60 -> "just now"
        d < 3_600 -> "${d / 60} min ago"
        d < 86_400 -> "${d / 3_600} h ago"
        d < 2 * 86_400 -> "yesterday"
        d < 30 * 86_400 -> "${d / 86_400} days ago"
        else -> null
    }
}

/** "3 h ago · Nov 2, 2026, 14:05" (local time); the date alone when older than a month. */
fun versionTimeLabel(epochSeconds: Long, nowSeconds: Long = nowEpochSeconds()): String {
    if (epochSeconds <= 0) return ""
    val abs = formatAbsoluteTime(epochSeconds)
    return relativeTime(epochSeconds, nowSeconds)?.let { "$it · $abs" } ?: abs
}

/** Background dispatcher for the blocking catalog calls (`Dispatchers.IO` on the JVM targets). */
internal expect val layoutIoDispatcher: CoroutineDispatcher

/** Wall clock in epoch seconds. */
internal expect fun nowEpochSeconds(): Long

/** A local date and time, e.g. "Nov 2, 2026, 14:05". */
internal expect fun formatAbsoluteTime(epochSeconds: Long): String

/**
 * Catalog calls in flight anywhere in the process (like [ImageLoads]); the simulator waits for 0 after a layout
 * command, since the calls run on a background dispatcher the Compose test clock does not see.
 */
object LayoutCalls {
    private val _inFlight = MutableStateFlow(0)
    val inFlight: StateFlow<Int> = _inFlight.asStateFlow()

    internal fun add(delta: Int) = _inFlight.update { it + delta }
}

/**
 * Runs the catalog calls of [bridge] on [io] and parses the answers. Version specs are immutable, so they are cached
 * per (layout id, version) for the life of the controller (it survives rotation on Android). Errors are
 * [LayoutApiException]s with a readable message; anything else (an unparsable answer) is wrapped into one.
 * Call the suspend functions from the UI's coroutines; cancelling one drops its result (the blocking call itself
 * finishes on its thread).
 */
class LayoutRepository(
    private val bridge: CanvasEngineBridge,
    private val io: CoroutineDispatcher = layoutIoDispatcher,
) {
    // Touched only on the caller's (UI) thread: the suspend functions return there after the background call.
    private val specs = HashMap<Pair<String, Int>, UiNode>()
    private val titles = HashMap<String, String>()

    /** The display title of [id] from the last list, else its last id segment. */
    fun titleOf(id: String): String = titles[id] ?: id.substringAfterLast('.')

    fun cachedSpec(id: String, version: Int): UiNode? = specs[id to version]

    suspend fun list(): List<LayoutSummary> = call {
        LayoutCatalogJson.parseList(bridge.listLayouts())
    }.also { l -> l.forEach { titles[it.id] = it.title } }

    suspend fun versions(id: String, before: Int? = null, limit: Int? = PAGE_SIZE): LayoutVersionPage =
        call { LayoutCatalogJson.parseVersions(bridge.layoutVersions(id, before, limit)) }

    suspend fun spec(id: String, version: Int): UiNode {
        cachedSpec(id, version)?.let { return it }
        val node = call { LayoutCatalogJson.parseVersionLayout(bridge.layoutVersion(id, version)) }
        specs[id to version] = node
        return node
    }

    suspend fun apply(change: LayoutChange): LayoutChangeOutcome = call {
        when (change) {
            is LayoutChange.Activate ->
                LayoutCatalogJson.parseActivate(bridge.activateLayout(change.layoutId, change.version), change.layoutId)
            is LayoutChange.Restore ->
                LayoutCatalogJson.parseRestore(bridge.restoreLayoutVersion(change.layoutId, change.version), change.layoutId)
        }
    }

    private suspend fun <T> call(block: () -> T): T {
        LayoutCalls.add(1)
        try {
            return withContext(io) {
                try {
                    block()
                } catch (e: LayoutApiException) {
                    throw e
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Exception) {
                    println("[Canvas] layout catalog: $e")
                    throw LayoutApiException("The pod sent an answer this app can't read.")
                }
            }
        } finally {
            LayoutCalls.add(-1)
        }
    }

    companion object {
        /** History page size. */
        const val PAGE_SIZE = 20
    }
}
