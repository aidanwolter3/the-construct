package org.genui.canvas

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleIn
import androidx.compose.animation.scaleOut
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SuggestionChip
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonObject

/**
 * Layout agent status from the snapshot's `agent_json` (M7). [state]: idle, running, done, error. [layoutVersion] is
 * the layout version the last successful prompt produced.
 */
data class AgentInfo(
    val state: String = "idle",
    val message: String? = null,
    val provider: String? = null,
    val layoutVersion: Int? = null,
) {
    val isRunning: Boolean get() = state == "running"

    companion object {
        /** Lenient: anything unparsable reads as idle. */
        fun parse(json: String): AgentInfo = try {
            val o = CanvasJson.json.parseToJsonElement(json).jsonObject
            fun prim(k: String) = (o[k] as? JsonPrimitive)?.takeIf { it !is JsonNull }
            AgentInfo(
                state = prim("state")?.contentOrNull ?: "idle",
                message = prim("message")?.contentOrNull?.takeIf { it.isNotBlank() },
                provider = prim("provider")?.contentOrNull?.takeIf { it.isNotBlank() },
                layoutVersion = prim("layout_version")?.takeIf { !it.isString }?.intOrNull,
            )
        } catch (t: Exception) {
            AgentInfo()
        }
    }
}

/**
 * The prompt panel's status line: "Applying...", "Layout updated (v3, rules)" (version and provider when known), the
 * error message, or null when idle.
 */
fun agentStatusText(agent: AgentInfo): String? = when (agent.state) {
    "running" -> "Applying..."
    "done" -> {
        val details = listOfNotNull(agent.layoutVersion?.let { "v$it" }, agent.provider).joinToString(", ")
        if (details.isEmpty()) "Layout updated" else "Layout updated ($details)"
    }
    "error" -> agent.message ?: "The layout agent failed"
    else -> null
}

/** Suggestion chips under the prompt field; tapping one replaces the field's text. */
val AGENT_SUGGESTIONS = listOf("Hide captions", "Media at the very top", "Add a weather widget")

/**
 * The in-app layout agent (M7, DECISIONS D23): a FAB (tag `agent-fab`, content description "Edit layout with AI")
 * that opens a bottom panel with a prompt field (`agent-prompt`), suggestion chips, a Send button (`agent-send`) and
 * the agent status. Lives outside the server-driven tree ([CanvasApp] draws it over the workspace), so a layout
 * swap never resets the prompt or the open panel. The prompt text is cleared only when the prompt this panel sent
 * succeeds; on error it stays for editing.
 */
@Composable
internal fun AgentPromptOverlay(agent: AgentInfo, onSend: (String) -> Unit, modifier: Modifier = Modifier) {
    var open by rememberSaveable { mutableStateOf(false) }
    var text by rememberSaveable { mutableStateOf("") }
    // The agent status when this panel last sent a prompt; null when no send is awaiting its result.
    var sentFrom by remember { mutableStateOf<AgentInfo?>(null) }
    LaunchedEffect(agent) {
        val from = sentFrom ?: return@LaunchedEffect
        if (agent == from) return@LaunchedEffect
        when (agent.state) {
            "done" -> { text = ""; sentFrom = null }
            "error", "idle" -> sentFrom = null
        }
    }
    val canSend = text.isNotBlank() && !agent.isRunning
    val send = {
        if (canSend) {
            sentFrom = agent
            onSend(text.trim())
        }
    }

    Box(modifier) {
        AnimatedVisibility(
            visible = !open,
            enter = fadeIn() + scaleIn(),
            exit = fadeOut() + scaleOut(),
            modifier = Modifier.align(Alignment.BottomEnd).padding(16.dp),
        ) {
            FloatingActionButton(
                onClick = { open = true },
                containerColor = MaterialTheme.colorScheme.primaryContainer,
                modifier = Modifier.testTag("agent-fab"),
            ) {
                Icon(CanvasIcons.AutoAwesome, contentDescription = "Edit layout with AI")
            }
        }
        AnimatedVisibility(
            visible = open,
            enter = slideInVertically { it } + fadeIn(),
            exit = slideOutVertically { it } + fadeOut(),
            modifier = Modifier.align(Alignment.BottomCenter),
        ) {
            Surface(
                shape = RoundedCornerShape(topStart = 20.dp, topEnd = 20.dp),
                color = MaterialTheme.colorScheme.surfaceContainerLow,
                shadowElevation = 12.dp,
                tonalElevation = 2.dp,
                modifier = Modifier.widthIn(max = FEED_MAX_WIDTH).fillMaxWidth().testTag("agent-panel"),
            ) {
                Column(Modifier.padding(start = 16.dp, end = 8.dp, top = 8.dp, bottom = 16.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Icon(
                            CanvasIcons.AutoAwesome,
                            contentDescription = null,
                            tint = MaterialTheme.colorScheme.primary,
                            modifier = Modifier.size(20.dp),
                        )
                        Spacer(Modifier.width(8.dp))
                        Text(
                            "Edit layout with AI",
                            style = MaterialTheme.typography.titleMedium,
                            fontWeight = FontWeight.SemiBold,
                            modifier = Modifier.weight(1f),
                        )
                        IconButton(onClick = { open = false }, modifier = Modifier.testTag("agent-close")) {
                            Icon(CanvasIcons.Close, contentDescription = "Close", modifier = Modifier.size(22.dp))
                        }
                    }
                    Column(Modifier.padding(end = 8.dp)) {
                        OutlinedTextField(
                            value = text,
                            onValueChange = { text = it },
                            placeholder = { Text("Describe a change, e.g. \"Hide captions\"") },
                            minLines = 2,
                            maxLines = 4,
                            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                            keyboardActions = KeyboardActions(onSend = { send() }),
                            modifier = Modifier.fillMaxWidth().testTag("agent-prompt"),
                        )
                        FlowRow(
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                            modifier = Modifier.padding(top = 4.dp),
                        ) {
                            AGENT_SUGGESTIONS.forEach { s ->
                                SuggestionChip(onClick = { text = s }, label = { Text(s) })
                            }
                        }
                        Row(Modifier.padding(top = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                            val status = agentStatusText(agent)
                            Text(
                                status.orEmpty(),
                                style = MaterialTheme.typography.bodySmall,
                                color = when (agent.state) {
                                    "error" -> MaterialTheme.colorScheme.error
                                    "done" -> Color(0xFF2E7D32)
                                    else -> MaterialTheme.colorScheme.onSurfaceVariant
                                },
                                modifier = Modifier.weight(1f).padding(end = 12.dp).testTag("agent-status"),
                            )
                            Button(onClick = send, enabled = canSend, modifier = Modifier.testTag("agent-send")) {
                                Text("Send")
                            }
                        }
                    }
                }
            }
        }
    }
}
