package org.genui.canvas.engine

// `[desktop]` rather than `[CanvasEngine]`: the Rust engine already prints `[CanvasEngine] ...` lines
// to the same stdout, so the prefix tells the Kotlin side apart.
internal actual fun engineLog(message: String) {
    println("[desktop] $message")
}
