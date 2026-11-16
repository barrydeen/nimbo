package app.nimbo.backup.data.api

import app.nimbo.backup.data.ServerOrigin
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import okhttp3.Call
import okhttp3.Callback
import okhttp3.Headers
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.MultipartBody
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody
import okhttp3.RequestBody.Companion.asRequestBody
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.IOException
import java.io.OutputStream
import java.time.ZonedDateTime
import java.time.format.DateTimeFormatter
import java.util.concurrent.TimeUnit

/** Server rejected our credentials (HTTP 401). */
class NimboAuthException(message: String) : IOException(message)

/** Server answered with a non-2xx status. */
class NimboApiException(
    val code: Int,
    message: String,
    val retryAfterMs: Long? = null,
) : IOException("HTTP $code: $message")

@Serializable
data class NodeSummary(
    val id: String,
    val kind: String,
    val name: String,
    val mime: String? = null,
    val size: Long = 0,
    /** Trusted server-side content hash; required before adoption. */
    val sha256: String? = null,
    val parent_id: String = "",
    val updated_at: Long = 0,
    val has_thumb: Boolean = false,
    /** Full path for flat search/gallery results (empty in folder listings). */
    val path: String = "",
    /** Pre-escaped FTS snippet with `<mark>` highlights, when searching. */
    val snippet: String = "",
)

@Serializable
data class Breadcrumb(val id: String, val name: String)

@Serializable
data class FolderListing(
    val folder: FolderRef,
    val breadcrumbs: List<Breadcrumb> = emptyList(),
    val items: List<NodeSummary> = emptyList(),
    val next_offset: Int? = null,
) {
    @Serializable
    data class FolderRef(val id: String, val name: String, val parent_id: String = "")
}

@Serializable
data class MediaListing(
    val items: List<NodeSummary> = emptyList(),
    val next_offset: Int? = null,
)

@Serializable
data class FileSearchResponse(val results: List<NodeSummary> = emptyList())

@Serializable
data class DocSummary(
    val path: String,
    val title: String = "",
    val updated_at: Long = 0,
)

@Serializable
data class DocsTreeResponse(val documents: List<DocSummary> = emptyList())

@Serializable
data class DocContent(
    val path: String,
    val content: String,
    val hash: String = "",
    val title: String = "",
)

@Serializable
data class InstanceInfo(
    val setup_complete: Boolean = false,
    val instance_name: String = "",
    val max_upload_bytes: Long? = null,
    val backup_scope: Boolean? = null,
)

/**
 * Thin client for the Nimbo files API, authenticated with a device token via
 * `Authorization: Bearer`.
 *
 * Every network call is suspending and ties `Call.cancel()` to coroutine
 * cancellation. HTTPS certificate and hostname validation is left untouched;
 * plain HTTP must be explicitly enabled by the user.
 */
