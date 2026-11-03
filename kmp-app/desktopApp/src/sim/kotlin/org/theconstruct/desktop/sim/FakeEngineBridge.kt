package org.theconstruct.desktop.sim

import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.builtins.MapSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.add
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put
import org.theconstruct.ActionIntent
import org.theconstruct.AppNotification
import org.theconstruct.AttachmentException
import org.theconstruct.AttachmentInfo
import org.theconstruct.ConstructEngineBridge
import org.theconstruct.CreatePosition
import org.theconstruct.ConstructJson
import org.theconstruct.EngineSnapshot
import org.theconstruct.LayoutApiException
import org.theconstruct.UiNode
import org.theconstruct.attachmentUri
import java.io.File
import javax.imageio.ImageIO

/**
 * In-memory [ConstructEngineBridge] for simulation mode and tests: no Rust, no server.
 *
 * Loads a hydration bundle (`{version, workspace, entities, queries}`, DECISIONS D2) and emits snapshots
 * shaped like the real engine's (D6: workspace JSON = the layout node, entities JSON = the
 * URI-keyed map, queries JSON = the bundle's queries; `seq` increases with every state change).
 * [authJson] is passed through unchanged (default signed out) until [logout] sets it to signed out. While it is
 * `signed_in`, `signIns` (Settings) lists one Pixelfed sign-in on the fake upstream ([SIGN_IN_SERVER]) with its
 * `username`.
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
 * Apps (the hamburger menu): [apps] (by default [FakeAppCatalog.fixture]) answers `listApps` / `appVersions` /
 * `appVersion` with the pod's JSON shapes; this bridge shows [appId]'s newest version and reports it in `layout_json`
 * (`{"app_id","version"}`). [publishVersion] appends a new newest version (the developer's deploy) and shows it, like
 * the engine applying an SSE `layout` event. Offline, every apps call throws [LayoutApiException] (Offline).
 *
 * Screens (DECISIONS D34): [screenData] is the fake pod's answers for data paths (`{path: {entities, queries}}`, e.g.
 * `fixtures/pixelfed_screen_data.json`). `action.data.load` answers from it at once (offline: an error, cached data
 * kept); an unknown path is "not found". `action.mutation.create` lists its entity at once and queues it; when sent,
 * the fake pod accepts it if some data path lists its query (the entity is added there, with a `meta` field, if it has
 * one, set to "just now") and the client reloads those paths; otherwise it is rejected and removed. A create with
 * `position: start` is listed first while pending.
 *
 * Attachments (schema 9, D49): [stageAttachment] accepts any image file ImageIO can read and lists its local entity
 * `construct://attachments/sim-<n>` (`url` = the file itself). A create with `attachments` (a new post) replaces
 * `"$attachments.<name>"` in its entity with those entity URIs and stays a pending placeholder (as the layout drew it,
 * e.g. its `pending_label` "Posting…") until [settle] (the simulator's `wait` / `idle` steps) while online; it then
 * becomes a finished post at the same URI and place: without `pending_label` / `failed`, with a fake `id`,
 * `like_count` 0, `reply_count` 0 and `is_liked` false.
 *
 * Notifications (D52): with [notifications] (shared by the run's bridges) the notification calls answer from it, the
 * snapshot carries its unread counts, and [FakeNotifications.inject] fires this bridge's listener (live arrival) while
 * it is open. Without, the interface defaults apply ("Notifications aren't available here.").
 */
