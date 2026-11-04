package org.theconstruct.desktop.sim

import coil3.EventListener
import coil3.ImageLoader
import coil3.PlatformContext
import coil3.SingletonImageLoader
import coil3.intercept.Interceptor
import coil3.network.okhttp.OkHttpNetworkFetcherFactory
import coil3.request.CachePolicy
import coil3.request.ErrorResult
import coil3.request.ImageRequest
import coil3.request.ImageResult
import coil3.request.SuccessResult
import java.io.File
import java.io.FileNotFoundException

/**
 * Image loading for the simulator (docs/simulator.md, DECISIONS: sim image loading).
 *
 * Fixture mode never touches the network: [FixtureMediaInterceptor] serves any image URL whose path ends in
 * `/storage/<file>` or `/v1/pixelfed/media/<file>` from `fake-pixelfed/fixtures/media/<file>`; every other URL fails
 * at once (the widget shows its error state), so screenshots are deterministic. Live mode loads over HTTP
 * (OkHttp) from the pod or wherever the URLs point. Both modes disable the disk cache. `wait-images` (and
 * every `shot`) waits for [org.theconstruct.ImageLoads] to drain.
 */
object SimImages {
    /** [System.nanoTime] of the last successful load (0 = none), so a shot can let its crossfade finish. */
    @Volatile
    var lastSuccessNanos: Long = 0L
        private set

    fun install(mode: String, mediaDir: File? = findPixelfedMedia()) {
        val loader = ImageLoader.Builder(PlatformContext.INSTANCE)
            .components {
                if (mode == "fixture") {
                    add(FixtureMediaInterceptor(mediaDir))
                } else {
                    add(OkHttpNetworkFetcherFactory())
                }
            }
            .diskCachePolicy(CachePolicy.DISABLED)
            .eventListener(object : EventListener() {
                override fun onSuccess(request: ImageRequest, result: SuccessResult) {
                    lastSuccessNanos = System.nanoTime()
                }
            })
            .build()
        SingletonImageLoader.setUnsafe(loader)
    }
}

/** See [SimImages]. [mediaDir] null (not found) fails every request. */
class FixtureMediaInterceptor(private val mediaDir: File?) : Interceptor {
    override suspend fun intercept(chain: Interceptor.Chain): ImageResult {
        val request = chain.request
        val data = request.data
        val url = (data as? String) ?: (data as? coil3.Uri)?.toString() ?: return chain.proceed()
        if (!url.startsWith("http://") && !url.startsWith("https://")) return chain.proceed()
        val file = resolve(url)
            ?: return ErrorResult(null, request, FileNotFoundException("fixture mode: no fixture media for $url"))
        return chain.withRequest(request.newBuilder().data(file).build()).proceed()
    }

    /** The fixture file for [url], or null. */
    fun resolve(url: String): File? {
        val dir = mediaDir ?: return null
        val name = MEDIA_PATH.find(url.substringBefore('?').substringBefore('#'))?.groupValues?.get(1) ?: return null
        if (name.startsWith(".")) return null
        return listOf(name, "$name.png").map { File(dir, it) }.firstOrNull { it.isFile }
    }

    private companion object {
        val MEDIA_PATH = Regex("""/(?:storage|v1/pixelfed/media)/([A-Za-z0-9._-]+)$""")
    }
}
