package app.nimbo.backup.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class RemoteNameRulesTest {

    @Test
    fun `normalize trims and keeps valid names`() {
        assertEquals("Photo.jpg", RemoteNameRules.normalize("  Photo.jpg  "))
        assertEquals("a", RemoteNameRules.normalize("a"))
    }

    @Test
    fun `normalize rejects backend-invalid names`() {
        assertNull(RemoteNameRules.normalize(""))
        assertNull(RemoteNameRules.normalize("   "))
        assertNull(RemoteNameRules.normalize("."))
        assertNull(RemoteNameRules.normalize(".."))
        assertNull(RemoteNameRules.normalize("a/b"))
        assertNull(RemoteNameRules.normalize("a\\b"))
        assertNull(RemoteNameRules.normalize("a\u0000b"))
        assertNull(RemoteNameRules.normalize("a\nb"))
        assertNull(RemoteNameRules.normalize("x".repeat(256)))
        assertEquals(255, RemoteNameRules.normalize("x".repeat(255))?.length)
    }

    @Test
    fun `same mirrors sqlite nocase ascii folding only`() {
        assertTrue(RemoteNameRules.same("Photos", "photos"))
        assertTrue(RemoteNameRules.same(" Photos ", "PHOTOS"))
        assertFalse(RemoteNameRules.same("Photos", "Photos2"))
        // SQLite NOCASE does not fold non-ASCII characters.
        assertFalse(RemoteNameRules.same("Ä", "ä"))
    }

    @Test
    fun `normalizePath trims each segment and rejects separators`() {
        assertEquals("a/b/c", RemoteNameRules.normalizePath(" a / b /c "))
        assertEquals("", RemoteNameRules.normalizePath(""))
        assertEquals("a/b", RemoteNameRules.normalizePath("a//b/"))
        assertNull(RemoteNameRules.normalizePath("a/../b"))
        assertNull(RemoteNameRules.normalizePath("a/\u0007/b"))
    }
}
