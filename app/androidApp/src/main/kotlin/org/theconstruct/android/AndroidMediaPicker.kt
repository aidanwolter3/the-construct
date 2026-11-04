package org.theconstruct.android

import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.ImageDecoder
import android.net.Uri
import android.os.Build
import android.util.Log
import android.webkit.MimeTypeMap
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContract
import androidx.activity.result.contract.ActivityResultContracts.PickMultipleVisualMedia
import androidx.activity.result.contract.ActivityResultContracts.PickVisualMedia
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.theconstruct.MediaPickerPlatform
import java.io.File
import java.util.UUID

private const val TAG = "ConstructPicker"

/** Picked copies live in `cacheDir/picked`; older ones than this are deleted when the next pick starts. */
private const val PICKED_MAX_AGE_MS = 60 * 60 * 1000L

/** HEIC/HEIF photos are decoded to at most this longest side before being re-encoded as JPEG (the engine scales further). */
private const val HEIF_MAX_SIDE = 4096

/**
 * The Android photo picker for `core://MediaPicker`: the system Photo Picker (images only, up to the picker's `max`;
 * on devices without it, the documents UI), registered with the activity's result registry by Compose. Each picked
 * Uri is copied into `cacheDir/picked` (HEIC/HEIF converted to JPEG, which the engine can decode) and its path
 * returned. A result arriving after the activity was recreated (rotation while picking) is lost: the pick returns
 * nothing.
 */
@Composable
internal fun rememberAndroidMediaPicker(): MediaPickerPlatform {
    val context = LocalContext.current.applicationContext
    val holder = remember { PendingPick() }
    val launcher = rememberLauncherForActivityResult(PickImages()) { uris -> holder.complete(uris) }
    return remember(launcher) { AndroidMediaPicker(context, launcher, holder) }
}

/** The one pick waiting for the picker's result. */
private class PendingPick {
    private var deferred: CompletableDeferred<List<Uri>>? = null

    @Synchronized
    fun start(): CompletableDeferred<List<Uri>> {
        deferred?.complete(emptyList())
        return CompletableDeferred<List<Uri>>().also { deferred = it }
    }

    @Synchronized
    fun complete(uris: List<Uri>) {
        deferred?.complete(uris)
        deferred = null
    }
}

/** Input: the most images to pick. One opens the single-photo picker (the multiple one needs at least two). */
private class PickImages : ActivityResultContract<Int, List<Uri>>() {
    private val request = PickVisualMediaRequest(PickVisualMedia.ImageOnly)

    override fun createIntent(context: Context, input: Int): Intent =
        if (input > 1) PickMultipleVisualMedia(input).createIntent(context, request) else PickVisualMedia().createIntent(context, request)

    // Handles both shapes: a single `data` Uri and a multi-select ClipData.
    override fun parseResult(resultCode: Int, intent: Intent?): List<Uri> = PickMultipleVisualMedia().parseResult(resultCode, intent)
}

private class AndroidMediaPicker(
    private val context: Context,
    private val launcher: ActivityResultLauncher<Int>,
    private val holder: PendingPick,
) : MediaPickerPlatform {
    override suspend fun pick(max: Int): List<String> {
        val result = holder.start()
        launcher.launch(max.coerceAtLeast(1))
        val uris = result.await().take(max.coerceAtLeast(1))
        if (uris.isEmpty()) return emptyList()
        return withContext(Dispatchers.IO) {
            val dir = File(context.cacheDir, "picked").apply { mkdirs() }
            val now = System.currentTimeMillis()
            dir.listFiles()?.filter { now - it.lastModified() > PICKED_MAX_AGE_MS }?.forEach { it.delete() }
            uris.mapNotNull { uri ->
                try {
                    copy(uri, dir)
                } catch (e: Exception) {
                    Log.w(TAG, "cannot read a picked photo: $e")
                    null
                }
            }
        }
    }

    /** Copies [uri] into [dir] (HEIC/HEIF as JPEG); returns the copy's path. */
    private fun copy(uri: Uri, dir: File): String {
        val resolver = context.contentResolver
        val mime = resolver.getType(uri)?.lowercase().orEmpty()
        val heif = mime == "image/heic" || mime == "image/heif" || mime == "image/heic-sequence" || mime == "image/heif-sequence"
        if (heif && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            val bitmap = ImageDecoder.decodeBitmap(ImageDecoder.createSource(resolver, uri)) { decoder, info, _ ->
                val side = maxOf(info.size.width, info.size.height)
                var sample = 1
                while (side / sample > HEIF_MAX_SIDE) sample *= 2
                decoder.setTargetSampleSize(sample)
                decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
            }
            val out = File(dir, "${UUID.randomUUID()}.jpg")
            try {
                out.outputStream().use { bitmap.compress(Bitmap.CompressFormat.JPEG, 92, it) }
            } finally {
                bitmap.recycle()
            }
            return out.absolutePath
        }
        val ext = MimeTypeMap.getSingleton().getExtensionFromMimeType(mime) ?: "img"
        val out = File(dir, "${UUID.randomUUID()}.$ext")
        val input = resolver.openInputStream(uri) ?: throw IllegalStateException("no stream for $uri")
        input.use { i -> out.outputStream().use { o -> i.copyTo(o) } }
        return out.absolutePath
    }
}
