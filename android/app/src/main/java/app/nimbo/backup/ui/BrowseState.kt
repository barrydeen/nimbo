package app.nimbo.backup.ui

import app.nimbo.backup.data.api.DocSummary
import app.nimbo.backup.data.api.NodeSummary

enum class MainTab(val label: String) {
    Backups("Backups"),
    Files("Files"),
    Gallery("Gallery"),
    Search("Search"),
    Vault("Vault"),
}

enum class MediaFilter(val apiValue: String?, val label: String) {
    All(null, "All"),
    Photos("image", "Photos"),
    Videos("video", "Videos"),
}

data class CrumbUi(val id: String, val name: String)

data class FilesState(
    val folderId: String = "",
    val folderName: String = "My Drive",
    val breadcrumbs: List<CrumbUi> = emptyList(),
    val items: List<NodeSummary> = emptyList(),
    val loading: Boolean = false,
    val error: String? = null,
)

data class GalleryState(
    val filter: MediaFilter = MediaFilter.All,
    val items: List<NodeSummary> = emptyList(),
    val loading: Boolean = false,
    val error: String? = null,
    val nextOffset: Int? = null,
)

data class SearchState(
    val query: String = "",
    val results: List<NodeSummary> = emptyList(),
    val loading: Boolean = false,
    val error: String? = null,
    val searched: Boolean = false,
)

data class VaultState(
    val documents: List<DocSummary> = emptyList(),
    val loading: Boolean = false,
    val error: String? = null,
    val loaded: Boolean = false,
)

data class DocViewerState(
    val path: String,
    val title: String = "",
    val content: String? = null,
    val loading: Boolean = false,
    val error: String? = null,
)

data class DownloadTarget(
    val id: String,
    val name: String,
    val mime: String?,
)

data class BrowseState(
    val files: FilesState = FilesState(),
    val gallery: GalleryState = GalleryState(),
    val search: SearchState = SearchState(),
    val vault: VaultState = VaultState(),
    val docViewer: DocViewerState? = null,
    val imageViewer: NodeSummary? = null,
    val downloading: Set<String> = emptySet(),
)

fun NodeSummary.isFolder(): Boolean = kind == "folder"

fun NodeSummary.isImage(): Boolean = mime?.startsWith("image/") == true

fun NodeSummary.isVideo(): Boolean = mime?.startsWith("video/") == true

fun NodeSummary.isMedia(): Boolean = isImage() || isVideo()
