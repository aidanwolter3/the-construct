package org.theconstruct

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.longOrNull

// Pure helpers behind the Pixelfed widgets (unit-tested in commonTest).

/**
 * The items a `FeedList` renders, each the entity's fields plus a synthetic `uri` field.
 * With `queries[queryKey]` present (M6, DECISIONS D7): those URIs in order, de-duplicated, skipping URIs
 * that have no entity. Without it (older bundles): every entity whose URI contains `/posts/`, newest
 * `created_at` first (D7 fallback).
 */
fun feedItems(
    queryKey: String,
    queries: Map<String, List<String>>,
    entities: Map<String, JsonObject>,
): List<JsonObject> {
    val uris = queries[queryKey]?.distinct()
        ?: entities.entries
            .filter { it.key.contains("/posts/") }
            .sortedByDescending { (it.value["created_at"] as? JsonPrimitive)?.longOrNull ?: 0L }
            .map { it.key }
    return uris.mapNotNull { uri -> entities[uri]?.let { JsonObject(it + ("uri" to JsonPrimitive(uri))) } }
}

const val MIN_CAROUSEL_ASPECT = 0.8f
const val MAX_CAROUSEL_ASPECT = 1.91f

/**
 * Width/height ratio of a `MediaCarousel`: the first media entity's `aspect_ratio` clamped to
 * [MIN_CAROUSEL_ASPECT]..[MAX_CAROUSEL_ASPECT] (Instagram's 4:5 portrait to 1.91:1 landscape), else the node's
 * [nodeAspect], else 1 (square) when that is not a positive finite number either.
 */
fun carouselAspectRatio(firstMedia: JsonObject?, nodeAspect: Float): Float {
    val a = (firstMedia?.get("aspect_ratio") as? JsonPrimitive)?.takeIf { !it.isString }?.doubleOrNull?.toFloat()
    if (a != null && a.isFinite() && a > 0f) return a.coerceIn(MIN_CAROUSEL_ASPECT, MAX_CAROUSEL_ASPECT)
    return nodeAspect.takeIf { it.isFinite() && it > 0f } ?: 1f
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
 */
data class AuthInfo(
    val state: String = "signed_out",
    val username: String? = null,
    val error: String? = null,
    val message: String? = null,
) {
    companion object {
        /** Lenient: anything unparsable reads as signed out. */
        fun parse(json: String): AuthInfo = try {
            val o = ConstructJson.json.parseToJsonElement(json).jsonObject
            fun str(k: String) = (o[k] as? JsonPrimitive)?.takeIf { it !is JsonNull }?.contentOrNull
            AuthInfo(str("state") ?: "signed_out", str("username"), str("error"), str("message"))
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
