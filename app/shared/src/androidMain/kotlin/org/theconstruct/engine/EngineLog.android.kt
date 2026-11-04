package org.theconstruct.engine

import android.util.Log

internal actual fun engineLog(message: String) {
    Log.i("ConstructEngine", message)
}
