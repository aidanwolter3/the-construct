package org.theconstruct

import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag

/** An `action.confirm` waiting for the user's answer, dispatched from layout [from]. */
data class PendingConfirm(val intent: ActionIntent.Confirm, val from: String)

/**
 * The dialog of `action.confirm` (feature `action_confirm`): drawn by the app, not a layout, so it always looks like
 * the platform's own prompt. Confirm runs the intent's `then`; Cancel, Back or a tap outside do nothing. The simulator
 * clicks the buttons as `tag:confirm:ok` and `tag:confirm:cancel`.
 */
@Composable
internal fun ConfirmDialog(controller: ConstructController) {
    val pending by controller.pendingConfirm.collectAsState()
    val c = pending?.intent ?: return
    AlertDialog(
        onDismissRequest = controller::cancelConfirm,
        title = { Text(c.title) },
        text = c.message?.takeIf { it.isNotBlank() }?.let { m -> { Text(m) } },
        confirmButton = { TextButton(onClick = controller::confirm, Modifier.testTag("confirm:ok")) { Text(c.confirmLabel) } },
        dismissButton = { TextButton(onClick = controller::cancelConfirm, Modifier.testTag("confirm:cancel")) { Text("Cancel") } },
    )
}
