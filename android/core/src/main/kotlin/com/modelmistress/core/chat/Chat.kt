package com.modelmistress.core.chat

enum class Role(val wire: String) {
    SYSTEM("system"),
    USER("user"),
    ASSISTANT("assistant");

    companion object {
        fun fromWire(value: String): Role? = entries.firstOrNull { it.wire == value }
    }
}

data class ChatMessage(val role: Role, val content: String)

data class GenerationOptions(
    val maxTokens: Int = DEFAULT_MAX_TOKENS,
    val temperature: Float = 0.7f,
    val topP: Float = 0.95f,
    val topK: Int = 40,
    val seed: Long? = null,
) {
    init {
        require(maxTokens in 1..MAX_TOKENS) { "max_tokens must be between 1 and $MAX_TOKENS." }
        require(temperature in 0f..2f) { "temperature must be between 0 and 2." }
        require(topP in 0f..1f) { "top_p must be between 0 and 1." }
        require(topK in 1..MAX_TOP_K) { "top_k must be between 1 and $MAX_TOP_K." }
    }

    companion object {
        const val DEFAULT_MAX_TOKENS = 512
        const val MAX_TOKENS = 4096
        const val MAX_TOP_K = 1000
    }
}

data class ChatRequest(
    val model: String?,
    val messages: List<ChatMessage>,
    val stream: Boolean,
    val options: GenerationOptions,
)

/** A model the device can answer with. [contextTokens] is null when the file does not say. */
data class ModelInfo(
    val id: String,
    val contextTokens: Int?,
    val sizeBytes: Long,
    val description: String,
)

enum class FinishReason(val wire: String) {
    STOP("stop"),
    LENGTH("length"),
}

data class GenerationStats(
    val promptTokens: Int,
    val completionTokens: Int,
    val finishReason: FinishReason,
)

fun interface TokenSink {
    /** Receives the next piece of reply text. Return false to stop generating. */
    fun onText(text: String): Boolean
}

/** One loaded model that can answer chat messages. Implementations run one reply at a time. */
interface ChatBackend {
    fun complete(messages: List<ChatMessage>, options: GenerationOptions, sink: TokenSink): GenerationStats
}

/** The models a device holds and the backend for each. */
interface ModelCatalog {
    fun models(): List<ModelInfo>
    fun defaultModelId(): String?
    fun backend(modelId: String): ChatBackend?
}

/** A request the API refuses. [status] is the HTTP status and [code] the stable error code clients can match. */
class ApiException(val status: Int, val code: String, message: String) : RuntimeException(message)
