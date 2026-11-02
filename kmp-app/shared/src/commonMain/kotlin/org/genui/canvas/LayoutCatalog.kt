package org.genui.canvas

import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonObject

/**
 * The layout on display (DECISIONS D26/D27), from [EngineSnapshot.layoutJson]: the active catalog layout's [layoutId],
 * its own [layoutVersion] and the workspace [revision]. All null while unknown or on an unversioned screen (the
 * signed-out `pixelfed_home`).
 */
data class ActiveLayout(
    val layoutId: String? = null,
    val layoutVersion: Int? = null,
    val revision: Int? = null,
) {
    companion object {
        /** Lenient: anything unparsable reads as unknown. */
        fun parse(json: String): ActiveLayout = try {
            val o = CanvasJson.json.parseToJsonElement(json).jsonObject
            fun prim(k: String) = (o[k] as? JsonPrimitive)?.takeIf { it !is JsonNull }
            ActiveLayout(
                layoutId = prim("layout_id")?.takeIf { it.isString }?.contentOrNull?.takeIf { it.isNotBlank() },
                layoutVersion = prim("layout_version")?.takeIf { !it.isString }?.intOrNull,
                revision = prim("revision")?.takeIf { !it.isString }?.intOrNull,
            )
        } catch (t: Exception) {
            ActiveLayout()
        }
    }
}

/**
 * A layout catalog call failed. [message] is a short sentence for the UI ("Sign in to change layouts.", "Can't reach
 * your pod. ..."); [kind] tells the cases apart; [status] is the HTTP status for [Kind.Server] (0 if none).
 */
class LayoutApiException(
    override val message: String,
    val kind: Kind = Kind.Server,
    val status: Int = 0,
) : Exception(message) {
    enum class Kind { Offline, Unauthorized, NotFound, Server }
}
