package app.nimbo.backup.data.api

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.MultipartBody
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody
import okhttp3.RequestBody.Companion.asRequestBody
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import java.io.File
import java.io.IOException
import java.util.concurrent.TimeUnit

/** Server rejected our credentials (HTTP 401). */
class NimboAuthException(message: String) : IOException(message)

/** Server answered with a non-2xx status. */
class NimboApiException(val code: Int, message: String) : IOException("HTTP $code: $message")

@Serializable
data class NodeSummary(
    val id: String,
    val kind: String,
    val name: String,
    val mime: String? = null,
    val size: Long = 0,
)

@Serializable
data class FolderListing(
    val folder: FolderRef,
    val items: List<NodeSummary> = emptyList(),
) {
    @Serializable
    data class FolderRef(val id: String, val name: String)
}

/**
 * Thin client for the Nimbo files API, authenticated with a device token via
 * `Authorization: Bearer`.
 */
class NimboClient(baseUrl: String, private val token: String) {

    private val root = baseUrl.trim().trimEnd('/')
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

    /** Cheap connectivity + credential check: GET /api/instance. */
    fun testConnection() {
        request(buildRequest("/api/instance")).use { resp ->
            if (resp.code == 401) throw NimboAuthException("token rejected by server")
            if (!resp.isSuccessful) throw NimboApiException(resp.code, resp.message)
        }
    }

    /** GET /api/files?folder_id= — lists children of a folder ("" = drive root). */
    fun listFolder(folderId: String): FolderListing {
        val q = if (folderId.isBlank()) "" else "?folder_id=${urlEncode(folderId)}"
        request(buildRequest("/api/files$q")).use { resp ->
            val body = resp.body?.string().orEmpty()
            if (resp.code == 401) throw NimboAuthException("token rejected by server")
            if (!resp.isSuccessful) throw NimboApiException(resp.code, body.errorMessage())
            return json.decodeFromString(FolderListing.serializer(), body)
        }
    }

    /** POST /api/files/folders — create a folder; fails 409 when the name is taken. */
    fun createFolder(parentId: String, name: String): NodeSummary {
        val payload = json
            .encodeToString(
                CreateFolderRequest.serializer(),
                CreateFolderRequest(parentId.ifBlank { null }, name),
            )
            .toRequestBody(jsonMediaType)
        request(buildRequest("/api/files/folders", payload)).use { resp ->
            val body = resp.body?.string().orEmpty()
            if (resp.code == 401) throw NimboAuthException("token rejected by server")
            if (!resp.isSuccessful) throw NimboApiException(resp.code, body.errorMessage())
            return json.decodeFromString(CreateFolderResponse.serializer(), body).item
        }
    }

    /** DELETE /api/files/{id} — used to replace a file whose content changed. */
    fun deleteNode(id: String) {
        request(buildRequest("/api/files/${urlEncode(id)}", method = "DELETE")).use { resp ->
            val body = resp.body?.string().orEmpty()
            if (resp.code == 401) throw NimboAuthException("token rejected by server")
            if (!resp.isSuccessful) throw NimboApiException(resp.code, body.errorMessage())
        }
    }

    /** POST /api/files/upload?folder_id= — multipart upload, field name `file`. */
    fun upload(folderId: String, name: String, tempFile: File): NodeSummary {
        val q = if (folderId.isBlank()) "" else "?folder_id=${urlEncode(folderId)}"
        val part = MultipartBody.Builder()
            .setType(MultipartBody.FORM)
            .addFormDataPart(
                "file",
                name,
                tempFile.asRequestBody("application/octet-stream".toMediaType()),
            )
            .build()
        request(buildRequest("/api/files/upload$q", part)).use { resp ->
            val body = resp.body?.string().orEmpty()
            if (resp.code == 401) throw NimboAuthException("token rejected by server")
            if (!resp.isSuccessful) throw NimboApiException(resp.code, body.errorMessage())
            val parsed = json.decodeFromString(UploadResponse.serializer(), body)
            val failed = parsed.failed.firstOrNull()
            if (failed != null) throw NimboApiException(400, failed.error)
            return parsed.uploaded.firstOrNull()
                ?: throw NimboApiException(500, "upload returned no file entry")
        }
    }

    private fun buildRequest(path: String, body: RequestBody? = null, method: String = "GET"): Request {
        val builder = Request.Builder().url(root + path)
        when {
            body == null && method == "GET" -> builder.get()
            body == null -> builder.delete()
            method != "POST" -> throw IllegalArgumentException("unsupported method")
            else -> builder.post(body)
        }
        return builder.build()
    }

    private fun request(request: Request): Response = http.newCall(request).execute()

    private fun String.errorMessage(): String = try {
        json.decodeFromString(ErrorResponse.serializer(), this).error.ifBlank { "HTTP error" }
    } catch (_: Exception) {
        take(200)
    }

    private fun urlEncode(value: String): String =
        java.net.URLEncoder.encode(value, Charsets.UTF_8.name())

    @kotlinx.serialization.Serializable
    private data class CreateFolderRequest(
        val parent_id: String? = null,
        val name: String,
    )

    @kotlinx.serialization.Serializable
    private data class ErrorResponse(val error: String = "", val message: String = "")

    @kotlinx.serialization.Serializable
    private data class CreateFolderResponse(val item: NodeSummary)

    @kotlinx.serialization.Serializable
    private data class FailedUpload(val name: String = "", val error: String = "")

    @kotlinx.serialization.Serializable
    private data class UploadResponse(
        val uploaded: List<NodeSummary> = emptyList(),
        val failed: List<FailedUpload> = emptyList(),
    )
}
