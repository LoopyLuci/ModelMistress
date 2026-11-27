package com.modelmistress.core.chat

import com.modelmistress.core.json.Json
import com.modelmistress.core.json.JsonException
import java.security.SecureRandom

/** The OpenAI wire format for chat completions, model lists and errors. */
object OpenAi {
    const val MAX_MESSAGES = 256
    const val MAX_CHARACTERS = 256 * 1024

    private const val ID_LENGTH = 24
    private const val ID_ALPHABET = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
    private val random = SecureRandom()

    fun newId(): String = "chatcmpl-" + String(CharArray(ID_LENGTH) { ID_ALPHABET[random.nextInt(ID_ALPHABET.length)] })

    fun parseChatRequest(body: String): ChatRequest {
        val root = try {
            Json.parse(body)
        } catch (error: JsonException) {
            throw ApiException(400, "invalid_json", "The request body is not valid JSON. ${error.message}")
        }
        val fields = root as? Map<*, *> ?: throw invalid("The request body must be a JSON object.")
        if (fields.containsKey("tools") || fields.containsKey("functions")) {
            throw ApiException(
                400,
                "tools_unsupported",
                "Tool definitions are not accepted. The device offers its own tools: send model 'misty' or a chat, " +
                    "or set mm.tools to false to turn them off.",
            )
        }
        if (fields["n"]?.let { it != 1L } == true) {
            throw ApiException(400, "unsupported_parameter", "Only n = 1 is supported.")
        }
        val model = fields["model"]?.let { it as? String ?: throw invalid("model must be a string.") }
        val stream = when (val value = fields["stream"]) {
            null -> false
            is Boolean -> value
            else -> throw invalid("stream must be true or false.")
        }
        val maxTokens = (fields["max_tokens"] ?: fields["max_completion_tokens"])
            ?.let { whole(it, "max_tokens", 1L, GenerationOptions.MAX_TOKENS.toLong()).toInt() }
            ?: GenerationOptions.DEFAULT_MAX_TOKENS
        val temperature = fields["temperature"]?.let { number(it, "temperature", 0.0, 2.0).toFloat() } ?: 0.7f
        val topP = fields["top_p"]?.let { number(it, "top_p", 0.0, 1.0).toFloat() } ?: 0.95f
        val topK = fields["top_k"]?.let { whole(it, "top_k", 1L, GenerationOptions.MAX_TOP_K.toLong()).toInt() } ?: 40
        val seed = fields["seed"]?.let { whole(it, "seed", Long.MIN_VALUE, Long.MAX_VALUE) }
        return ChatRequest(
            model = model,
            messages = messages(fields["messages"]),
            stream = stream,
            options = GenerationOptions(maxTokens, temperature, topP, topK, seed),
            mm = mmOptions(fields["mm"]),
        )
    }

    private fun mmOptions(value: Any?): MmOptions {
        if (value == null) return MmOptions()
        val fields = value as? Map<*, *> ?: throw invalid("mm must be an object.")
        val chat = fields["chat"]?.let { it as? String ?: throw invalid("mm.chat must be a string.") }
        val sandbox = fields["sandbox"]?.let { it as? String ?: throw invalid("mm.sandbox must be a string.") }
        val tools = when (val value = fields["tools"]) {
            null -> null
            is Boolean -> value
            else -> throw invalid("mm.tools must be true or false.")
        }
        return MmOptions(chatId = chat, sandboxId = sandbox, toolsAllowed = tools)
    }

    /**
     * One reply as a single JSON object. [extra] is added at the top level, under the router's own keys; a standard client ignores
     * keys it does not know.
     */
    fun completion(
        id: String,
        model: String,
        created: Long,
        text: String,
        stats: GenerationStats,
        extra: Map<String, Any?> = emptyMap(),
    ): String =
        Json.write(
            linkedMapOf<String, Any?>(
                "id" to id,
                "object" to "chat.completion",
                "created" to created,
                "model" to model,
                "choices" to listOf(
                    mapOf(
                        "index" to 0,
                        "message" to mapOf("role" to "assistant", "content" to text),
                        "finish_reason" to stats.finishReason.wire,
                    ),
                ),
                "usage" to usage(stats),
            ).apply { putAll(extra) },
        )

