package com.modelmistress.core.images

import com.modelmistress.core.chat.ApiException
import com.modelmistress.core.json.Json
import com.modelmistress.core.text.Base64

/** The image models a device can make with. One image is made at a time. */
interface ImageCatalog {
    /** The ids of the image models installed on this device. */
    fun models(): List<String>

    /** The model that answers when a request names none, or names "auto". Null when no image model is installed. */
    fun defaultModelId(): String?

    /** Makes one image with [model] and returns it as PNG bytes. Throws [ApiException] when it cannot. */
    fun generate(model: String, spec: ImageSpec): ByteArray
}

/** One image to make. A [seed] of -1 picks a random one. */
data class ImageSpec(
    val prompt: String,
    val negativePrompt: String,
    val width: Int,
    val height: Int,
    val steps: Int,
    val seed: Long,
)

/** A checked `POST /v1/images/generations` request: the model it names (null for the default), the image, and how many. */
data class ImageRequest(
    val model: String?,
    val image: ImageSpec,
    val count: Int,
) {
    /** The spec for the [index]th image. A fixed seed moves on by one for each image, so the images differ. */
    fun spec(index: Int): ImageSpec =
        if (image.seed < 0) image else image.copy(seed = image.seed + index)
}

/** The OpenAI-compatible image request and response, checked against the limits this device can run. */
object ImageApi {
    const val MAX_PROMPT_CHARS = 4000

    /** Fewer than the desktop's ten: each image takes minutes on a phone's CPU. */
    const val MAX_IMAGES = 4
    const val MIN_SIDE = 64
    const val MAX_SIDE = 2048
    const val DEFAULT_SIDE = 512
    const val DEFAULT_STEPS = 20
    const val MAX_STEPS = 100

    private const val SIZE_MESSAGE = "size must be WIDTHxHEIGHT, with each side a multiple of 8 from 64 to 2048."

    /** Reads and checks a request body. Throws [ApiException] with a stable code for each refusal. */
    fun parse(body: String): ImageRequest {
        val fields = runCatching { Json.parse(body) as? Map<*, *> }.getOrNull()
            ?: throw ApiException(400, "invalid_json", "The request body is not a JSON object.")
        val prompt = text(fields, "prompt")?.takeIf { it.isNotBlank() } ?: throw invalid("A prompt is required.")
        if (prompt.length > MAX_PROMPT_CHARS) {
            throw invalid("The prompt is longer than $MAX_PROMPT_CHARS characters.")
        }
        val model = text(fields, "model")?.trim()?.takeIf { it.isNotEmpty() && it != "auto" }
        val count = whole(fields, "n") ?: 1
        if (count !in 1..MAX_IMAGES) throw invalid("n must be between 1 and $MAX_IMAGES.")
        val (width, height) = size(text(fields, "size") ?: "${DEFAULT_SIDE}x$DEFAULT_SIDE")
        val steps = whole(fields, "steps") ?: DEFAULT_STEPS
        if (steps !in 1..MAX_STEPS) throw invalid("steps must be between 1 and $MAX_STEPS.")
        val seed = whole(fields, "seed")?.toLong() ?: -1L
        if (seed < -1) throw invalid("seed must be -1 for a random image, or a whole number from 0 up.")
        val format = text(fields, "response_format") ?: "b64_json"
        if (format != "b64_json") {
            throw ApiException(400, "unsupported_parameter", "Only response_format b64_json is supported on this device.")
        }
        return ImageRequest(
            model = model,
            image = ImageSpec(
                prompt = prompt,
                negativePrompt = text(fields, "negative_prompt").orEmpty(),
                width = width,
                height = height,
                steps = steps,
                seed = seed,
            ),
            count = count,
        )
    }

    /** The response body: the images as base64 PNG, in the order they were made. */
    fun response(created: Long, images: List<ByteArray>): String = Json.write(
        mapOf(
            "created" to created,
            "data" to images.map { mapOf("b64_json" to Base64.encode(it)) },
        ),
    )

    private fun size(text: String): Pair<Int, Int> {
        val parts = text.trim().lowercase().split('x')
        if (parts.size != 2) throw invalid(SIZE_MESSAGE)
        val width = parts[0].toIntOrNull()
        val height = parts[1].toIntOrNull()
        if (width == null || height == null || !validSide(width) || !validSide(height)) throw invalid(SIZE_MESSAGE)
        return width to height
    }

    private fun validSide(side: Int): Boolean = side in MIN_SIDE..MAX_SIDE && side % 8 == 0

    private fun text(fields: Map<*, *>, key: String): String? {
        val value = fields[key] ?: return null
        return value as? String ?: throw invalid("$key must be a string.")
    }

    /** A whole number field, or null when the field is absent. */
    private fun whole(fields: Map<*, *>, key: String): Int? {
        val value = fields[key] ?: return null
        val number = (value as? Number)?.toDouble() ?: throw invalid("$key must be a whole number.")
        if (number != Math.rint(number) || number < Int.MIN_VALUE || number > Int.MAX_VALUE) {
            throw invalid("$key must be a whole number.")
        }
        return number.toInt()
    }

    private fun invalid(message: String) = ApiException(400, "invalid_request", message)
}
