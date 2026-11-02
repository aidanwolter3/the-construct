package org.genui.canvas.android

import android.content.Context
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * Debug builds only: which server the engine talks to, chosen in the app and kept in SharedPreferences.
 * Precedence (EngineConfig.from): `--es baseUrl` intent extra > this saved choice > BuildConfig.CANVAS_BASE_URL
 * (the staging pod unless built with -PcanvasBaseUrl). A new choice applies on the next engine creation, which the
 * picker triggers by restarting the activity.
 */
internal object DebugServer {
    private const val PREFS = "debug_server"
    private const val KEY = "baseUrl"

    val presets = listOf(
        "Staging" to BuildConfig.STAGING_BASE_URL,
        "Local (emulator)" to "http://10.0.2.2:8080",
        "Local (USB, adb reverse)" to "http://127.0.0.1:8080",
    )

    fun saved(context: Context): String? =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(KEY, null)?.takeIf { it.isNotBlank() }

    /** null clears the choice (back to the BuildConfig default). */
    fun save(context: Context, url: String?) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().apply {
            if (url == null) remove(KEY) else putString(KEY, url)
        }.commit()
    }

    fun label(url: String): String = presets.firstOrNull { it.second == url }?.first ?: url

    /** The normalized URL if [input] is an http(s) URL with a host (no path beyond `/`), else null. */
    fun parse(input: String): String? {
        val url = input.trim().trimEnd('/')
        val uri = runCatching { java.net.URI(url) }.getOrNull() ?: return null
        if (uri.scheme != "http" && uri.scheme != "https") return null
        if (uri.host.isNullOrEmpty() || !uri.rawPath.isNullOrEmpty() || uri.rawQuery != null || uri.rawFragment != null) return null
        return url
    }
}

/** A small "server: …" chip; tap for the menu. [onPick] gets the new URL (null = build default). */
@Composable
internal fun DebugServerChip(current: String, onPick: (String?) -> Unit, modifier: Modifier = Modifier) {
    // Saveable: the menu and a half-typed custom URL survive rotation.
    var menu by rememberSaveable { mutableStateOf(false) }
    var custom by rememberSaveable { mutableStateOf<String?>(null) }
    Box(modifier) {
        Surface(
            onClick = { menu = true },
            shape = androidx.compose.foundation.shape.RoundedCornerShape(12.dp),
            tonalElevation = 2.dp,
            shadowElevation = 2.dp,
            modifier = Modifier.alpha(0.85f),
        ) {
            Text(
                "server: ${DebugServer.label(current)}",
                style = TextStyle(fontSize = 11.sp),
                modifier = Modifier.padding(horizontal = 8.dp, vertical = 4.dp),
            )
        }
        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
            DebugServer.presets.forEach { (name, url) ->
                DropdownMenuItem(
                    text = { Column { Text(name); Text(url, style = TextStyle(fontSize = 11.sp)) } },
                    onClick = { menu = false; onPick(url) },
                )
            }
            DropdownMenuItem(text = { Text("Custom…") }, onClick = { menu = false; custom = current })
            DropdownMenuItem(
                text = { Text("Build default (${DebugServer.label(BuildConfig.CANVAS_BASE_URL)})") },
                onClick = { menu = false; onPick(null) },
            )
        }
    }
    custom?.let { text ->
        AlertDialog(
            onDismissRequest = { custom = null },
            title = { Text("Server URL") },
            text = {
                val invalid = DebugServer.parse(text) == null
                OutlinedTextField(
                    value = text,
                    onValueChange = { custom = it },
                    singleLine = true,
                    isError = invalid,
                    supportingText = { if (invalid) Text("http(s)://host[:port]") },
                )
            },
            confirmButton = {
                val url = DebugServer.parse(text)
                TextButton(enabled = url != null, onClick = { custom = null; url?.let(onPick) }) { Text("Use") }
            },
            dismissButton = { TextButton(onClick = { custom = null }) { Text("Cancel") } },
        )
    }
}
