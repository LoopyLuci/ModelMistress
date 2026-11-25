package com.modelmistress.runtime

import com.modelmistress.core.chat.ApiException
import com.modelmistress.core.chat.ChatBackend
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.GenerationStats
import com.modelmistress.core.chat.ModelCatalog
import com.modelmistress.core.chat.ModelInfo
import com.modelmistress.core.chat.TokenSink
import com.modelmistress.engine.LlamaEngine

/**
 * The models this device answers with. Only one is loaded at a time, because a phone has little memory to spare:
 * asking for another model unloads the current one first. Replies hold a lock, so requests queue behind each other.
 */
class EngineHub(
    private val library: ModelLibrary,
    private val settings: RuntimeSettings,
) : ModelCatalog {
    private val lock = Any()
    private var loaded: Loaded? = null

    private class Loaded(val id: String, val engine: LlamaEngine)

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

    /** Unloads the current model and frees its memory. */
    fun unloadAll() {
        synchronized(lock) {
            loaded?.engine?.close()
            loaded = null
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
    private fun engineFor(modelId: String): LlamaEngine {
        loaded?.takeIf { it.id == modelId }?.let { return it.engine }
        loaded?.engine?.close()
        loaded = null
        val model = library.find(modelId)
            ?: throw ApiException(404, "model_not_found", "The model '$modelId' is not installed on this device.")
        val engine = try {
            LlamaEngine.load(model.file.absolutePath, settings.contextTokens, settings.threads)
        } catch (error: IllegalStateException) {
            throw ApiException(503, "model_unavailable", error.message ?: "The model could not be loaded.")
        }
        loaded = Loaded(modelId, engine)
        return engine
    }
}
