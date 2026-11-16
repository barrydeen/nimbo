package app.nimbo.backup.ui

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.LruCache
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Box
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** Small process-wide bitmap cache so gallery scrolling does not re-download. */
object RemoteImageCache {
    private val cache = object : LruCache<String, Bitmap>(24 * 1024 * 1024) {
        override fun sizeOf(key: String, value: Bitmap): Int = value.byteCount
    }

    fun get(key: String): Bitmap? = cache.get(key)

    fun put(key: String, bitmap: Bitmap) {
        cache.put(key, bitmap)
    }
}

/**
 * Authenticated image loader for thumbnails and full-size previews. The request
 * function carries the device token through [NimboClient], so images are never
 * fetched through a token-less public URL.
 */
@Composable
fun RemoteImage(
    cacheKey: String,
    load: suspend () -> ByteArray?,
    modifier: Modifier = Modifier,
    contentDescription: String? = null,
    contentScale: ContentScale = ContentScale.Crop,
    maxWidth: Int = 2048,
    placeholder: @Composable () -> Unit,
) {
    var bitmap by remember(cacheKey) { mutableStateOf(RemoteImageCache.get(cacheKey)) }
    LaunchedEffect(cacheKey) {
        if (bitmap != null) return@LaunchedEffect
        val bytes = load() ?: return@LaunchedEffect
        val decoded = withContext(Dispatchers.Default) { decodeSampled(bytes, maxWidth) }
        if (decoded != null) {
            RemoteImageCache.put(cacheKey, decoded)
            bitmap = decoded
        }
    }
    val current = bitmap
    if (current != null) {
        Image(
            bitmap = current.asImageBitmap(),
            contentDescription = contentDescription,
            modifier = modifier,
            contentScale = contentScale,
        )
    } else {
        Box(modifier = modifier, contentAlignment = Alignment.Center) {
            placeholder()
        }
    }
}

private fun decodeSampled(bytes: ByteArray, maxWidth: Int): Bitmap? {
    if (bytes.isEmpty()) return null
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
    if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null

    var sample = 1
    val limit = maxWidth.coerceAtLeast(256)
    while (bounds.outWidth / (sample * 2) >= limit) {
        sample *= 2
    }
    val options = BitmapFactory.Options().apply {
        inSampleSize = sample
        inPreferredConfig = Bitmap.Config.ARGB_8888
    }
    return try {
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size, options)
    } catch (_: OutOfMemoryError) {
        null
    }
}
