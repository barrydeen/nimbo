package app.nimbo.backup.data.api

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class BrowsePayloadTest {

    private val json = Json { ignoreUnknownKeys = true; explicitNulls = false }

    @Test
    fun folderListingCarriesBreadcrumbsAndThumbnailFlag() {
        val payload = """
            {
              "folder": {"id": "abc", "name": "Photos", "parent_id": "root"},
              "breadcrumbs": [
                {"id": "root", "name": "My Drive"},
                {"id": "abc", "name": "Photos"}
              ],
              "items": [
                {"id": "f1", "kind": "file", "name": "cat.jpg", "mime": "image/jpeg",
                 "size": 1234, "updated_at": 1700000000, "has_thumb": true,
                 "sha256": "abc", "path": ""},
                {"id": "d1", "kind": "folder", "name": "Nested", "size": 0}
              ],
              "next_offset": 500
            }
        """.trimIndent()
        val listing = json.decodeFromString(FolderListing.serializer(), payload)
        assertEquals("abc", listing.folder.id)
        assertEquals("root", listing.folder.parent_id)
        assertEquals(2, listing.breadcrumbs.size)
        assertEquals(500, listing.next_offset)
        assertTrue(listing.items[0].has_thumb)
        assertEquals("image/jpeg", listing.items[0].mime)
        assertFalse(listing.items[1].has_thumb)
        assertEquals("folder", listing.items[1].kind)
    }

    @Test
    fun legacyFolderListingStillParses() {
        val listing = json.decodeFromString(
            FolderListing.serializer(),
            """{"folder":{"id":"root","name":"My Drive"},"items":[]}""",
        )
        assertEquals("", listing.folder.parent_id)
        assertTrue(listing.breadcrumbs.isEmpty())
        assertNull(listing.next_offset)
    }

    @Test
    fun mediaAndSearchPayloadsParse() {
        val media = json.decodeFromString(
            MediaListing.serializer(),
            """
            {"items":[
              {"id":"v1","kind":"file","name":"clip.mp4","mime":"video/mp4",
               "size":99,"updated_at":1700000001,"has_thumb":true,"path":"Movies/clip.mp4"}
            ],"next_offset":120}
            """.trimIndent(),
        )
        assertEquals(1, media.items.size)
        assertTrue(media.items[0].has_thumb)
        assertEquals("Movies/clip.mp4", media.items[0].path)
        assertEquals(120, media.next_offset)

        val search = json.decodeFromString(
            FileSearchResponse.serializer(),
            """
            {"results":[
              {"id":"d1","kind":"folder","name":"Notes","path":"Notes"},
              {"id":"f1","kind":"file","name":"cat.jpg","path":"Photos/cat.jpg",
               "has_thumb":true,"snippet":"a <mark>cat</mark>"}
            ]}
            """.trimIndent(),
        )
        assertEquals(2, search.results.size)
        assertEquals("Notes", search.results[0].path)
        assertTrue(search.results[1].snippet.contains("<mark>"))
    }

    @Test
    fun vaultPayloadsParse() {
        val tree = json.decodeFromString(
            DocsTreeResponse.serializer(),
            """{"documents":[{"path":"notes/one.md","title":"One","updated_at":1700000002}]}""",
        )
        assertEquals("notes/one.md", tree.documents[0].path)
        assertEquals("One", tree.documents[0].title)

        val content = json.decodeFromString(
            DocContent.serializer(),
            """{"path":"notes/one.md","title":"One","content":"# One","hash":"h"}""",
        )
        assertEquals("# One", content.content)
        assertEquals("h", content.hash)
    }
}
