package app.nimbo.backup.data

/**
 * Name rules shared with the Nimbo backend (`files::validate_name` and the
 * case-insensitive sibling uniqueness index).
 *
 * The backend trims names, rejects empty/"."/"..", path separators, control
 * characters and names longer than 255 UTF-8 bytes, and compares sibling names
 * case-insensitively. The Android client must apply the same rules before it
 * constructs a remote path or decides that a remote folder already exists.
 */
object RemoteNameRules {

    const val MAX_NAME_BYTES = 255

    /** Returns the canonical remote name, or null when the name cannot be mirrored. */
    fun normalize(raw: String): String? {
        val name = raw.trim()
        if (name.isEmpty() || name == "." || name == "..") return null
        if (name.toByteArray(Charsets.UTF_8).size > MAX_NAME_BYTES) return null
        if (name.any { it == '\u0000' || it.isISOControl() }) return null
        if (name.contains('/') || name.contains('\\')) return null
        return name
    }

    /** True when two names address the same backend sibling. */
    fun same(a: String, b: String): Boolean {
        val left = normalize(a) ?: return false
        val right = normalize(b) ?: return false
        return asciiLower(left) == asciiLower(right)
    }

    /**
     * SQLite's NOCASE collation (used by the backend sibling index) folds ASCII
     * A–Z only. Mirroring that here avoids adopting a differently-cased folder
     * that the server would treat as a separate sibling.
     */
    private fun asciiLower(value: String): String = buildString(value.length) {
        for (ch in value) {
            append(if (ch in 'A'..'Z') ch + ('a' - 'A') else ch)
        }
    }

    /**
     * Normalizes a relative path whose segments are separated by `/`.
     * Returns null when any segment is not mirrorable.
     */
    fun normalizePath(path: String): String? {
        if (path.isBlank()) return ""
        val segments = path.split('/').filter { it.isNotEmpty() }
        val normalized = segments.map { normalize(it) ?: return null }
        return normalized.joinToString("/")
    }
}
