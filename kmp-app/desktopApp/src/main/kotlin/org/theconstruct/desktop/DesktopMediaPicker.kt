package org.theconstruct.desktop

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.theconstruct.MediaPickerPlatform
import java.awt.FileDialog
import java.awt.Frame
import java.io.File

/** Image files the desktop picker offers (the engine checks the content when staging). */
internal val PICKABLE_EXTENSIONS = setOf("jpg", "jpeg", "png", "webp", "gif")

/** Whether [file] has one of the [PICKABLE_EXTENSIONS]. */
internal fun isPickableImage(file: File): Boolean = file.extension.lowercase() in PICKABLE_EXTENSIONS

/**
 * The desktop photo picker: the native AWT file dialog (multi-select, image files only), shown over [parent]. The
 * dialog is modal and blocks its thread, so it runs on Dispatchers.IO rather than the UI thread. Returns at most [pick]'s
 * `max` paths (the dialog itself can't limit the count).
 */
class DesktopMediaPicker(private val parent: () -> Frame?) : MediaPickerPlatform {
    override suspend fun pick(max: Int): List<String> = withContext(Dispatchers.IO) {
        val dialog = FileDialog(parent(), if (max > 1) "Choose photos" else "Choose a photo", FileDialog.LOAD).apply {
            isMultipleMode = max > 1
            // Honoured on macOS and Linux; the Windows dialog ignores filename filters, hence the check below.
            setFilenameFilter { _, name -> File(name).extension.lowercase() in PICKABLE_EXTENSIONS }
        }
        try {
            dialog.isVisible = true
            dialog.files.orEmpty().filter(::isPickableImage).take(max.coerceAtLeast(1)).map { it.absolutePath }
        } finally {
            dialog.dispose()
        }
    }
}
