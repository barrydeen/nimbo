package app.nimbo.backup.sync

/** How the engine should react to a failed item or request. */
enum class FailureKind { AUTH, PERMANENT, TRANSIENT }

/** Minimal remote item needed to prove content identity. */
data class RemoteCandidate(
    val id: String,
    val kind: String,
    val name: String,
    val size: Long,
    val sha256: String?,
)

/**
 * Content identity rules. A remote file is only adopted when its server-side
 * SHA-256 matches the locally computed SHA-256; name/size are never proof.
 */
object ContentIdentity {

    fun hashesEqual(a: String?, b: String?): Boolean {
        val left = a?.trim()?.lowercase() ?: return false
        val right = b?.trim()?.lowercase() ?: return false
        return left.isNotEmpty() && left == right
    }

    fun isAdoptable(candidate: RemoteCandidate?, localSha: String, localSize: Long): Boolean {
        if (candidate == null) return false
        if (candidate.kind != "file") return false
        if (candidate.size != localSize) return false
        return hashesEqual(candidate.sha256, localSha)
    }
}

/**
 * Retry classification shared by uploads, folder creation and moves.
 *
 * 401 means the credential is no longer usable: stop scheduling until re-pair.
 * 4xx policy errors (bad name, too large, empty, conflict, forbidden) are
 * permanent for the current input. Network errors, 408/425/429 and 5xx are
 * retried with WorkManager's exponential backoff until the per-file attempt
 * budget is exhausted.
 */
object RetryPolicy {

    const val MAX_ATTEMPTS = 5

    fun classifyHttp(code: Int): FailureKind = when {
        code == 401 -> FailureKind.AUTH
        code == 408 || code == 425 || code == 429 || code in 500..599 -> FailureKind.TRANSIENT
        code in 400..499 -> FailureKind.PERMANENT
        else -> FailureKind.TRANSIENT
    }

    /** Server upload failures are returned in a 200 body as `{failed:[{error}]}`. */
    fun classifyServerFailure(message: String): FailureKind {
        val text = message.lowercase()
        return when {
            "empty" in text -> FailureKind.PERMANENT
            "exceeds" in text || "limit" in text || "too large" in text -> FailureKind.PERMANENT
            "invalid" in text || "must not" in text || "too long" in text -> FailureKind.PERMANENT
            "name" in text && "required" in text -> FailureKind.PERMANENT
            "already exists" in text || "conflict" in text -> FailureKind.PERMANENT
            else -> FailureKind.TRANSIENT
        }
    }

    fun shouldRetry(attempts: Int): Boolean = attempts < MAX_ATTEMPTS

    fun isPermanent(kind: FailureKind): Boolean = kind == FailureKind.PERMANENT
}

/** Streaming guard against a provider growing a file after the scan. */
object StreamSizeGuard {

    /**
     * True when writing [read] more bytes would exceed the scanned [expected]
     * size. A negative [read] is EOF and never exceeds.
     */
    fun wouldExceed(total: Long, read: Int, expected: Long): Boolean {
        if (read < 0) return false
        if (total < 0 || expected < 0) return true
        if (total > expected) return true
        return read > expected - total
    }
}
