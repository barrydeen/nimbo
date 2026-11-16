package app.nimbo.backup.data

import android.net.Uri
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

/**
 * The pairing payload shown as a QR code by the web UI:
 * `nimbo://connect?v=1&url=<server url>&token=<device token>`
 *
 * Parsed only by the explicit in-app QR scanner; the app intentionally does not
 * export a BROWSABLE intent-filter for this scheme.
 *
 * Also accepts the same fields as JSON for forward compatibility.
 */
object QrPayload {

    const val SCHEME = "nimbo"
    const val HOST = "connect"
    const val VERSION = "v1"

    fun encode(serverUrl: String, token: String): String =
        "$SCHEME://$HOST?$VERSION&url=${Uri.encode(serverUrl)}&token=${Uri.encode(token)}"

    /** Returns (serverUrl, token) when [text] is a valid pairing payload. */
    fun parse(text: String): Pair<String, String>? {
        val trimmed = text.trim()
        if (trimmed.isEmpty()) return null
        val uri = runCatching { Uri.parse(trimmed) }.getOrNull()
        if (uri?.scheme == SCHEME && uri.host == HOST) {
            return validate(uri.getQueryParameter("url"), uri.getQueryParameter("token"))
        }
        return try {
            val obj = Json.parseToJsonElement(trimmed).jsonObject
            validate(
                obj["url"]?.jsonPrimitive?.content,
                obj["token"]?.jsonPrimitive?.content,
            )
        } catch (_: Exception) {
            null
        }
    }

    private fun validate(url: String?, token: String?): Pair<String, String>? {
        val serverUrl = url?.trim()?.trimEnd('/').orEmpty()
        if (!serverUrl.startsWith("http://") && !serverUrl.startsWith("https://")) return null
        val deviceToken = token?.trim().orEmpty()
        if (!deviceToken.startsWith("nmb_")) return null
        return serverUrl to deviceToken
    }
}
