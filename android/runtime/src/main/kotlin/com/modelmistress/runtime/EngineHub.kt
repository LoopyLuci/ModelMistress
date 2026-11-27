package com.modelmistress.runtime

import com.modelmistress.core.chat.ApiException
import com.modelmistress.core.chat.ChatBackend
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.GenerationStats
import com.modelmistress.core.chat.LocalEngine
import com.modelmistress.core.chat.ModelCatalog
import com.modelmistress.core.chat.ModelInfo
import com.modelmistress.core.chat.TokenSink
import com.modelmistress.core.models.ModelFormat
import com.modelmistress.engine.LlamaEngine
import com.modelmistress.litert.LiteRtEngine
import java.io.File

/**
 * The models this device answers with. Only one is loaded at a time, because a phone has little memory to spare:
 * asking for another model unloads the current one first. Replies hold a lock, so requests queue behind each other.
 * [cacheDirectory] is where LiteRT-LM keeps what it compiles, so a model loads faster the second time.
 */
class EngineHub(
    private val library: ModelLibrary,
    private val settings: RuntimeSettings,
    private val cacheDirectory: File,
) : ModelCatalog {
    private val lock = Any()

    /** Read without the lock, so a status request does not wait for a reply that is running. */
    @Volatile
    private var loaded: Loaded? = null

    private class Loaded(val id: String, val engine: LocalEngine)

    override fun models(): List<ModelInfo> = library.list().map { model ->
        ModelInfo(
            id = model.id,
            contextTokens = model.info?.contextLength,
            sizeBytes = model.sizeBytes,
            description = model.summary,
        )
    }

    override fun defaultModelId(): String? = settings.defaultModelId

    override fun backend(modelId: String): ChatBackend? =
        if (library.find(modelId) != null) Route(modelId) else null

    /**
     * Runs [block] with [modelId] loaded, holding the lock so no reply uses the engine meanwhile. The benchmark times models
     * this way; the model stays loaded afterwards until another one is asked for.
     */
    fun <T> withEngine(modelId: String, block: (LocalEngine) -> T): T = synchronized(lock) {
        block(engineFor(modelId))
    }

    /** Unloads the current model and frees its memory. */
    fun unloadAll() {
        synchronized(lock) {
            loaded?.engine?.close()
            loaded = null
        }
    }

    /** The id of the model that is loaded now, or null when none is. */
    fun loadedModelId(): String? = loaded?.id

    /** Unloads [modelId] when it is the loaded one, so a model removed from the device is not kept in memory. */
    fun unloadModel(modelId: String) {
        synchronized(lock) {
            if (loaded?.id == modelId) {
                loaded?.engine?.close()
                loaded = null
            }
        }
    }

    private inner class Route(private val modelId: String) : ChatBackend {
        override fun complete(
            messages: List<ChatMessage>,
            options: GenerationOptions,
            sink: TokenSink,
        ): GenerationStats = synchronized(lock) {
            engineFor(modelId).complete(messages, options, sink)
        }
    }

    /** Must be called with the lock held. */
    private fun engineFor(modelId: String): LocalEngine {
        loaded?.takeIf { it.id == modelId }?.let { return it.engine }
        loaded?.engine?.close()
        loaded = null
        val model = library.find(modelId)
            ?: throw ApiException(404, "model_not_found", "The model '$modelId' is not installed on this device.")
        val engine: LocalEngine = try {
            when (model.format) {
                ModelFormat.GGUF -> LlamaEngine.load(model.file.absolutePath, settings.contextTokens, settings.threads)
                ModelFormat.LITERT_LM -> LiteRtEngine.load(
                    model.file.absolutePath,
                    cacheDirectory,
                    settings.contextTokens,
                    settings.threads,
                )
            }
        } catch (error: IllegalStateException) {
            throw ApiException(503, "model_unavailable", error.message ?: "The model could not be loaded.")
        }
        loaded = Loaded(modelId, engine)
        return engine
    }
}
