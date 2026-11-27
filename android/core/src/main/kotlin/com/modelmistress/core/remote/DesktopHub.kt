package com.modelmistress.core.remote

import com.modelmistress.core.chat.ApiException
import com.modelmistress.core.chat.ChatBackend
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.FinishReason
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.GenerationStats
import com.modelmistress.core.chat.ModelInfo
import com.modelmistress.core.chat.TokenSink
import com.modelmistress.core.json.Json
import com.modelmistress.core.json.JsonException
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL

/**
 * A ModelMistress hub on another machine that this device is paired with, such as the desktop app at home.
 *
 * Reached by address: [url] is the hub's root, for example `http://100.101.98.77:8000` over Tailscale or a LAN.
 * Reached through the Transferd overlay: [overlayId] is the desktop's overlay node id, and [url] is `overlay://<id>`,
 * which names it; the address a request really goes to is a loopback endpoint that the overlay gives
 * (see [OverlayEndpoints]). [key] is a peer key the desktop issued for this device; the hub accepts it for its OpenAI
 * routes only, whichever way it is reached.
 */
data class DesktopHub(val name: String, val url: String, val key: String, val overlayId: String? = null) {
    init {
        require(NAME.matches(name)) { "The desktop name uses 1 to 32 letters, digits, '-' or '_'." }
        if (overlayId == null) {
            require(url.startsWith("http://") || url.startsWith("https://")) {
                "The desktop address must start with http:// or https://."
            }
        } else {
            require(NODE_ID.matches(overlayId)) { "The overlay id is 64 hexadecimal characters, as the desktop shows it." }
            require(url == "$OVERLAY_SCHEME$overlayId") { "A desktop reached through the overlay is named by its overlay id." }
        }
        require(key.isNotBlank()) { "The desktop needs the peer key it issued for this device." }
    }

    /** Whether requests to this desktop travel over the Transferd overlay. */
    val throughOverlay: Boolean get() = overlayId != null

    companion object {
        private val NAME = Regex("[A-Za-z0-9_-]{1,32}")
        private val NODE_ID = Regex("[0-9a-f]{64}")
        private const val OVERLAY_SCHEME = "overlay://"

        /** Trims the address and drops a trailing slash or `/v1`, so either form can be pasted. */
        fun of(name: String, url: String, key: String): DesktopHub =
            DesktopHub(name.trim(), url.trim().trimEnd('/').removeSuffix("/v1").trimEnd('/'), key.trim())

        /** A desktop reached through the overlay, by the overlay node id it showed when it was paired. */
        fun overlay(name: String, overlayId: String, key: String): DesktopHub {
            val id = overlayId.trim().lowercase()
            return DesktopHub(name.trim(), "$OVERLAY_SCHEME$id", key.trim(), id)
        }
    }
}

/**
 * Gives the address a paired desktop's hub answers on from this device. The overlay gives a loopback address for each
 * paired node; requests sent to it travel over the overlay. Throws [IOException] when the desktop cannot be reached.
 */
fun interface OverlayEndpoints {
    /** The root, `http://127.0.0.1:<port>`, of the hub on the desktop with [overlayId]. */
    fun hubRoot(overlayId: String): String

    companion object {
        /** For a build with no overlay: no desktop can be reached through it. */
        val UNAVAILABLE = OverlayEndpoints {
            throw IOException("This app cannot reach a desktop through the Transferd overlay.")
        }
    }
}

/**
 * Talks to one [DesktopHub] over its OpenAI-compatible routes. Every call opens its own connection, so a reply that is
 * stopped early simply closes the connection, and the hub stops generating when it notices.
 */
class DesktopClient(private val hub: DesktopHub, private val endpoints: OverlayEndpoints = OverlayEndpoints.UNAVAILABLE) {

    /** Throws when the hub cannot be reached or refuses the key. */
    fun health() {
        get("/v1/health")
    }

    /** The hub's models with their ids as the hub names them. */
    fun models(): List<ModelInfo> {
        val root = Json.parse(get("/v1/models")) as? Map<*, *>
        val data = root?.get("data") as? List<*> ?: throw failure("sent a model list this app cannot read.")
        return data.mapNotNull { entry ->
            val model = entry as? Map<*, *> ?: return@mapNotNull null
            val id = model["id"] as? String ?: return@mapNotNull null
            // The desktop puts the file facts under "meta" (architecture, context_length, size_gb, size_label).
            val meta = model["meta"] as? Map<*, *>
            val sizeGb = (meta?.get("size_gb") as? Number)?.toDouble()
            ModelInfo(
                id = id,
                contextTokens = ((model["context_length"] ?: meta?.get("context_length")) as? Number)?.toInt(),
                sizeBytes = (model["size_bytes"] as? Number)?.toLong()
                    ?: sizeGb?.let { (it * 1_000_000_000).toLong() }
                    ?: 0L,
                description = listOfNotNull(
                    model["description"] as? String,
                    meta?.get("architecture") as? String,
                    meta?.get("size_label") as? String,
                ).joinToString(" "),
            )
        }
    }

