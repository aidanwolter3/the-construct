package org.theconstruct

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
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.longOrNull

// The pod's apps and their linear version history (GET /v1/apps, /v1/apps/{id}/versions[/{n}]) for the hamburger
// menu, plus the pure rules the menu follows (pinning, "newer version available", the badge, server URLs). The
// Compose UI is in Menu.kt.

/**
 * The layout on display, from [EngineSnapshot.layoutJson]: the app ([appId], i.e. the workspace id) and its latest
 * [version] the engine applied. [version] is null while unknown and on an unversioned screen (the signed-out
 * `pixelfed_home`). The engine emits `{"app_id","version"}` (D33).
 */
data class ActiveLayout(
    val appId: String? = null,
    val version: Long? = null,
) {
    companion object {
        /** Lenient: anything unparsable reads as unknown. */
        fun parse(json: String): ActiveLayout = try {
            val o = ConstructJson.json.parseToJsonElement(json).jsonObject
            ActiveLayout(
                appId = o.jStr("app_id")?.takeIf { it.isNotBlank() },
                version = o.jLong("version"),
            )
        } catch (t: Exception) {
            ActiveLayout()
        }
    }
}

/**
 * An apps / versions call failed. [message] is a short sentence for the UI ("Can't reach your pod. ..."); [kind] tells
 * the cases apart; [status] is the HTTP status for [Kind.Server] (0 if none).
 */
class LayoutApiException(
    override val message: String,
    val kind: Kind = Kind.Server,
    val status: Int = 0,
) : Exception(message) {
    enum class Kind { Offline, Unauthorized, NotFound, Server }
}

/**
 * One entry of `GET /v1/apps`. [updatedAt] is in epoch seconds (0 = unknown). [latestVersion] is the app's newest
 * version; [latestCompatibleVersion] the newest one this build can draw, i.e. what opening the app shows (null when
 * none fits; equal to [latestVersion] from a pod too old to say).
 */
data class AppSummary(
    val id: String,
    val title: String,
    val icon: String = "",
    val latestVersion: Long? = null,
    val updatedAt: Long = 0,
    val latestCompatibleVersion: Long? = latestVersion,
) {
    /** The app's newest version when this build can't draw it ("Update the app to see vN"), else null. */
    val updateRequiredFor: Long? get() = updateRequiredFor(latestVersion, latestCompatibleVersion)
}

/** [latest] when it is newer than [compatible] (the newest version this build can draw), else null. */
fun updateRequiredFor(latest: Long?, compatible: Long?): Long? =
    latest?.takeIf { compatible == null || it > compatible }

/** The hint for a version only a newer build can draw. */
fun updateHint(version: Long): String = "Update the app to see v$version"

/**
 * One history row. [createdAt] is in epoch seconds (0 = unknown). [schema] is the client schema the version was
 * written for; [incompatible] is why this build can't draw it (`update_required`, `retired`, or another reason a newer
 * pod sends), null when it can (or the pod is too old to say).
 */
data class AppVersionInfo(
    val version: Long,
    val createdAt: Long = 0,
    val source: String = "",
    val note: String? = null,
    val schema: Long? = null,
    val incompatible: String? = null,
) {
    val compatible: Boolean get() = incompatible == null
}

/** The picker's line (and the error) for a version this build can't draw. */
fun incompatibleLabel(reason: String): String = when (reason) {
    "update_required" -> "Needs a newer version of the app"
    "retired" -> "Made for an older version of the app"
    else -> "This version of the app can't show it"
}

/**
 * A page of history, newest first; pass [nextBefore] as `before` for the next (older) page. [latestCompatibleVersion]
 * as in [AppSummary].
 */
data class AppVersionPage(
    val versions: List<AppVersionInfo>,
    val nextBefore: Long? = null,
    val latestVersion: Long? = null,
    val latestCompatibleVersion: Long? = latestVersion,
)

/** An app's history as loaded so far (newest first) in the version picker. */
data class VersionHistory(
    val versions: List<AppVersionInfo> = emptyList(),
    val nextBefore: Long? = null,
    val loaded: Boolean = false,
    val loadingMore: Boolean = false,
    val error: String? = null,
) {
    /** "Load older" is offered while the pod said older versions exist. */
    val hasMore: Boolean get() = loaded && nextBefore != null

    /** The first page: replaces everything. */
    fun withFirstPage(page: AppVersionPage): VersionHistory = VersionHistory(page.versions, page.nextBefore, loaded = true)

    /**
     * An older page requested with `before = [requestedBefore]`: appended (rows already listed are skipped) unless the
     * history was reloaded meanwhile (its [nextBefore] moved), in which case it is dropped.
     */
    fun withOlderPage(requestedBefore: Long, page: AppVersionPage): VersionHistory {
        if (nextBefore != requestedBefore) return copy(loadingMore = false)
        val seen = versions.mapTo(HashSet()) { it.version }
        return copy(versions = versions + page.versions.filter { it.version !in seen }, nextBefore = page.nextBefore, loadingMore = false, error = null)
    }
}

