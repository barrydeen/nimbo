package app.nimbo.backup.sync

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class SyncRulesTest {

    private val sha = "a".repeat(64)

    @Test
    fun `adoption requires a file with matching server sha and size`() {
        val match = RemoteCandidate("id", "file", "photo.jpg", 12, sha)
        assertTrue(ContentIdentity.isAdoptable(match, sha.uppercase(), 12))
        assertTrue(ContentIdentity.isAdoptable(match, sha, 12))
        assertFalse(ContentIdentity.isAdoptable(match.copy(kind = "folder"), sha, 12))
        assertFalse(ContentIdentity.isAdoptable(match.copy(sha256 = null), sha, 12))
        assertFalse(ContentIdentity.isAdoptable(match.copy(sha256 = "b".repeat(64)), sha, 12))
        assertFalse(ContentIdentity.isAdoptable(match.copy(size = 13), sha, 12))
        assertFalse(ContentIdentity.isAdoptable(null, sha, 12))
    }

    @Test
    fun `hash comparison requires both hashes`() {
        assertTrue(ContentIdentity.hashesEqual(sha, sha.uppercase()))
        assertFalse(ContentIdentity.hashesEqual(null, sha))
        assertFalse(ContentIdentity.hashesEqual("", ""))
    }

    @Test
    fun `http failures are classified as auth, transient or permanent`() {
        assertEquals(FailureKind.AUTH, RetryPolicy.classifyHttp(401))
        assertEquals(FailureKind.TRANSIENT, RetryPolicy.classifyHttp(408))
        assertEquals(FailureKind.TRANSIENT, RetryPolicy.classifyHttp(425))
        assertEquals(FailureKind.TRANSIENT, RetryPolicy.classifyHttp(429))
        assertEquals(FailureKind.TRANSIENT, RetryPolicy.classifyHttp(500))
        assertEquals(FailureKind.TRANSIENT, RetryPolicy.classifyHttp(503))
        assertEquals(FailureKind.PERMANENT, RetryPolicy.classifyHttp(400))
        assertEquals(FailureKind.PERMANENT, RetryPolicy.classifyHttp(403))
        assertEquals(FailureKind.PERMANENT, RetryPolicy.classifyHttp(404))
        assertEquals(FailureKind.PERMANENT, RetryPolicy.classifyHttp(409))
        assertEquals(FailureKind.PERMANENT, RetryPolicy.classifyHttp(413))
    }

    @Test
    fun `server upload failures distinguish poison files from transient errors`() {
        assertEquals(FailureKind.PERMANENT, RetryPolicy.classifyServerFailure("file is empty"))
        assertEquals(
            FailureKind.PERMANENT,
            RetryPolicy.classifyServerFailure("file exceeds the 200 MiB upload limit"),
        )
        assertEquals(FailureKind.PERMANENT, RetryPolicy.classifyServerFailure("invalid file name"))
        assertEquals(FailureKind.PERMANENT, RetryPolicy.classifyServerFailure("already exists"))
        assertEquals(FailureKind.TRANSIENT, RetryPolicy.classifyServerFailure("temporary failure"))
    }

    @Test
    fun `attempt budget is bounded`() {
        assertTrue(RetryPolicy.shouldRetry(0))
        assertTrue(RetryPolicy.shouldRetry(RetryPolicy.MAX_ATTEMPTS - 1))
        assertFalse(RetryPolicy.shouldRetry(RetryPolicy.MAX_ATTEMPTS))
        assertTrue(RetryPolicy.isPermanent(FailureKind.PERMANENT))
        assertFalse(RetryPolicy.isPermanent(FailureKind.TRANSIENT))
    }

    @Test
    fun `streaming size guard rejects growth at the scanned limit`() {
        assertFalse(StreamSizeGuard.wouldExceed(total = 0, read = 10, expected = 10))
        assertFalse(StreamSizeGuard.wouldExceed(total = 5, read = 5, expected = 10))
        assertTrue(StreamSizeGuard.wouldExceed(total = 5, read = 6, expected = 10))
        assertTrue(StreamSizeGuard.wouldExceed(total = 10, read = 1, expected = 10))
        assertTrue(StreamSizeGuard.wouldExceed(total = 11, read = 0, expected = 10))
        assertFalse(StreamSizeGuard.wouldExceed(total = 3, read = -1, expected = 10))
    }
}
