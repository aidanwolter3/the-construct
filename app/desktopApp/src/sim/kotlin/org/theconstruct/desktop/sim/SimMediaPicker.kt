package org.theconstruct.desktop.sim

import org.theconstruct.MediaPickerPlatform

/**
 * The simulator's photo picker: the script's `pick <path> [<path>…]` queues files ([enqueue]) that the next picker
 * invocation (a click on a `core://MediaPicker`'s add tile) returns, at most its `max`; an empty queue is a cancelled
 * pick. Every invocation is recorded in [requests] (its `max`).
 */
class SimMediaPicker(private val log: (String) -> Unit = ::println) : MediaPickerPlatform {
    private val lock = Any()
    private val queued = mutableListOf<String>()

    /** The `max` of every pick so far, in order. */
    val requests: MutableList<Int> = java.util.Collections.synchronizedList(mutableListOf())

    /** Files the next pick returns (replacing any still queued). */
    fun enqueue(paths: List<String>) {
        synchronized(lock) {
            queued.clear()
            queued += paths
        }
    }

    /** Paths still queued. */
    val pending: List<String> get() = synchronized(lock) { queued.toList() }

    override suspend fun pick(max: Int): List<String> {
        requests += max
        val out = synchronized(lock) {
            queued.take(max.coerceAtLeast(0)).also { queued.clear() }
        }
        log(if (out.isEmpty()) "[sim] picker: nothing queued (cancelled)" else "[sim] picker (max $max): ${out.joinToString()}")
        return out
    }
}
