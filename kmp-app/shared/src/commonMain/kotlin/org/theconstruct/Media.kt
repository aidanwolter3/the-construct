package org.theconstruct

import androidx.compose.runtime.ProvidableCompositionLocal
import androidx.compose.runtime.staticCompositionLocalOf
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonObject

// Photo picking for `core://MediaPicker` (DECISIONS D49, D51; docs/POSTING.md). Nothing here knows what the photos are
// for: the picker stages files with the engine and keeps their ids in the form; a create's `attachments` sends them.

/**
 * A staged attachment ([ConstructEngineBridge.stageAttachment]): [uri] is its local entity
 * (`construct://attachments/<id>`), [url] a `file://` URL of the prepared JPEG.
 */
data class AttachmentInfo(
    val id: String,
    val uri: String,
    val url: String,
    val width: Int? = null,
    val height: Int? = null,
) {
    companion object {
        /** The engine's `{"id","uri","url","width","height"}`; throws [AttachmentException] when it does not parse. */
        fun parse(json: String): AttachmentInfo {
            val o = try {
                ConstructJson.json.parseToJsonElement(json).jsonObject
            } catch (e: Exception) {
                throw AttachmentException("The engine sent an answer this app can't read.")
            }
            fun str(k: String) = (o[k] as? JsonPrimitive)?.takeIf { it.isString }?.contentOrNull
            val id = str("id") ?: throw AttachmentException("The engine sent an answer this app can't read.")
            return AttachmentInfo(
                id = id,
                uri = str("uri") ?: attachmentUri(id),
                url = str("url").orEmpty(),
                width = (o["width"] as? JsonPrimitive)?.intOrNull,
                height = (o["height"] as? JsonPrimitive)?.intOrNull,
            )
        }
    }
}

/** The local entity URI of staged attachment [id]. */
fun attachmentUri(id: String): String = "construct://attachments/$id"

/** Staging a file failed; [message] is a readable reason (the picker shows it). */
class AttachmentException(message: String) : Exception(message)

/**
 * The platform's photo picker (desktop: a file dialog; Android: the system Photo Picker; the simulator: queued files).
 * Provided to the shared UI through [ConstructApp]'s `mediaPicker`.
 */
interface MediaPickerPlatform {
    /** Lets the user pick up to [max] photos; returns local file paths, empty when cancelled. */
    suspend fun pick(max: Int): List<String>
}

/** The platform picker [ConstructApp] was given (null: picking photos is not available here). */
val LocalMediaPickerPlatform: ProvidableCompositionLocal<MediaPickerPlatform?> = staticCompositionLocalOf { null }

/** The controller's [Attachments], provided by [ConstructApp]. */
internal val LocalAttachments: ProvidableCompositionLocal<Attachments?> = staticCompositionLocalOf { null }

/**
 * Picks and stagings in flight anywhere in the process (like [LayoutCalls]): the simulator waits for 0 after a click,
 * since they run on a background dispatcher the Compose test clock does not see.
 */
object MediaCalls {
    private val _inFlight = MutableStateFlow(0)
    val inFlight: StateFlow<Int> = _inFlight.asStateFlow()

    internal fun add(delta: Int) = _inFlight.update { it + delta }
}

/**
 * The attachments this client staged and no create has taken yet, owned by [ConstructController]. [stage] runs the
 * blocking engine call on [io]. Ids left in a screen's form when the screen closes without a create are discarded
 * ([release]); a dispatched create takes its ids over ([consume]), the engine then owns their files.
 */
class Attachments internal constructor(
    private val bridge: () -> ConstructEngineBridge,
    private val io: CoroutineDispatcher = layoutIoDispatcher,
) {
    private val lock = Any()
    private val staged = LinkedHashMap<String, AttachmentInfo>()

    /** Stages the file at [path] (decoded, scaled, re-encoded by the engine); the failure's message is readable. */
    suspend fun stage(path: String): Result<AttachmentInfo> {
        MediaCalls.add(1)
        try {
            val result = withContext(io) {
                try {
                    bridge().stageAttachment(path)
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Exception) {
                    Result.failure(AttachmentException(e.message ?: "Couldn't add that photo."))
                }
            }
            result.onSuccess { info -> synchronized(lock) { staged[info.id] = info } }
            return result
        } finally {
            MediaCalls.add(-1)
        }
    }

    /** What staging [id] answered, while it is staged here. */
    fun info(id: String): AttachmentInfo? = synchronized(lock) { staged[id] }

    /** Removes a picked photo: the engine deletes its file and entity. */
    fun discard(id: String) {
        synchronized(lock) { staged.remove(id) }
        try {
            bridge().discardAttachment(id)
        } catch (t: Throwable) {
            println("[Construct] discarding attachment $id failed: $t")
        }
    }

    /** A create took [ids] over: they are no longer discarded with their form. */
    internal fun consume(ids: Collection<String>) {
        synchronized(lock) { ids.forEach { staged.remove(it) } }
    }

    /** A screen closed with [form]: discards the attachments still listed in its list values. */
    internal fun release(form: Map<String, String>) {
        val ids = form.values.flatMap { parseFormList(it).orEmpty() }
        val mine = synchronized(lock) { ids.filter { it in staged } }
        mine.forEach(::discard)
    }

    /** The engine went away (another app opened): nothing left to discard there. */
    internal fun forgetAll() {
        synchronized(lock) { staged.clear() }
    }
}

/** The ids of [attachments] (an `action.mutation.create`'s, after [bindForm]: arrays of ids). */
internal fun attachmentIds(attachments: Map<String, kotlinx.serialization.json.JsonElement>): List<String> =
    attachments.values.flatMap { v ->
        (v as? kotlinx.serialization.json.JsonArray)?.mapNotNull { (it as? JsonPrimitive)?.contentOrNull }.orEmpty()
    }

/** The thumbnail URL of attachment [id]: its local entity's `preview_url` or `url`, else what staging answered. */
internal fun attachmentUrl(id: String, entities: Map<String, JsonObject>, attachments: Attachments?): String? {
    val e = entities[attachmentUri(id)]
    fun field(k: String) = (e?.get(k) as? JsonPrimitive)?.takeIf { it.isString }?.contentOrNull?.takeIf { it.isNotBlank() }
    return field("preview_url") ?: field("url") ?: attachments?.info(id)?.url?.takeIf { it.isNotBlank() }
}