    /** Streams a reply from the hub. [sink] returning false stops it. */
    fun chat(model: String, messages: List<ChatMessage>, options: GenerationOptions, sink: TokenSink): GenerationStats {
        val payload = Json.write(
            linkedMapOf(
                "model" to model,
                "stream" to true,
                "max_tokens" to options.maxTokens,
                "temperature" to options.temperature.toDouble(),
                "top_p" to options.topP.toDouble(),
                "messages" to messages.map { mapOf("role" to it.role.wire, "content" to it.content) },
            ),
        )
        val connection = open("/v1/chat/completions", "POST", CHAT_READ_TIMEOUT_MS)
        try {
            connection.doOutput = true
            connection.setRequestProperty("Content-Type", "application/json")
            connection.setRequestProperty("Accept", "text/event-stream")
            try {
                connection.outputStream.use { it.write(payload.toByteArray(Charsets.UTF_8)) }
                val status = connection.responseCode
                if (status >= 400) throw failure(status, errorText(connection))
                return readStream(connection, sink)
            } catch (error: IOException) {
                throw unreachable(error)
            }
        } finally {
            connection.disconnect()
        }
    }

    private fun readStream(connection: HttpURLConnection, sink: TokenSink): GenerationStats {
        var pieces = 0
        var promptTokens = 0
        var completionTokens: Int? = null
        var finish: String? = null
        connection.inputStream.bufferedReader(Charsets.UTF_8).use { reader ->
            while (true) {
                val line = reader.readLine() ?: break
                if (!line.startsWith("data:")) continue
                val data = line.removePrefix("data:").trim()
                if (data == "[DONE]") break
                val event = Json.parse(data) as? Map<*, *> ?: continue
                (event["usage"] as? Map<*, *>)?.let { usage ->
                    (usage["prompt_tokens"] as? Number)?.let { promptTokens = it.toInt() }
                    (usage["completion_tokens"] as? Number)?.let { completionTokens = it.toInt() }
                }
                val choice = (event["choices"] as? List<*>)?.firstOrNull() as? Map<*, *> ?: continue
                (choice["finish_reason"] as? String)?.let { finish = it }
                val piece = (choice["delta"] as? Map<*, *>)?.get("content") as? String
                if (!piece.isNullOrEmpty()) {
                    pieces++
                    if (!sink.onText(piece)) break
                }
            }
        }
        return GenerationStats(
            promptTokens = promptTokens,
            completionTokens = completionTokens ?: pieces,
            finishReason = if (finish == "length") FinishReason.LENGTH else FinishReason.STOP,
        )
    }

    private fun get(path: String): String {
        val connection = open(path, "GET", READ_TIMEOUT_MS)
        try {
            val status = try {
                connection.responseCode
            } catch (error: IOException) {
                throw unreachable(error)
            }
            if (status >= 400) throw failure(status, errorText(connection))
            return try {
                connection.inputStream.bufferedReader(Charsets.UTF_8).use { it.readText() }
            } catch (error: IOException) {
                throw unreachable(error)
            }
        } finally {
            connection.disconnect()
        }
    }

    /** The hub's root: the desktop's own address, or the loopback endpoint the overlay gives for a desktop reached through it. */
    private fun root(): String {
        val overlayId = hub.overlayId ?: return hub.url
        return try {
            endpoints.hubRoot(overlayId)
        } catch (error: Exception) {
            throw unreachable(error)
        }
    }

    private fun open(path: String, method: String, readTimeout: Int): HttpURLConnection {
        val root = root()
        val connection = try {
            URL(root + path).openConnection() as HttpURLConnection
        } catch (error: IOException) {
            throw unreachable(error)
        }
        connection.requestMethod = method
        connection.connectTimeout = CONNECT_TIMEOUT_MS
        connection.readTimeout = readTimeout
        connection.setRequestProperty("Authorization", "Bearer ${hub.key}")
        return connection
    }

    private fun errorText(connection: HttpURLConnection): String {
        val body = try {
            connection.errorStream?.bufferedReader(Charsets.UTF_8)?.use { it.readText() }.orEmpty()
        } catch (error: IOException) {
            ""
        }
        val message = try {
            ((Json.parse(body) as? Map<*, *>)?.get("error") as? Map<*, *>)?.get("message") as? String
        } catch (error: JsonException) {
            null
        }
        return message ?: body.trim().take(MAX_ERROR_CHARS)
    }

    private fun unreachable(cause: Throwable): ApiException {
        val where = if (hub.throughOverlay) {
            "through the Transferd overlay. Check that the desktop's overlay node is running and that this phone is paired with it"
        } else {
            "at ${hub.url}. Check that it is running and that this device can reach it (Tailscale connected?)"
        }
        return ApiException(503, "desktop_unreachable", "Could not reach '${hub.name}' $where. ${cause.message.orEmpty()}".trim())
    }

    private fun failure(status: Int, detail: String): ApiException = when (status) {
        401, 403 -> ApiException(
            502,
            "desktop_key_refused",
            "'${hub.name}' refused the key. Make a new peer key on that machine and save it here.",
        )
        else -> ApiException(502, "desktop_error", "'${hub.name}' answered $status: $detail".trim())
    }

    private fun failure(detail: String): ApiException =
        ApiException(502, "desktop_error", "'${hub.name}' $detail")

    private companion object {
        const val CONNECT_TIMEOUT_MS = 5_000
        const val READ_TIMEOUT_MS = 10_000
        const val CHAT_READ_TIMEOUT_MS = 300_000
        const val MAX_ERROR_CHARS = 200
    }
}

/** A reply from a paired desktop. It holds no state of its own; each reply opens a connection. */
class DesktopBackend(private val client: DesktopClient, private val model: String) : ChatBackend {
    override fun complete(messages: List<ChatMessage>, options: GenerationOptions, sink: TokenSink): GenerationStats =
        client.chat(model, messages, options, sink)
}
