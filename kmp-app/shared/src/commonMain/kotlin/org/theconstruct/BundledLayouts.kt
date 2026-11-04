package org.theconstruct

import kotlinx.serialization.json.jsonObject

/**
 * The newest layout file of each app in `layouts/` at build time (generated into [BundledLayoutFiles] by
 * `:shared:generateBundledLayouts`): what a launcher draws while its engine has no workspace yet, e.g. the first start
 * offline or against a pod without the app (docs/decisions/launcher-home-app.md). Platforms pick which apps get one
 * ([ConstructController]'s `fallbackLayouts`).
 */
object BundledLayouts {
    /** The ids of the apps with a bundled layout. */
    val appIds: Set<String> get() = BundledLayoutFiles.files.keys

    /** The file number of [appId]'s bundled layout, or null. */
    fun number(appId: String): Int? = BundledLayoutFiles.files[appId]?.first

    /** [appId]'s bundled layout (the file's `layout`), or null when there is none or it doesn't decode. */
    fun layout(appId: String): UiNode? {
        val text = BundledLayoutFiles.files[appId]?.second ?: return null
        return try {
            val doc = ConstructJson.json.parseToJsonElement(text).jsonObject
            doc["layout"]?.let { ConstructJson.json.decodeFromJsonElement(UiNode.serializer(), it) }
        } catch (t: Throwable) {
            println("[Construct] bundled layout of $appId unreadable: $t")
            null
        }
    }

    /** The bundled layouts of [appIds] that exist and decode. */
    fun forApps(vararg appIds: String?): Map<String, UiNode> =
        appIds.filterNotNull().distinct().mapNotNull { id -> layout(id)?.let { id to it } }.toMap()
}