/** Lenient parsers for the pod's apps JSON; a missing or mistyped field takes its default. */
object AppCatalogJson {
    private fun obj(json: String): JsonObject = ConstructJson.json.parseToJsonElement(json).jsonObject

    fun parseApps(json: String): List<AppSummary> =
        ((obj(json)["apps"] as? JsonArray) ?: JsonArray(emptyList())).mapNotNull { e ->
            val o = e as? JsonObject ?: return@mapNotNull null
            val id = o.jStr("id")?.takeIf { it.isNotBlank() } ?: return@mapNotNull null
            AppSummary(
                id = id,
                title = o.jStr("title")?.takeIf { it.isNotBlank() } ?: id,
                icon = o.jStr("icon").orEmpty(),
                latestVersion = o.jLong("latest_version"),
                updatedAt = parseTimestamp(o["updated_at"]),
                latestCompatibleVersion = o.latestCompatible(),
            )
        }

    fun parseVersions(json: String): AppVersionPage {
        val o = obj(json)
        val versions = ((o["versions"] as? JsonArray) ?: JsonArray(emptyList())).mapNotNull { e ->
            val v = e as? JsonObject ?: return@mapNotNull null
            parseVersionInfo(v)
        }
        return AppVersionPage(versions, o.jLong("next_before"), o.jLong("latest_version"), o.latestCompatible())
    }

    /** `latest_compatible_version` (null = none fits); `latest_version` from a pod that doesn't send it. */
    private fun JsonObject.latestCompatible(): Long? =
        if (containsKey("latest_compatible_version")) jLong("latest_compatible_version") else jLong("latest_version")

    private fun parseVersionInfo(v: JsonObject): AppVersionInfo? = AppVersionInfo(
        version = v.jLong("version") ?: return null,
        createdAt = parseTimestamp(v["created_at"]),
        source = v.jStr("source").orEmpty(),
        note = v.jStr("note")?.takeIf { it.isNotBlank() },
        schema = v.jLong("schema"),
        incompatible = incompatibility(v),
    )

    /** Null when the pod says (or doesn't say otherwise) that this build can draw the version. */
    private fun incompatibility(v: JsonObject): String? =
        if (v.jPrim("compatible")?.takeIf { !it.isString }?.booleanOrNull == false) v.jStr("incompatible") ?: "" else null

    /**
     * The version's full layout (`layout`, a workspace [UiNode]). Throws a [LayoutApiException] when the pod says this
     * build can't draw it.
     */
    fun parseVersionLayout(json: String): UiNode {
        val o = obj(json)
        incompatibility(o)?.let { throw LayoutApiException("${incompatibleLabel(it)}.") }
        val layout: JsonElement = o["layout"] ?: throw IllegalArgumentException("the version has no layout")
        return ConstructJson.decodeNode(layout.toString())
    }
}

private fun JsonObject.jPrim(k: String): JsonPrimitive? = (this[k] as? JsonPrimitive)?.takeIf { it !is JsonNull }
private fun JsonObject.jStr(k: String): String? = jPrim(k)?.takeIf { it.isString }?.contentOrNull
private fun JsonObject.jLong(k: String): Long? = jPrim(k)?.takeIf { !it.isString }?.longOrNull

private val RFC3339 = Regex(
    """(\d{4})-(\d{2})-(\d{2})[Tt ](\d{2}):(\d{2}):(\d{2})(?:\.\d+)?(?:([Zz])|([+-])(\d{2}):?(\d{2}))""",
)

/** Epoch seconds from an RFC 3339 string (`2026-11-02T14:05:00Z`) or a number of epoch seconds; 0 when unreadable. */
fun parseTimestamp(e: JsonElement?): Long {
    val p = e as? JsonPrimitive ?: return 0
    if (p is JsonNull) return 0
    if (!p.isString) return p.longOrNull ?: 0
    val m = RFC3339.matchEntire(p.content.trim()) ?: return 0
    val g = m.groupValues
    val days = daysFromCivil(g[1].toInt(), g[2].toInt(), g[3].toInt())
    var secs = days * 86_400 + g[4].toLong() * 3_600 + g[5].toLong() * 60 + g[6].toLong()
    if (g[8].isNotEmpty()) {
        val offset = g[9].toLong() * 3_600 + g[10].toLong() * 60
        secs -= if (g[8] == "+") offset else -offset
    }
    return secs
}

/** Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's algorithm). */
private fun daysFromCivil(year: Int, month: Int, day: Int): Long {
    val y = (if (month <= 2) year - 1 else year).toLong()
    val era = (if (y >= 0) y else y - 399) / 400
    val yoe = y - era * 400
    val mp = (month + 9) % 12
    val doy = (153 * mp + 2) / 5 + day - 1
    val doe = yoe * 365 + yoe / 4 - yoe / 100 + doy
    return era * 146_097 + doe - 719_468
}

/** Display label for a version's `source`. */
fun versionSourceLabel(source: String): String = when (source) {
    "file" -> "Published"
    "seed" -> "Built-in"
    "agent" -> "AI edit"
    "rollback" -> "Rollback"
    "restore" -> "Restore"
    "manual" -> "Manual edit"
    "switch" -> "Layout switch"
    "" -> "Unknown"
    else -> source.replaceFirstChar { it.uppercase() }
}

/** The line under a history row: why this build can't draw it, else its note, else its source. */
fun versionDetail(v: AppVersionInfo): String =
    v.incompatible?.let { incompatibleLabel(it) + (v.note?.let { n -> " · $n" } ?: "") } ?: v.note ?: versionSourceLabel(v.source)

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

/** "3 h ago", or the local date when older than a month; "" when unknown. */
fun versionTimeLabel(epochSeconds: Long, nowSeconds: Long = nowEpochSeconds()): String {
    if (epochSeconds <= 0) return ""
    return relativeTime(epochSeconds, nowSeconds) ?: formatAbsoluteTime(epochSeconds)
}

/** Background dispatcher for the blocking apps calls (`Dispatchers.IO` on the JVM targets). */
internal expect val layoutIoDispatcher: CoroutineDispatcher

/** Wall clock in epoch seconds. */
internal expect fun nowEpochSeconds(): Long

/** A local date and time, e.g. "Nov 2, 2026, 14:05". */
internal expect fun formatAbsoluteTime(epochSeconds: Long): String

/**
 * Apps calls in flight anywhere in the process (like [ImageLoads]); the simulator waits for 0 after a menu command,
 * since the calls run on a background dispatcher the Compose test clock does not see.
 */
object LayoutCalls {
    private val _inFlight = MutableStateFlow(0)
    val inFlight: StateFlow<Int> = _inFlight.asStateFlow()

    internal fun add(delta: Int) = _inFlight.update { it + delta }
}

/**
 * Runs the apps calls of the current bridge ([bridge] is read per call: [ConstructController.openApp] swaps it) on
 * [io] and parses the answers. Version specs are immutable, so they are cached per (app, version) for the life of the
 * controller. Errors are [LayoutApiException]s with a readable message; anything else (an unparsable answer) is
 * wrapped into one.
 */
