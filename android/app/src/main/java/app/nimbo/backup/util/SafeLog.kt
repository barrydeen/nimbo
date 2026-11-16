package app.nimbo.backup.util

import android.util.Log
import app.nimbo.backup.BuildConfig

/**
 * Release logging intentionally contains no folder names, SAF URIs, server
 * response bodies or exception messages. Verbose detail is debug-only.
 */
object SafeLog {

    fun d(tag: String, message: String) {
        if (BuildConfig.DEBUG) Log.d(tag, message)
    }

    fun w(tag: String, message: String) {
        if (BuildConfig.DEBUG) Log.w(tag, message) else Log.w(tag, "backup operation failed")
    }

    fun w(tag: String, message: String, throwable: Throwable) {
        if (BuildConfig.DEBUG) Log.w(tag, message, throwable) else Log.w(tag, "backup operation failed")
    }

    fun e(tag: String, message: String, throwable: Throwable) {
        if (BuildConfig.DEBUG) Log.e(tag, message, throwable) else Log.e(tag, "backup operation failed")
    }
}
