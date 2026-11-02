package org.genui.canvas

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
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
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.serialization.json.JsonObject

/**
 * UI state derived from engine snapshots. [workspace] is null until the engine has a layout.
 * [pendingActions] counts queued (unsent) outbox mutations. [seq] is the seq of the last applied
 * snapshot (-1 before the first). [queries] maps a FeedList `query_key` to ordered entity URIs (M6) and
 * [auth] is the sign-in status.
 */
data class CanvasState(
    val workspace: UiNode? = null,
    val entities: Map<String, JsonObject> = emptyMap(),
    val queries: Map<String, List<String>> = emptyMap(),
    val auth: AuthInfo = AuthInfo(),
    val isOnline: Boolean = false,
    val pendingActions: Int = 0,
    val error: String? = null,
    val seq: Long = -1,
)

/**
 * Bridges a [CanvasEngineBridge] to Compose. Snapshot callbacks may arrive on any thread;
 * [state] is a thread-safe [StateFlow]. A snapshot whose seq is not above the last applied one is
 * dropped (the engine notifies from several threads, so an older snapshot can arrive late); the check
 * and the write are one atomic [MutableStateFlow.update]. Decode failures are surfaced in
 * [CanvasState.error] (keeping the last good workspace/entities) instead of throwing into the engine.
 */
class CanvasController(private val bridge: CanvasEngineBridge) {
    private val _state = MutableStateFlow(CanvasState())
    val state: StateFlow<CanvasState> = _state.asStateFlow()

    init {
        bridge.subscribe(::onSnapshot)
    }

    private fun onSnapshot(snapshot: EngineSnapshot) {
        if (snapshot.seq <= _state.value.seq) return // cheap pre-check; re-checked atomically below
        var decodeError: String? = null
        val decoded: CanvasState? = try {
            val trimmed = snapshot.workspaceJson.trim()
            val workspace = if (trimmed.isEmpty() || trimmed == "null") null else CanvasJson.decodeNode(trimmed)
            val e = snapshot.entitiesJson.trim()
            val entities = if (e.isEmpty() || e == "null") emptyMap() else CanvasJson.decodeEntities(e)
            val queries = decodeQueries(snapshot.queriesJson)
            CanvasState(
                workspace, entities, queries, AuthInfo.parse(snapshot.authJson), snapshot.isOnline,
                snapshot.pendingActions, error = null, seq = snapshot.seq,
            )
        } catch (t: Throwable) {
            println("[Canvas] snapshot decode failed: $t")
            decodeError = "Failed to decode engine state: ${t.message}"
            null
        }
        _state.update { cur ->
            when {
                snapshot.seq <= cur.seq -> cur
                decoded != null -> decoded
                else -> cur.copy(
                    auth = AuthInfo.parse(snapshot.authJson),
                    isOnline = snapshot.isOnline,
                    pendingActions = snapshot.pendingActions,
                    error = decodeError,
                    seq = snapshot.seq,
                )
            }
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

    /** Signs out ("Sign out" in the status line); the engine notifies `signed_out` and refetches. */
    fun logout() {
        try {
            bridge.logout()
        } catch (t: Throwable) {
            println("[Canvas] logout failed: $t")
            _state.update { it.copy(error = "Sign-out failed: ${t.message}") }
        }
    }
}

@Composable
fun CanvasApp(controller: CanvasController, modifier: Modifier = Modifier) {
    val state by controller.state.collectAsState()
    // Material3 light scheme on a white background and surface (feed photos sit on white, as on Pixelfed/Instagram).
    MaterialTheme(colorScheme = lightColorScheme(background = Color.White, surface = Color.White)) {
        Surface(modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
            Column(Modifier.fillMaxSize()) {
                StatusLine(state.isOnline, state.pendingActions, state.auth, controller::logout)
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
                    CompositionLocalProvider(LocalCanvasQueries provides state.queries) {
                        RenderUiNode(workspace, state.entities, controller::dispatch, Modifier.fillMaxSize())
                    }
                }
            }
        }
    }
}

/**
 * Connectivity, plus "N pending" (amber dot) while queued mutations wait to be sent, plus the sign-in
 * status on the right unless signed out: "@username" and a "Sign out" button, "Signing in…", or the error.
 */
@Composable
private fun StatusLine(isOnline: Boolean, pendingActions: Int, auth: AuthInfo, onSignOut: () -> Unit) {
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
        if (pendingActions > 0) {
            Spacer(Modifier.width(16.dp))
            Box(Modifier.size(8.dp).background(Color(0xFFF9A825), CircleShape))
            Spacer(Modifier.width(6.dp))
            Text(
                "$pendingActions pending",
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        val authText = when (auth.state) {
            "signed_in" -> auth.username?.let { "@$it" } ?: "Signed in"
            "pending" -> "Signing in…"
            "error" -> "Sign-in failed" + (auth.error?.let { ": $it" } ?: "")
            else -> null
        }
        if (authText != null) {
            Spacer(Modifier.weight(1f))
            Text(
                authText,
                style = MaterialTheme.typography.labelMedium,
                color = if (auth.state == "error") MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false),
            )
            if (auth.state == "signed_in") {
                Spacer(Modifier.width(8.dp))
                Text(
                    "Sign out",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.primary,
                    maxLines = 1,
                    modifier = Modifier
                        .clip(RoundedCornerShape(4.dp))
                        .clickable(role = Role.Button, onClick = onSignOut)
                        .padding(horizontal = 4.dp, vertical = 2.dp),
                )
            }
        }
    }
}
