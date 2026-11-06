package org.theconstruct

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * The OS permission to show notifications (Android 13+'s POST_NOTIFICATIONS), for the device data
 * `local://device#notification_permission` and the `action.device` command `request_notification_permission`.
 * Android supplies one to [ConstructApp]; desktop and the simulator pass none ([NoNotificationPermission]).
 */
interface NotificationPermissionPlatform {
    /** `granted`, `denied`, or `""` where there is no such permission. */
    val state: StateFlow<String>

    /** Shows the system prompt, or the app's notification settings when the prompt can't be shown again. */
    fun request()
}

/** No permission to ask for (desktop, simulator): the state is `""` and [request] does nothing. */
object NoNotificationPermission : NotificationPermissionPlatform {
    override val state: StateFlow<String> = MutableStateFlow("").asStateFlow()
    override fun request() {}
}
