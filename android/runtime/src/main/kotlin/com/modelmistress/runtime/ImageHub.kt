package com.modelmistress.runtime

import com.modelmistress.core.chat.ApiException
import com.modelmistress.core.images.ImageCatalog
import com.modelmistress.core.images.ImageSpec
import com.modelmistress.sd.ImageEngine

/**
 * The image models this device makes images with. One is loaded at a time, as the chat models are: asking for another
 * unloads the current one first. Generations hold a lock, so they queue behind each other. A loaded image model uses
 * memory alongside a loaded chat model, so the two are not loaded together by default.
 */
class ImageHub(
    private val library: ImageLibrary,
    private val settings: RuntimeSettings,
) : ImageCatalog {
    private val lock = Any()
    private var loaded: Pair<String, ImageEngine>? = null

    override fun models(): List<String> = library.list().map { it.id }

    /** The first installed image model, so a request that names none still gets one when there is one. */
    override fun defaultModelId(): String? = library.list().firstOrNull()?.id

    override fun generate(model: String, spec: ImageSpec): ByteArray = synchronized(lock) {
        engineFor(model).generate(spec)
    }

    /** Unloads the current image model and frees its memory. */
    fun unloadAll() {
        synchronized(lock) {
            loaded?.second?.close()
            loaded = null
        }
    }

    /** Must be called with the lock held. */
    private fun engineFor(model: String): ImageEngine {
        loaded?.takeIf { it.first == model }?.let { return it.second }
        loaded?.second?.close()
        loaded = null
        val file = library.find(model)
            ?: throw ApiException(404, "model_not_found", "The image model '$model' is not installed on this device.")
        val engine = try {
            ImageEngine.load(file.file.absolutePath, settings.threads)
        } catch (error: IllegalStateException) {
            throw ApiException(503, "model_unavailable", error.message ?: "The image model could not be loaded.")
        }
        loaded = model to engine
        return engine
    }
}