    /** One streamed chunk. [delta] is the part of the reply it carries; [finishReason] is set on the last chunk. */
    fun chunk(id: String, model: String, created: Long, delta: Map<String, Any?>, finishReason: String?): String =
        Json.write(
            mapOf(
                "id" to id,
                "object" to "chat.completion.chunk",
                "created" to created,
                "model" to model,
                "choices" to listOf(mapOf("index" to 0, "delta" to delta, "finish_reason" to finishReason)),
            ),
        )

    fun models(models: List<ModelInfo>, created: Long): String = Json.write(
        mapOf(
            "object" to "list",
            "data" to models.map { model ->
                mapOf(
                    "id" to model.id,
                    "object" to "model",
                    "created" to created,
                    "owned_by" to "modelmistress",
                    "context_length" to model.contextTokens,
                    "size_bytes" to model.sizeBytes,
                    "description" to model.description,
                )
            },
        ),
    )

    fun error(message: String, type: String, code: String): String = Json.write(
        mapOf("error" to mapOf("message" to message, "type" to type, "code" to code)),
    )

    private fun usage(stats: GenerationStats): Map<String, Any?> = mapOf(
        "prompt_tokens" to stats.promptTokens,
        "completion_tokens" to stats.completionTokens,
        "total_tokens" to stats.promptTokens + stats.completionTokens,
    )

    private fun invalid(message: String) = ApiException(400, "invalid_request", message)

    private fun messages(value: Any?): List<ChatMessage> {
        val list = value as? List<*> ?: throw invalid("messages must be a list.")
        if (list.isEmpty()) throw invalid("messages must not be empty.")
        if (list.size > MAX_MESSAGES) throw invalid("A request may hold at most $MAX_MESSAGES messages.")
        var characters = 0
        return list.map { item ->
            val message = item as? Map<*, *> ?: throw invalid("Each message must be an object.")
            if (message.containsKey("tool_calls") || message.containsKey("tool_call_id")) {
                throw ApiException(400, "tools_unsupported", "Tool calls are not available on this device.")
            }
            val roleName = message["role"] as? String ?: throw invalid("Each message needs a role.")
            val role = if (roleName == "developer") {
                Role.SYSTEM
            } else {
                Role.fromWire(roleName)
                    ?: throw ApiException(400, "unsupported_role", "The role '$roleName' is not supported.")
            }
            val text = content(message["content"], role)
            characters += text.length
            if (characters > MAX_CHARACTERS) throw invalid("The messages are longer than this device accepts.")
            ChatMessage(role, text)
        }
    }

    private fun content(value: Any?, role: Role): String = when (value) {
        null -> if (role == Role.ASSISTANT) "" else throw invalid("Each message needs content.")
        is String -> value
        is List<*> -> value.joinToString("") { part ->
            val piece = part as? Map<*, *> ?: throw invalid("Each content part must be an object.")
            if (piece["type"] != "text") {
                throw ApiException(400, "unsupported_content", "Only text content is supported on this device.")
            }
            piece["text"] as? String ?: throw invalid("A text part needs a text field.")
        }
        else -> throw invalid("Message content must be text.")
    }

    private fun whole(value: Any?, name: String, min: Long, max: Long): Long {
        val number: Long = when (value) {
            is Long -> value
            is Double -> if (value % 1.0 == 0.0 && value >= -9.0e18 && value <= 9.0e18) value.toLong() else null
            else -> null
        } ?: throw invalid("$name must be a whole number.")
        if (number < min || number > max) throw invalid("$name must be between $min and $max.")
        return number
    }

    private fun number(value: Any?, name: String, min: Double, max: Double): Double {
        val number = when (value) {
            is Long -> value.toDouble()
            is Double -> value
            else -> throw invalid("$name must be a number.")
        }
        if (number.isNaN() || number < min || number > max) throw invalid("$name must be between $min and $max.")
        return number
    }
}