class FakeEngineBridge(
    bundleJson: String,
    online: Boolean = true,
    private var authJson: String = "{\"state\":\"signed_out\"}",
    appId: String? = null,
    apps: FakeAppCatalog? = null,
    screenData: String? = null,
    /** The fake pod's notifications (null = this fake has none). */
    val notifications: FakeNotifications? = null,
) : ConstructEngineBridge {
    /** [prevValues]: field -> value before this row was applied (null = absent). */
    private class Row(val intent: ActionIntent.OptimisticMutate, var prevValues: Map<String, JsonElement?>)

    private var workspace: UiNode

    /** The app this engine shows (the bundle's, see [FakeAppCatalog.appIdFor], unless given). */
    val appId: String

    /** The apps and their histories (shared with the other bridges of a simulator run). */
    val apps: FakeAppCatalog

    /** The version on screen: [appId]'s newest. */
    val layoutVersion: Long get() = synchronized(lock) { shownVersion }
    private var shownVersion = 1L
    private val queriesJson: String
    private val entities: MutableMap<String, JsonObject>
    private val server: MutableMap<String, JsonObject>
    private val outbox = ArrayDeque<Row>()
    private val listeners = mutableListOf<(EngineSnapshot) -> Unit>()
    private var isOnline = online
    private var seq = 0L
    private val lock = Any()

    /** A queued `action.mutation.create`. */
    private class CreateRow(val intent: ActionIntent.Create)

    private val createRows = ArrayDeque<CreateRow>()

    /** Creates with attachments still "posting" (see the class docs), oldest first. */
    private val postRows = ArrayDeque<CreateRow>()

    /** Finished posts: query_key -> URIs listed first (newest first) or last. */
    private val postedFirst: MutableMap<String, List<String>> = LinkedHashMap()
    private val postedLast: MutableMap<String, List<String>> = LinkedHashMap()
    private var attachmentCount = 0
    private var postCount = 0

    /** The fake pod's data path answers. */
    private val serverData: MutableMap<String, JsonObject> = LinkedHashMap()

    /** Queries of the data paths loaded so far (path -> query_key -> uris). */
    private val loadedQueries: MutableMap<String, Map<String, List<String>>> = LinkedHashMap()
    private val loads: MutableMap<String, JsonObject> = LinkedHashMap()
    private var sentCount = 0

    init {
        screenData?.let { raw ->
            ConstructJson.json.parseToJsonElement(raw).jsonObject.forEach { (path, v) ->
                (v as? JsonObject)?.let { serverData[path] = it }
            }
        }
    }

    init {
        val bundle = ConstructJson.decodeBundle(bundleJson)
        workspace = bundle.workspace
        queriesJson = ConstructJson.json.encodeToString(queriesSerializer, bundle.queries)
        entities = LinkedHashMap(bundle.entities)
        server = LinkedHashMap(bundle.entities)
        this.appId = appId ?: FakeAppCatalog.appIdFor(workspace, pixelfed = entities.keys.any { it.startsWith("pixelfed://") })
        this.apps = apps ?: FakeAppCatalog.fixture(this.appId, workspace)
        // Show the app's newest version this build can draw, as a pod serves it (the bundle's layout in the fixture
        // catalog).
        this.apps.latestCompatibleLayout(this.appId)?.let { workspace = it }
        shownVersion = this.apps.latestCompatibleVersion(this.appId) ?: 1
    }

    @Volatile
    private var notificationListener: ((String) -> Unit)? = null

    /** Re-emits on a notifications change; on a live arrival also tells the listener first. */
    private val notificationWatcher: (AppNotification?) -> Unit = { n ->
        if (n != null) notificationListener?.invoke(ConstructJson.json.encodeToString(AppNotification.serializer(), n))
        synchronized(lock) { seq++ }
        emit()
    }

    init {
        notifications?.watch(notificationWatcher)
    }

    override fun setNotificationListener(onNotification: (String) -> Unit) {
        notificationListener = onNotification
    }

    override fun setDeviceInfo(platform: String, name: String) {
        println("[sim] device info: $platform / $name")
    }

    override fun deviceId(): String = if (notifications == null) "" else "sim-device"

    override fun listNotifications(before: String?, limit: Int, appId: String?): String =
        notificationsCall { it.listJson(before, limit, appId) }

    override fun markNotificationsRead(ids: List<String>): String = notificationsCall { it.markRead(ids) }

    override fun markAllNotificationsRead(appId: String?): String = notificationsCall { it.markAllRead(appId) }

    override fun notificationChannels(): String = notificationsCall { it.channelsJson() }

    override fun setChannelEnabled(appId: String, channel: String, enabled: Boolean) {
        notificationsCall(needsPod = true) { it.setChannelEnabled(appId, channel, enabled); "" }
    }

    override fun pushPublicKey(): String =
        throw LayoutApiException("This pod doesn't send push notifications.", LayoutApiException.Kind.NotFound, 404)

    /** Like the engine: lists work offline (its cache; here the store), marking read is optimistic. */
    private fun notificationsCall(needsPod: Boolean = false, call: (FakeNotifications) -> String): String {
        val n = notifications ?: throw LayoutApiException("Notifications aren't available here.")
        if (needsPod && !online) {
            throw LayoutApiException("Can't reach your pod. Check your connection and try again.", LayoutApiException.Kind.Offline)
        }
        return call(n)
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
        val intent = ConstructJson.decodeIntent(intentJson)
        if (intent is ActionIntent.LoadData) {
            synchronized(lock) {
                loadLocked(intent.path)
                seq++
            }
            emit()
            return
        }
        if (intent is ActionIntent.Create && intent.attachments.isNotEmpty()) {
            synchronized(lock) {
                if (!createPostLocked(intent)) return
                seq++
            }
            emit()
            return
        }
        if (intent is ActionIntent.Create) {
            synchronized(lock) {
                if (entities.containsKey(intent.entityUri) || intent.entity !is JsonObject) {
                    println("[sim] create ${intent.entityUri} ignored")
                    return
                }
                entities[intent.entityUri] = intent.entity as JsonObject
                createRows.addLast(CreateRow(intent))
                println("[sim] create ${intent.entityUri} in ${intent.queryKey} (queued; $pendingCount pending)")
                if (isOnline) flushLocked()
                seq++
            }
            emit()
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

    /**
     * Stages [path] like the engine (D49), without re-encoding: any image ImageIO reads is accepted; its local entity
     * points at the file itself.
     */
    override fun stageAttachment(path: String): Result<AttachmentInfo> {
        val file = File(path)
        if (!file.isFile) return Result.failure(AttachmentException("Couldn't read that photo."))
        val image = try {
            ImageIO.read(file)
        } catch (e: Exception) {
            null
        } ?: return Result.failure(AttachmentException("That file isn't a photo this app can read."))
        val info = synchronized(lock) {
            val id = "sim-${++attachmentCount}"
            val url = "file://${file.absolutePath}"
            entities[attachmentUri(id)] = JsonObject(
                mapOf(
                    "url" to JsonPrimitive(url),
                    "preview_url" to JsonPrimitive(url),
                    "aspect_ratio" to JsonPrimitive(image.width.toDouble() / image.height.coerceAtLeast(1)),
                    "width" to JsonPrimitive(image.width),
                    "height" to JsonPrimitive(image.height),
                ),
            )
            seq++
            AttachmentInfo(id, attachmentUri(id), url, image.width, image.height)
        }
        println("[sim] staged ${file.name} as ${info.id} (${info.width}x${info.height})")
        emit()
        return Result.success(info)
    }

    override fun discardAttachment(id: String) {
        synchronized(lock) {
            if (entities.remove(attachmentUri(id)) == null) return
            seq++
        }
        println("[sim] discarded attachment $id")
        emit()
    }

    /** Ids of the attachments currently staged, for tests. */
    val stagedAttachments: List<String>
        get() = synchronized(lock) {
            entities.keys.filter { it.startsWith("construct://attachments/") }.map { it.removePrefix("construct://attachments/") }
        }

    /**
     * Lists a create with attachments as a pending placeholder (see the class docs). False (nothing changed) when its
     * entity URI is taken, its entity is not an object, or it names attachments that are not staged.
     */
    private fun createPostLocked(intent: ActionIntent.Create): Boolean {
        if (entities.containsKey(intent.entityUri) || intent.entity !is JsonObject) {
            println("[sim] create ${intent.entityUri} ignored")
            return false
        }
        val ids = intent.attachments.mapValues { (_, v) ->
            (v as? kotlinx.serialization.json.JsonArray)?.mapNotNull { (it as? JsonPrimitive)?.content }.orEmpty()
        }
        val missing = ids.values.flatten().filter { attachmentUri(it) !in entities }
        if (missing.isNotEmpty()) {
            println("[sim] create ${intent.entityUri} ignored: attachments $missing are not staged")
            return false
        }
        val uris = ids.mapValues { (_, list) -> kotlinx.serialization.json.JsonArray(list.map { JsonPrimitive(attachmentUri(it)) }) }
        val entity = substituteAttachments(intent.entity, uris) as JsonObject
        val payload = substituteAttachments(intent.payload, ids.mapValues { (_, list) -> kotlinx.serialization.json.JsonArray(list.map(::JsonPrimitive)) })
        entities[intent.entityUri] = entity
        postRows.addLast(CreateRow(intent.copy(entity = entity, payload = payload)))
        println("[sim] post ${intent.entityUri} in ${intent.queryKey} with ${ids.values.sumOf { it.size }} attachment(s) (posting; $pendingCount pending)")
        return true
    }

    /**
     * The fake pod finishes the posts in flight (the simulator's `wait` / `idle`): while online, each pending
     * placeholder becomes a finished post at the same URI and place (see the class docs). Returns how many.
     */
    fun settle(): Int {
        val n = synchronized(lock) {
            if (!isOnline || postRows.isEmpty()) return 0
            var n = 0
            while (postRows.isNotEmpty()) {
                val i = postRows.removeFirst().intent
                val finished = JsonObject(
                    i.entity.jsonObject - "pending_label" - "failed" + mapOf(
                        "id" to JsonPrimitive("sim-post-${++postCount}"),
                        "like_count" to JsonPrimitive(0),
                        "reply_count" to JsonPrimitive(0),
                        "is_liked" to JsonPrimitive(false),
                    ),
                )
                entities[i.entityUri] = finished
                server[i.entityUri] = finished
                if (i.position == CreatePosition.Start) {
                    postedFirst[i.queryKey] = listOf(i.entityUri) + postedFirst[i.queryKey].orEmpty()
                } else {
                    postedLast[i.queryKey] = postedLast[i.queryKey].orEmpty() + i.entityUri
                }
                println("[sim] sent ${i.endpoint}: ${i.payload}; ${i.entityUri} posted")
                n++
            }
            seq++
            n
        }
        emit()
        return n
    }

    override fun close() {
        notifications?.unwatch(notificationWatcher)
        notificationListener = null
        synchronized(lock) {
            listeners.clear()
        }
        println("[sim] engine for $appId closed")
    }

    override fun listApps(): String = appsCall { apps.appsJson() }

    override fun appVersions(appId: String, before: Long?, limit: Int?): String =
        appsCall { apps.versionsJson(appId, before, limit) }

    override fun appVersion(appId: String, version: Long): String = appsCall { apps.versionJson(appId, version) }

    override fun signIns(): String = appsCall {
        val auth = synchronized(lock) { authJson }
        val o = runCatching { ConstructJson.json.parseToJsonElement(auth).jsonObject }.getOrNull()
        if ((o?.get("state") as? JsonPrimitive)?.contentOrNull != "signed_in") return@appsCall """{"sign_ins":[]}"""
        val row = buildJsonObject {
            put("id", "pixelfed")
            put("title", "Pixelfed")
            put("server", SIGN_IN_SERVER)
            (o["username"] as? JsonPrimitive)?.contentOrNull?.let { put("username", it) }
            put("apps", buildJsonArray { add(buildJsonObject { put("id", "pixelfed_home"); put("title", "Pixelfed") }) })
        }
        buildJsonObject { put("sign_ins", buildJsonArray { add(row) }) }.toString()
    }

    private fun appsCall(read: () -> String): String {
        synchronized(lock) {
            if (!isOnline) {
                throw LayoutApiException(
                    "Can't reach your pod. Check your connection and try again.", LayoutApiException.Kind.Offline,
                )
            }
        }
        return read()
    }

    /**
     * Publishes a new newest version of [appId] ([FakeAppCatalog.publish]) and shows it at once, like the engine after
     * the pod's SSE `layout` event. Returns the new version number.
     */
    fun publishVersion(note: String): Long {
        val v = apps.publish(appId, note)
        synchronized(lock) {
            workspace = apps.latestLayout(appId) ?: workspace
            shownVersion = v
            seq++
        }
        println("[sim] published $appId v$v: $note")
        emit()
        return v
    }

    /** Current connectivity. */
    val online: Boolean get() = synchronized(lock) { isOnline }

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

    /** Number of queued (unsent) mutations and creates. */
    val pendingActions: Int get() = synchronized(lock) { pendingCount }

    private val pendingCount: Int get() = outbox.size + createRows.size + postRows.size

    /** The fake pod's answer for [path], for tests. */
    fun serverDataJson(path: String): String? = synchronized(lock) { serverData[path]?.toString() }

    /** Answers a data load from [serverData] (see the class docs). */
    private fun loadLocked(path: String) {
        if (!isOnline) {
            loads[path] = loadJson("error", "Can't reach your pod. Check your connection and try again.")
            return
        }
        val bundle = serverData[path]
        if (bundle == null) {
            loads[path] = loadJson("error", "This isn't available anymore.")
            return
        }
        // The server knows these entities from now on (a comment can be liked), and answers with what it changed.
        (bundle["entities"] as? JsonObject)?.forEach { (uri, e) ->
            (e as? JsonObject)?.let {
                val known = JsonObject(it + server[uri].orEmpty())
                server[uri] = known
                entities[uri] = known
            }
        }
        loadedQueries[path] = (bundle["queries"] as? JsonObject).orEmpty().mapValues { (_, v) ->
            (v as? kotlinx.serialization.json.JsonArray)?.mapNotNull { (it as? JsonPrimitive)?.content }.orEmpty()
        }
        loads[path] = loadJson("ready", null)
        println("[sim] loaded $path")
    }

    private fun loadJson(state: String, error: String?): JsonObject = JsonObject(
        buildMap {
            put("state", JsonPrimitive(state))
            error?.let { put("error", JsonPrimitive(it)) }
        },
    )

    /** The fake pod's `POST` for a create: accepted when a data path lists its query. */
    private fun sendCreateLocked(row: CreateRow) {
        val i = row.intent
        val paths = serverData.filterValues { (it["queries"] as? JsonObject)?.containsKey(i.queryKey) == true }.keys
        if (paths.isEmpty()) {
            println("[sim] server rejected ${i.endpoint} (no data path lists ${i.queryKey}); removing ${i.entityUri}")
            entities.remove(i.entityUri)
            return
        }
        val entity = i.entity as JsonObject
        val stored = if (entity.containsKey("meta")) JsonObject(entity + ("meta" to JsonPrimitive("just now"))) else entity
        val uri = "${i.entityUri}-sent-${++sentCount}"
        for (path in paths) {
            val b = serverData.getValue(path)
            val ents = (b["entities"] as? JsonObject).orEmpty() + (uri to stored)
            val queries = (b["queries"] as? JsonObject).orEmpty().toMutableMap()
            val list = (queries[i.queryKey] as? kotlinx.serialization.json.JsonArray).orEmpty() + JsonPrimitive(uri)
            queries[i.queryKey] = kotlinx.serialization.json.JsonArray(list)
            serverData[path] = JsonObject(mapOf("entities" to JsonObject(ents), "queries" to JsonObject(queries)))
        }
        println("[sim] sent ${i.endpoint}: ${i.entityUri} stored as $uri")
        entities.remove(i.entityUri)
        // The client reloads the paths it loaded that list the query.
        for (path in paths.filter { it in loadedQueries }) loadLocked(path)
    }

    /** Sends queued rows FIFO to the fake server (mutations, then creates). */
    private fun flushLocked() {
        flushMutationsLocked()
        while (createRows.isNotEmpty()) sendCreateLocked(createRows.removeFirst())
    }

    private fun flushMutationsLocked() {
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

    /** The bundle's queries, the loaded data paths' queries, and pending creates appended to theirs. */
    private fun queriesLocked(): String {
        val out = LinkedHashMap<String, List<String>>(
            ConstructJson.json.decodeFromString(queriesSerializer, queriesJson),
        )
        loadedQueries.values.forEach { out.putAll(it) }
        for ((key, uris) in postedFirst) out[key] = uris + out[key].orEmpty()
        for ((key, uris) in postedLast) out[key] = out[key].orEmpty() + uris
        // Pending rows: in dispatch order, so the newest `start` one ends up first.
        for (r in createRows + postRows) {
            val list = out[r.intent.queryKey].orEmpty()
            if (r.intent.entityUri in list) continue
            out[r.intent.queryKey] = if (r.intent.position == CreatePosition.Start) {
                listOf(r.intent.entityUri) + list
            } else {
                list + r.intent.entityUri
            }
        }
        return ConstructJson.json.encodeToString(queriesSerializer, out)
    }

    private fun snapshotLocked(): EngineSnapshot = EngineSnapshot(
        workspaceJson = ConstructJson.encodeNode(workspace),
        entitiesJson = ConstructJson.json.encodeToString(JsonObject.serializer(), JsonObject(entities)),
        isOnline = isOnline,
        seq = seq,
        pendingActions = pendingCount,
        queriesJson = queriesLocked(),
        loadsJson = JsonObject(loads).toString(),
        authJson = authJson,
        layoutJson = JsonObject(mapOf("app_id" to JsonPrimitive(appId), "version" to JsonPrimitive(shownVersion))).toString(),
        notificationsJson = notifications?.unreadJson() ?: NO_UNREAD,
    )

    private fun emit() {
        val (snapshot, targets) = synchronized(lock) { snapshotLocked() to listeners.toList() }
        targets.forEach { it(snapshot) }
    }

    companion object {
        /** The server of the fake's Pixelfed sign-in (fake-pixelfed's default address). */
        const val SIGN_IN_SERVER = "http://127.0.0.1:8081"

        private const val NO_UNREAD = "{\"unread_total\":0,\"unread_by_app\":{}}"
        private val queriesSerializer = MapSerializer(String.serializer(), ListSerializer(String.serializer()))

        /** [e] with every string exactly `$attachments.<name>` replaced by [values]`[name]` (others kept as they are). */
        fun substituteAttachments(e: JsonElement, values: Map<String, JsonElement>): JsonElement = when (e) {
            is JsonObject -> JsonObject(e.mapValues { (_, v) -> substituteAttachments(v, values) })
            is kotlinx.serialization.json.JsonArray -> kotlinx.serialization.json.JsonArray(e.map { substituteAttachments(it, values) })
            is JsonPrimitive -> if (e.isString && e.content.startsWith(ATTACHMENTS_PREFIX)) {
                values[e.content.removePrefix(ATTACHMENTS_PREFIX)] ?: e
            } else {
                e
            }
        }

        private const val ATTACHMENTS_PREFIX = "\$attachments."
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
                ConstructJson.json.parseToJsonElement(e)
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
