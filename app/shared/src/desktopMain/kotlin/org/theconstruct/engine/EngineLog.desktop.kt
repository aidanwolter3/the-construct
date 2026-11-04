package org.theconstruct.engine

// `[desktop]` rather than `[ConstructEngine]`: the Rust engine already prints `[ConstructEngine] ...` lines
// to the same stdout, so the prefix tells the Kotlin side apart.
internal actual fun engineLog(message: String) {
    println("[desktop] $message")
}
