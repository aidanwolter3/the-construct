package org.theconstruct

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.longOrNull

// Pure helpers behind the generic widgets (unit-tested in commonTest).

/**
 * Width/height ratio of a `core://Carousel`: the first item's [field] (a positive number), clamped to
 * [min]..[max] when given, else the node's [fallback], else 1 (square) when that is not a positive finite number
 * either.
 */
fun carouselAspectRatio(first: JsonObject?, field: String?, min: Double?, max: Double?, fallback: Double): Float {
    val a = field?.let { (first?.get(it) as? JsonPrimitive)?.takeIf { p -> !p.isString }?.doubleOrNull }
    if (a != null && a.isFinite() && a > 0.0) {
        var r = a
        if (min != null) r = maxOf(r, min)
        if (max != null) r = minOf(r, max)
        return r.toFloat()
    }
    return fallback.takeIf { it.isFinite() && it > 0.0 }?.toFloat() ?: 1f
}

private const val BASE83 = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz#\$%*+,-.:;=?@[]^_{|}~"

/**
 * The average colour of a BlurHash (its DC component, characters 2..5), as 0xRRGGBB, or null if [hash] is
 * not a plausible BlurHash. Used as the image placeholder colour.
 */
fun blurhashAverageRgb(hash: String?): Int? {
    if (hash == null || hash.length < 6) return null
    var v = 0
    for (c in hash.substring(2, 6)) {
        val d = BASE83.indexOf(c)
        if (d < 0) return null
        v = v * 83 + d
    }
    return v.takeIf { it in 0..0xFFFFFF }
}

/**
 * Sign-in status from the engine snapshot's `auth_json` (M6). [state]: signed_out, pending, signed_in, error.
 * [message]: optional progress text while pending (e.g. "Loading your feed…" after the code exchange).
 * [session]: the engine holds a session (a pod token, D46), with or without an app sign-in.
 */
data class AuthInfo(
    val state: String = "signed_out",
    val username: String? = null,
    val error: String? = null,
    val message: String? = null,
    val session: Boolean = false,
) {
    companion object {
        /** Lenient: anything unparsable reads as signed out. */
        fun parse(json: String): AuthInfo = try {
            val o = ConstructJson.json.parseToJsonElement(json).jsonObject
            fun str(k: String) = (o[k] as? JsonPrimitive)?.takeIf { it !is JsonNull }?.contentOrNull
            AuthInfo(
                str("state") ?: "signed_out",
                str("username"),
                str("error"),
                str("message"),
                session = (o["session"] as? JsonPrimitive)?.booleanOrNull == true,
            )
        } catch (t: Exception) {
            AuthInfo()
        }
    }
}

/** Decodes the snapshot's `queries_json`; non-array values and non-string URIs are skipped. */
fun decodeQueries(json: String): Map<String, List<String>> {
    val t = json.trim()
    if (t.isEmpty() || t == "null") return emptyMap()
    val o = ConstructJson.json.parseToJsonElement(t).jsonObject
    return o.mapNotNull { (k, v) ->
        (v as? JsonArray)?.let { arr ->
            k to arr.mapNotNull { (it as? JsonPrimitive)?.takeIf { p -> p.isString }?.content }
        }
    }.toMap()
}

/** A string field, or null when missing, null or blank. */
internal fun JsonObject.str(field: String): String? =
    (this[field] as? JsonPrimitive)?.takeIf { it !is JsonNull }?.content?.takeIf { it.isNotBlank() }

/** A boolean field (JSON true only). */
internal fun JsonObject.flag(field: String): Boolean =
    (this[field] as? JsonPrimitive)?.takeIf { !it.isString && it !is JsonNull }?.content == "true"
