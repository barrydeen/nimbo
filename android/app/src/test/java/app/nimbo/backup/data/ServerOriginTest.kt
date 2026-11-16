package app.nimbo.backup.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ServerOriginTest {

    @Test
    fun `https default port and trailing slash are normalized`() {
        val server = ServerOrigin.parse("HTTPS://Nimbo.Example.com:443/", allowInsecure = false)
        assertEquals("https://nimbo.example.com", server.rootUrl)
        assertEquals("https://nimbo.example.com", server.origin)
        assertTrue(server.secure)
    }

    @Test
    fun `custom port and base path are retained`() {
        val server = ServerOrigin.parse("https://nimbo.example.com:8443/nimbo/", false)
        assertEquals("https://nimbo.example.com:8443/nimbo", server.rootUrl)
        assertEquals("https://nimbo.example.com:8443", server.origin)
    }

    @Test
    fun `http is rejected unless explicitly allowed`() {
        try {
            ServerOrigin.parse("http://192.0.2.10:8080", allowInsecure = false)
            throw AssertionError("expected rejection")
        } catch (_: InsecureTransportException) {
            // expected
        }
        val allowed = ServerOrigin.parse("http://192.0.2.10:8080/", true)
        assertEquals("http://192.0.2.10:8080", allowed.rootUrl)
        assertFalse(allowed.secure)
    }

    @Test
    fun `embedded credentials and query strings are rejected`() {
        for (bad in listOf(
            "https://user:pass@example.com",
            "https://example.com/?token=secret",
            "https://example.com/#fragment",
            "ftp://example.com",
            "not a url",
        )) {
            try {
                ServerOrigin.parse(bad, allowInsecure = true)
                throw AssertionError("expected rejection for $bad")
            } catch (_: IllegalArgumentException) {
                // expected
            }
        }
    }

    @Test
    fun `originOf returns null for invalid input`() {
        assertNull(ServerOrigin.originOf("http://example.com", allowInsecure = false))
        assertEquals(
            "http://example.com",
            ServerOrigin.originOf("http://example.com:80/", allowInsecure = true),
        )
    }
}