class NimboClient(
    baseUrl: String,
    private val token: String,
    allowInsecureTransport: Boolean = false,
) {

    private val server = ServerOrigin.parse(baseUrl, allowInsecureTransport)
    private val root = server.rootUrl
    private val json = Json { ignoreUnknownKeys = true; explicitNulls = false }
    private val jsonMediaType = "application/json; charset=utf-8".toMediaType()

    private val http: OkHttpClient = OkHttpClient.Builder()
        .addInterceptor { chain ->
            chain.proceed(
                chain.request().newBuilder()
                    .header("Authorization", "Bearer $token")
                    .build(),
            )
        }
        .connectTimeout(15, TimeUnit.SECONDS)
        .readTimeout(60, TimeUnit.SECONDS)
        .writeTimeout(300, TimeUnit.SECONDS)
        .build()

    /** Cheap connectivity + credential check; also returns upload capabilities. */
    suspend fun testConnection(): InstanceInfo = instance()

    suspend fun instance(): InstanceInfo {
        val resp = execute(buildRequest("/api/instance")).requireSuccess()
        return json.decodeFromString(InstanceInfo.serializer(), resp.body)
    }

    /** Authenticated account id, used to invalidate cached remote ids across re-pairs. */
    suspend fun accountId(): String? {
        val resp = execute(buildRequest("/api/auth/me")).requireSuccess()
        return try {
            json.decodeFromString(MeResponse.serializer(), resp.body).user.id.takeIf { it.isNotBlank() }
        } catch (_: Exception) {
            null
        }
    }

    /**
     * GET /api/files?folder_id= — lists every child of a folder through bounded
     * pages. The old server returns no `next_offset` and remains compatible.
     */
    suspend fun listFolder(folderId: String): FolderListing {
        val folderQuery = if (folderId.isBlank()) "" else "&folder_id=${urlEncode(folderId)}"
        val first = fetchFolderPage(folderQuery, offset = 0)
        val items = first.items.toMutableList()
        var next = first.next_offset
        var pages = 1
        while (next != null) {
            if (pages++ >= MAX_LIST_PAGES) {
                throw NimboApiException(500, "folder listing exceeded its safety bound")
            }
            val page = fetchFolderPage(folderQuery, next.toLong())
            items += page.items
            next = page.next_offset
        }
        return first.copy(items = items, next_offset = null)
    }

    private suspend fun fetchFolderPage(folderQuery: String, offset: Long): FolderListing {
        val q = "?offset=$offset&limit=$LIST_PAGE_SIZE$folderQuery"
        val resp = execute(buildRequest("/api/files$q")).requireSuccess()
        return json.decodeFromString(FolderListing.serializer(), resp.body)
    }

    /**
     * GET /api/files/media — flat newest-first image/video listing across the
     * accessible subtree, used by the gallery.
     */
    suspend fun media(kind: String?, offset: Int, limit: Int): MediaListing {
        val kindQuery = if (kind.isNullOrBlank()) "" else "&kind=${urlEncode(kind)}"
        val resp = execute(
            buildRequest("/api/files/media?offset=$offset&limit=$limit$kindQuery"),
        ).requireSuccess()
        return json.decodeFromString(MediaListing.serializer(), resp.body)
    }

    /** GET /api/files/search — names, extracted text, metadata and descriptions. */
    suspend fun searchFiles(query: String): List<NodeSummary> {
        val resp = execute(
            buildRequest("/api/files/search?q=${urlEncode(query)}"),
        ).requireSuccess()
        return json.decodeFromString(FileSearchResponse.serializer(), resp.body).results
    }

    /** GET /api/docs/tree — read-only vault index. Requires a full-scope token. */
    suspend fun docsTree(): List<DocSummary> {
        val resp = execute(buildRequest("/api/docs/tree")).requireSuccess()
        return json.decodeFromString(DocsTreeResponse.serializer(), resp.body).documents
    }

    /** GET /api/docs/content?path= — read-only document content. */
    suspend fun docContent(path: String): DocContent {
        val resp = execute(
            buildRequest("/api/docs/content?path=${urlEncode(path)}"),
        ).requireSuccess()
        return json.decodeFromString(DocContent.serializer(), resp.body)
    }

    /** GET /api/files/{id}/thumbnail — generated JPEG thumbnail. */
    suspend fun thumbnail(id: String, maxBytes: Long = 4L * 1024 * 1024): ByteArray =
        executeBytes(buildRequest("/api/files/${urlEncode(id)}/thumbnail"), maxBytes)

    /** GET /api/files/{id}/content — inline bytes for an in-app image viewer. */
    suspend fun contentBytes(id: String, maxBytes: Long = 40L * 1024 * 1024): ByteArray =
        executeBytes(buildRequest("/api/files/${urlEncode(id)}/content"), maxBytes)

    /**
     * GET /api/files/{id}/download — streams attachment bytes into [out] without
     * materialising the whole file in memory.
     */
    suspend fun downloadTo(id: String, out: OutputStream): Long = suspendCancellableCoroutine { cont ->
        val call = http.newCall(buildRequest("/api/files/${urlEncode(id)}/download"))
        cont.invokeOnCancellation { call.cancel() }
        call.enqueue(object : Callback {
            override fun onFailure(call: Call, e: IOException) {
                if (cont.isActive) cont.resumeWith(Result.failure(e))
            }

            override fun onResponse(call: Call, response: Response) {
                response.use { resp ->
                    if (resp.code == 401) {
                        if (cont.isActive) {
                            cont.resumeWith(Result.failure(NimboAuthException("token rejected by server")))
                        }
                        return
                    }
                    if (resp.code !in 200..299) {
                        val message = try {
                            resp.body?.string().orEmpty().errorMessage()
                        } catch (_: Exception) {
                            "download failed"
                        }
                        if (cont.isActive) {
                            cont.resumeWith(
                                Result.failure(
                                    NimboApiException(resp.code, message, retryAfterMs(resp.headers)),
                                ),
                            )
                        }
                        return
                    }
                    val body = resp.body ?: run {
                        if (cont.isActive) {
                            cont.resumeWith(Result.failure(NimboApiException(500, "empty download")))
                        }
                        return
                    }
                    try {
                        body.byteStream().use { input ->
                            val buffer = ByteArray(64 * 1024)
                            var total = 0L
                            while (true) {
                                if (!cont.isActive) {
                                    call.cancel()
                                    return
                                }
                                val read = input.read(buffer)
                                if (read < 0) break
                                out.write(buffer, 0, read)
                                total += read
                            }
                            if (cont.isActive) cont.resumeWith(Result.success(total))
                        }
                    } catch (e: Exception) {
                        if (cont.isActive) cont.resumeWith(Result.failure(e))
                    }
                }
            }
        })
    }

    /** POST /api/files/folders — create a folder; fails 409 when the name is taken. */
    suspend fun createFolder(parentId: String, name: String): NodeSummary {
        val payload = json
            .encodeToString(
                CreateFolderRequest.serializer(),
                CreateFolderRequest(parentId.ifBlank { null }, name),
            )
            .toRequestBody(jsonMediaType)
        val resp = execute(buildRequest("/api/files/folders", "POST", payload)).requireSuccess()
        return json.decodeFromString(CreateFolderResponse.serializer(), resp.body).item
    }

    /** PATCH /api/files/{id} — reconcile a rename or move for a stable local URI. */
    suspend fun updateNode(id: String, name: String, parentId: String): NodeSummary {
        val payload = json
            .encodeToString(
                UpdateNodeRequest.serializer(),
                UpdateNodeRequest(name = name, parent_id = parentId),
            )
            .toRequestBody(jsonMediaType)
        val resp = execute(buildRequest("/api/files/${urlEncode(id)}", "PATCH", payload))
            .requireSuccess()
        return json.decodeFromString(CreateFolderResponse.serializer(), resp.body).item
    }

    /** DELETE /api/files/{id}; only explicit 404s are safe to ignore by callers. */
    suspend fun deleteNode(id: String) {
        execute(buildRequest("/api/files/${urlEncode(id)}", "DELETE")).requireSuccess()
    }

    /** POST /api/files/upload?folder_id= — multipart upload, field name `file`. */
    suspend fun upload(folderId: String, name: String, tempFile: File): NodeSummary {
        val q = if (folderId.isBlank()) "" else "?folder_id=${urlEncode(folderId)}"
        val part = multipart(name, tempFile)
        val resp = execute(buildRequest("/api/files/upload$q", "POST", part)).requireSuccess()
        val parsed = json.decodeFromString(UploadResponse.serializer(), resp.body)
        val failed = parsed.failed.firstOrNull()
        if (failed != null) throw NimboApiException(400, failed.error)
        return parsed.uploaded.firstOrNull()
            ?: throw NimboApiException(500, "upload returned no file entry")
    }

    /**
     * PUT /api/files/{id}/content — atomically replace an existing node's bytes.
     * The server keeps the node id/name and retires the old blob only after the
     * new reference commits, so the previous good copy survives client failure.
     */
    suspend fun replaceContent(id: String, name: String, tempFile: File): NodeSummary {
        val part = multipart(name, tempFile)
        val resp = execute(
            buildRequest("/api/files/${urlEncode(id)}/content", "PUT", part),
        ).requireSuccess()
        return json.decodeFromString(CreateFolderResponse.serializer(), resp.body).item
    }

    private fun multipart(name: String, tempFile: File): MultipartBody =
        MultipartBody.Builder()
            .setType(MultipartBody.FORM)
            .addFormDataPart(
                "file",
                name,
                tempFile.asRequestBody("application/octet-stream".toMediaType()),
            )
            .build()

    private fun buildRequest(path: String, method: String = "GET", body: RequestBody? = null): Request {
        val builder = Request.Builder().url(root + path)
        when (method) {
            "DELETE" -> builder.delete(body)
            "PATCH" -> builder.patch(body ?: ByteArray(0).toRequestBody(null))
            "PUT" -> builder.put(body ?: ByteArray(0).toRequestBody(null))
            "POST" -> builder.post(body ?: ByteArray(0).toRequestBody(null))
            else -> builder.get()
        }
        return builder.build()
    }

    private suspend fun execute(request: Request): HttpResponse = suspendCancellableCoroutine { cont ->
        val call = http.newCall(request)
        cont.invokeOnCancellation { call.cancel() }
        call.enqueue(object : Callback {
            override fun onFailure(call: Call, e: IOException) {
                if (cont.isActive) cont.resumeWith(Result.failure(e))
            }

            override fun onResponse(call: Call, response: Response) {
                try {
                    response.use { resp ->
                        val body = resp.body?.string().orEmpty()
                        if (cont.isActive) {
                            cont.resumeWith(
                                Result.success(
                                    HttpResponse(
                                        code = resp.code,
                                        message = resp.message,
                                        headers = resp.headers,
                                        body = body,
                                    ),
                                ),
                            )
                        }
                    }
                } catch (e: Throwable) {
                    if (cont.isActive) cont.resumeWith(Result.failure(e))
                }
            }
        })
    }

    /** Bounded binary GET used for thumbnails and inline image previews. */
    private suspend fun executeBytes(request: Request, maxBytes: Long): ByteArray =
        suspendCancellableCoroutine { cont ->
            val call = http.newCall(request)
            cont.invokeOnCancellation { call.cancel() }
            call.enqueue(object : Callback {
                override fun onFailure(call: Call, e: IOException) {
                    if (cont.isActive) cont.resumeWith(Result.failure(e))
                }

                override fun onResponse(call: Call, response: Response) {
                    response.use { resp ->
                        if (resp.code == 401) {
                            if (cont.isActive) {
                                cont.resumeWith(
                                    Result.failure(NimboAuthException("token rejected by server")),
                                )
                            }
                            return
                        }
                        if (resp.code !in 200..299) {
                            val message = try {
                                resp.body?.string().orEmpty().errorMessage()
                            } catch (_: Exception) {
                                "HTTP error"
                            }
                            if (cont.isActive) {
                                cont.resumeWith(
                                    Result.failure(
                                        NimboApiException(
                                            resp.code,
                                            message,
                                            retryAfterMs(resp.headers),
                                        ),
                                    ),
                                )
                            }
                            return
                        }
                        val body = resp.body ?: run {
                            if (cont.isActive) {
                                cont.resumeWith(
                                    Result.failure(NimboApiException(500, "empty response")),
                                )
                            }
                            return
                        }
                        val declared = body.contentLength()
                        if (declared > maxBytes) {
                            if (cont.isActive) {
                                cont.resumeWith(
                                    Result.failure(
                                        NimboApiException(413, "response exceeds the preview limit"),
                                    ),
                                )
                            }
                            return
                        }
                        try {
                            body.byteStream().use { input ->
                                val initial = if (declared in 1..maxBytes) declared.toInt() else 16 * 1024
                                val output = ByteArrayOutputStream(initial)
                                val buffer = ByteArray(64 * 1024)
                                var total = 0L
                                while (true) {
                                    if (!cont.isActive) {
                                        call.cancel()
                                        return
                                    }
                                    val read = input.read(buffer)
                                    if (read < 0) break
                                    total += read
                                    if (total > maxBytes) {
                                        if (cont.isActive) {
                                            cont.resumeWith(
                                                Result.failure(
                                                    NimboApiException(
                                                        413,
                                                        "response exceeds the preview limit",
                                                    ),
                                                ),
                                            )
                                        }
                                        return
                                    }
                                    output.write(buffer, 0, read)
                                }
                                if (cont.isActive) cont.resumeWith(Result.success(output.toByteArray()))
                            }
                        } catch (e: Exception) {
                            if (cont.isActive) cont.resumeWith(Result.failure(e))
                        }
                    }
                }
            })
        }

    private fun HttpResponse.requireSuccess(): HttpResponse {
        if (code == 401) throw NimboAuthException("token rejected by server")
        if (code !in 200..299) {
            throw NimboApiException(code, body.errorMessage(), retryAfterMs(headers))
        }
        return this
    }

    private fun String.errorMessage(): String = try {
        json.decodeFromString(ErrorResponse.serializer(), this).error.ifBlank { "HTTP error" }
    } catch (_: Exception) {
        take(200)
    }

    private fun urlEncode(value: String): String =
        java.net.URLEncoder.encode(value, Charsets.UTF_8.name())

    private data class HttpResponse(
        val code: Int,
        val message: String,
        val headers: Headers,
        val body: String,
    )

    private fun retryAfterMs(headers: Headers): Long? {
        val raw = headers["Retry-After"]?.trim() ?: return null
        raw.toLongOrNull()?.let { return (it.coerceAtLeast(0) * 1000L) }
        return try {
            val at = ZonedDateTime.parse(raw, DateTimeFormatter.RFC_1123_DATE_TIME)
            (at.toInstant().toEpochMilli() - System.currentTimeMillis()).coerceAtLeast(0)
        } catch (_: Exception) {
            null
        }
    }

    @Serializable
    private data class MeResponse(val user: MeUser)
    @Serializable
    private data class MeUser(val id: String = "")

    @Serializable
    private data class CreateFolderRequest(
        val parent_id: String? = null,
        val name: String,
    )

    @Serializable
    private data class UpdateNodeRequest(
        val name: String,
        val parent_id: String,
    )

    @Serializable
    private data class ErrorResponse(val error: String = "", val message: String = "")

    @Serializable
    private data class CreateFolderResponse(val item: NodeSummary)

    @Serializable
    private data class FailedUpload(val name: String = "", val error: String = "")

    @Serializable
    private data class UploadResponse(
        val uploaded: List<NodeSummary> = emptyList(),
        val failed: List<FailedUpload> = emptyList(),
    )

    private companion object {
        const val LIST_PAGE_SIZE = 500
        const val MAX_LIST_PAGES = 200
    }
}
