package org.genui.canvas

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.serialization.json.JsonObject

/** UI state derived from engine snapshots. [workspace] is null until the engine has a layout. */
data class CanvasState(
    val workspace: UiNode? = null,
    val entities: Map<String, JsonObject> = emptyMap(),
    val isOnline: Boolean = false,
    val error: String? = null,
)

/**
 * Bridges a [CanvasEngineBridge] to Compose. Snapshot callbacks may arrive on any thread;
 * [state] is a thread-safe [StateFlow]. Decode failures are surfaced in [CanvasState.error]
 * (keeping the last good workspace/entities) instead of throwing into the engine.
 */
class CanvasController(private val bridge: CanvasEngineBridge) {
    private val _state = MutableStateFlow(CanvasState())
    val state: StateFlow<CanvasState> = _state.asStateFlow()

    init {
        bridge.subscribe(::onSnapshot)
    }

    private fun onSnapshot(workspaceJson: String, entitiesJson: String, isOnline: Boolean) {
        try {
            val trimmed = workspaceJson.trim()
            val workspace = if (trimmed.isEmpty() || trimmed == "null") null else CanvasJson.decodeNode(trimmed)
            val e = entitiesJson.trim()
            val entities = if (e.isEmpty() || e == "null") emptyMap() else CanvasJson.decodeEntities(e)
            _state.value = CanvasState(workspace, entities, isOnline, error = null)
        } catch (t: Throwable) {
            println("[Canvas] snapshot decode failed: $t")
            _state.update { it.copy(isOnline = isOnline, error = "Failed to decode engine state: ${t.message}") }
        }
    }

    /** Encodes [intent] and forwards it to the engine. */
    fun dispatch(intent: ActionIntent) {
        val json = CanvasJson.encodeIntent(intent)
        // Logging is the bridge's job (desktop prints "[desktop] dispatch: <json>").
        try {
            bridge.dispatch(json)
        } catch (t: Throwable) {
            println("[Canvas] dispatch failed: $t")
            _state.update { it.copy(error = "Dispatch failed: ${t.message}") }
        }
    }
}

@Composable
fun CanvasApp(controller: CanvasController, modifier: Modifier = Modifier) {
    val state by controller.state.collectAsState()
    MaterialTheme {
        Surface(modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
            Column(Modifier.fillMaxSize()) {
                StatusLine(state.isOnline)
                state.error?.let {
                    Text(
                        it,
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
                    )
                }
                val workspace = state.workspace
                if (workspace == null) {
                    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                        Text("Loading…", style = MaterialTheme.typography.bodyLarge)
                    }
                } else {
                    RenderUiNode(workspace, state.entities, controller::dispatch, Modifier.fillMaxSize())
                }
            }
        }
    }
}

@Composable
private fun StatusLine(isOnline: Boolean) {
    Row(
        Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surfaceVariant).padding(horizontal = 16.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.size(8.dp).background(if (isOnline) Color(0xFF2E7D32) else Color(0xFF9E9E9E), CircleShape))
        Spacer(Modifier.width(8.dp))
        Text(
            if (isOnline) "Online" else "Offline",
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}
