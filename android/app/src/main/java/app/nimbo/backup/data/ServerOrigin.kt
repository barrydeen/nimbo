package app.nimbo.backup.data

import java.net.URI

/** Thrown when a plain-HTTP URL is supplied without an explicit opt-in. */
class InsecureTransportException(message: String) : IllegalArgumentException(message)

/**
 * Result of validating and normalizing a user-supplied server URL.
 *
 * [origin] deliberately omits default ports and the base path, so it can be
 * used as the stable cache identity for server-side node/folder ids.
 */
data class NormalizedServer(
    val rootUrl: String,
    val origin: String,
    val secure: Boolean,
)

object ServerOrigin {

    fun parse(raw: String, allowInsecure: Boolean): NormalizedServer {
        val trimmed = raw.trim()
        require(trimmed.isNotEmpty()) { "enter a server URL" }

        val uri = try {
            URI(trimmed)
        } catch (e: Exception) {
            throw IllegalArgumentException("that is not a valid server URL", e)
        }

        val scheme = uri.scheme?.lowercase()
            ?: throw IllegalArgumentException("server URL must include https://")
        if (scheme != "https" && scheme != "http") {
            throw IllegalArgumentException("server URL must start with https://")
        }
        if (uri.userInfo != null) {
            throw IllegalArgumentException("server URL must not contain a username or password")
        }
        if (uri.host.isNullOrBlank()) {
            throw IllegalArgumentException("server URL must include a host name")
        }
        if (uri.query != null || uri.fragment != null) {
            throw IllegalArgumentException("server URL must not contain a query or fragment")
        }
        if (scheme == "http" && !allowInsecure) {
            throw InsecureTransportException(
                "HTTP is disabled because it exposes your device token and backup contents. " +
                    "Use HTTPS, or explicitly enable insecure HTTP below.",
            )
        }

        val defaultPort = if (scheme == "https") 443 else 80
        val port = if (uri.port == -1) defaultPort else uri.port
        val host = uri.host.lowercase()
        val authorityHost = if (host.contains(':') && !host.startsWith("[")) "[$host]" else host
        val authority = if (port == defaultPort) authorityHost else "$authorityHost:$port"
        val path = uri.rawPath.orEmpty().trimEnd('/')
        val root = "$scheme://$authority$path"
        return NormalizedServer(
            rootUrl = root,
            origin = "$scheme://$authority",
            secure = scheme == "https",
        )
    }

    /** Best-effort normalization used when comparing cached state. */
    fun originOf(raw: String, allowInsecure: Boolean): String? = try {
        parse(raw, allowInsecure).origin
    } catch (_: IllegalArgumentException) {
        null
    }
}