class AppRepository(
    private val bridge: () -> ConstructEngineBridge,
    private val io: CoroutineDispatcher = layoutIoDispatcher,
) {
    // Touched only on the caller's (UI) thread: the suspend functions return there after the background call.
    private val specs = HashMap<Pair<String, Long>, UiNode>()

    /** The last apps list loaded (any thread may read it), or null before the first. */
    @Volatile
    var lastApps: List<AppSummary>? = null
        private set

    suspend fun apps(): List<AppSummary> = call { AppCatalogJson.parseApps(bridge().listApps()) }.also { lastApps = it }

    /** Runs a blocking notification call of the current bridge on the background dispatcher, like the apps calls. */
    internal suspend fun <T> notifications(block: (ConstructEngineBridge) -> T): T = call { block(bridge()) }

    suspend fun versions(appId: String, before: Long? = null, limit: Int? = PAGE_SIZE): AppVersionPage =
        call { AppCatalogJson.parseVersions(bridge().appVersions(appId, before, limit)) }

    suspend fun spec(appId: String, version: Long): UiNode {
        specs[appId to version]?.let { return it }
        val node = call { AppCatalogJson.parseVersionLayout(bridge().appVersion(appId, version)) }
        specs[appId to version] = node
        return node
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
                    println("[Construct] apps: $e")
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

// ---------------------------------------------------------------------------------------------------------------------
// Pinning and the menu badge
// ---------------------------------------------------------------------------------------------------------------------

/**
 * A version the user picked in the version picker: shown instead of the latest (against the current entities) until
 * they pick "Latest" again. Client-local: nothing is sent to the pod, and it is not persisted.
 */
data class PinnedVersion(val appId: String, val version: Long, val layout: UiNode)

/**
 * Whether [pin] applies to what the engine shows: only while the workspace shows a versioned layout of the pinned app
 * (the signed-out screen, which has no version, is shown as is).
 */
fun pinApplies(state: ConstructState, pin: PinnedVersion?): Boolean {
    if (pin == null || state.workspace == null) return false
    val active = state.activeLayout
    if (active.version == null) return false
    return active.appId == null || active.appId == pin.appId
}

/** What to render: the pinned version's layout when [pinApplies], else the engine's workspace. */
fun displayedWorkspace(state: ConstructState, pin: PinnedVersion?): UiNode? =
    if (pinApplies(state, pin)) pin!!.layout else state.workspace

/**
 * The pinned version is behind the version the engine shows when following the latest ("Newer version available").
 * That version is what the pod serves this build (the newest it can draw), so a version only a newer build can draw
 * never counts.
 */
fun newerVersionAvailable(state: ConstructState, pin: PinnedVersion?): Boolean {
    if (!pinApplies(state, pin)) return false
    val latest = state.activeLayout.version ?: return false
    return latest > pin!!.version
}

/**
 * The hamburger's badge dot: offline, mutations waiting to be sent, or a pinned version with a newer one out. Unknown
 * connectivity (before the first snapshot) does not badge.
 */
fun showMenuBadge(state: ConstructState, pin: PinnedVersion?): Boolean =
    state.isOnline == false || state.pendingActions > 0 || newerVersionAvailable(state, pin)

/**
 * The line under an app in the menu's Apps section. [shown] is the version following the latest shows: the engine's
 * for the app on screen ([isCurrent]), else [AppSummary.latestCompatibleVersion]. [pinned] is the pinned version shown
 * instead. Adds [updateHint] when the app's newest version needs a newer build.
 */
fun appRowSubtitle(app: AppSummary, isCurrent: Boolean, shown: Long?, pinned: Long?): String {
    val base = when {
        pinned != null -> "Showing v$pinned" + (shown?.let { " · latest v$it" } ?: "")
        isCurrent -> "Latest" + (shown?.let { " · v$it" } ?: "")
        else -> shown?.let { "v$it" } ?: ""
    }
    val hint = updateRequiredFor(app.latestVersion, maxOf(shown ?: 0, app.latestCompatibleVersion ?: 0).takeIf { it > 0 })
        ?.let { updateHint(it) } ?: return base
    return if (base.isEmpty()) hint else "$base · $hint"
}

/**
 * The version picker's "Latest" detail; [latest] is the version following the latest shows. Adds [updateHint] when the
 * newest loaded version needs a newer build of the app.
 */
fun latestRowDetail(history: VersionHistory, latest: Long?): String {
    val base = "Follows new versions as they're published"
    val newest = history.versions.firstOrNull()?.takeIf { it.incompatible == "update_required" } ?: return base
    return updateRequiredFor(newest.version, latest)?.let { "$base. ${updateHint(it)}" } ?: base
}

/**
 * The pin of [appId] the version picker checks: [pin] when that app is on screen ([isCurrent]) and the pin applies
 * ([pinApplies]), as in the Apps section. Otherwise null ("Latest" is checked for the app on screen).
 */
fun pinnedInPicker(state: ConstructState, pin: PinnedVersion?, appId: String, isCurrent: Boolean): PinnedVersion? =
    pin?.takeIf { isCurrent && it.appId == appId && pinApplies(state, it) }

/**
 * The picker's "Pinned: vN" row (`version-pinned`, checked, under "Latest"): the pinned version when it is not among
 * the loaded history rows (it is on an older page), so the selection stays visible; null once its own row is listed.
 */
fun pinnedRowVersion(pinned: PinnedVersion?, history: VersionHistory): Long? {
    val v = pinned?.version ?: return null
    if (!history.loaded || history.versions.any { it.version == v }) return null
    return v
}

/**
 * The menu's "Sign in" action: the first sign-in intent the current screen offers (the signed-out screen's button
 * carries the pod's Pixelfed instance), or null when the screen has none.
 */
fun findSignInIntent(node: UiNode?): ActionIntent.AuthLogin? = when (node) {
    null -> null
    is UiNode.Button -> node.onClick as? ActionIntent.AuthLogin
    is UiNode.Workspace -> node.slots.firstNotNullOfOrNull { findSignInIntent(it.content) }
    is UiNode.Container -> node.children.firstNotNullOfOrNull { findSignInIntent(it) }
    else -> null
}

// ---------------------------------------------------------------------------------------------------------------------
// Server choice
// ---------------------------------------------------------------------------------------------------------------------

/** One preset server in the menu's Server section; [key] makes its test tag `server-<key>`. */
data class ServerOption(val key: String, val label: String, val url: String)

/**
 * The menu's Server section, supplied by the platform: the [currentUrl], the [options] (e.g. Staging / Local emulator /
 * Local USB) and [onSelect], called with a preset's URL or a custom one that passed [ServerUrls.check] with
 * [cleartextHosts]. The platform applies it (creates the new engine, then saves the choice). No section without one.
 *
 * [cleartextHosts]: the hosts a custom `http://` URL may name (Android: the loopback / emulator hosts its network
 * security config lets images load from, [ServerUrls.ANDROID_CLEARTEXT_HOSTS]); null = any host (desktop).
 *
 * [error]: why the last switch failed ([switchFailed]), shown under the section's header; the platform clears it on the
 * next attempt.
 */
class ServerChoice(
    val currentUrl: String,
    val options: List<ServerOption>,
    val cleartextHosts: Set<String>? = null,
    val error: String? = null,
    val onSelect: (String) -> Unit,
) {
    companion object {
        /** The Server section's message for a switch to [url] whose engine could not be created. */
        fun switchFailed(url: String, reason: Throwable): String =
            "Couldn't switch to ${url.trimEnd('/')}: ${reason.message?.takeIf { it.isNotBlank() } ?: reason::class.simpleName}"
    }

    /** The preset's label for [url], else the URL itself. */
    fun label(url: String = currentUrl): String =
        options.firstOrNull { it.url.trimEnd('/') == url.trimEnd('/') }?.label ?: url
}

/** A custom server URL as typed: [Ok] (normalized), [Invalid] (not `http(s)://host[:port]`) or [NeedsHttps]. */
sealed class ServerUrlCheck {
    data class Ok(val url: String) : ServerUrlCheck()
    object Invalid : ServerUrlCheck()

    /** `http://` to a host the platform only reaches over HTTPS (Android: anything but the loopback hosts). */
    object NeedsHttps : ServerUrlCheck()
}

object ServerUrls {
    private const val LABEL = """[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?"""
    private val URL = Regex("""(https?)://($LABEL(?:\.$LABEL)*|\[[0-9A-Fa-f:.]+])(?::(\d{1,5}))?""")

    /**
     * Android's cleartext hosts (network_security_config.xml): the emulator's alias for the host machine and the
     * loopback names (`adb reverse` on a device). Coil loads images over Java's stack, which blocks `http://` elsewhere.
     */
    val ANDROID_CLEARTEXT_HOSTS: Set<String> = setOf("localhost", "127.0.0.1", "10.0.2.2")

    /** Hint under the custom URL field when [check] says [ServerUrlCheck.NeedsHttps]. */
    const val NEEDS_HTTPS_HINT = "Use https:// for remote servers (http:// only for localhost, 127.0.0.1 or 10.0.2.2)"

    /** Hint under the custom URL field for an [ServerUrlCheck.Invalid] URL. */
    const val FORMAT_HINT = "http(s)://host[:port]"

    /**
     * [parse] plus the platform's cleartext policy: `https://` to any host; `http://` only to a host in
     * [cleartextHosts] (case-insensitive), or to any host when it is null.
     */
    fun check(input: String, cleartextHosts: Set<String>? = null): ServerUrlCheck {
        val url = parse(input) ?: return ServerUrlCheck.Invalid
        val m = URL.matchEntire(url)!!
        if (m.groupValues[1].lowercase() == "http" && cleartextHosts != null && m.groupValues[2].lowercase() !in cleartextHosts) {
            return ServerUrlCheck.NeedsHttps
        }
        return ServerUrlCheck.Ok(url)
    }

    /**
     * The normalized URL (trimmed, trailing `/` dropped) if [input] is `http(s)://host[:port]` (no path, query or
     * fragment; port 1..65535), else null.
     */
    fun parse(input: String): String? {
        val url = input.trim().trimEnd('/')
        val m = URL.matchEntire(url) ?: return null
        val port = m.groupValues[3]
        if (port.isNotEmpty() && port.toInt() !in 1..65_535) return null
        return url
    }
}
