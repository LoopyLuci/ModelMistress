package com.modelmistress.server

import com.modelmistress.core.agent.AgentEvent
import com.modelmistress.core.agent.AgentRequest
import com.modelmistress.core.agent.AgentService
import com.modelmistress.core.chat.ApiException
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.ChatRequest
import com.modelmistress.core.chat.FinishReason
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.GenerationStats
import com.modelmistress.core.chat.ModelRouter
import com.modelmistress.core.chat.OpenAi
import com.modelmistress.core.images.ImageApi
import com.modelmistress.core.images.ImageCatalog
import com.modelmistress.core.json.Json
import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.io.Closeable
import java.io.IOException
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.net.SocketTimeoutException
import java.security.MessageDigest
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.RejectedExecutionException

data class ServerConfig(
    val host: String = "127.0.0.1",
    val port: Int = 8080,
    val apiKey: String? = null,
    val version: String = "0.1.0",
    val maxBodyBytes: Int = 4 * 1024 * 1024,
)

/**
 * The OpenAI-compatible HTTP server. It binds [ServerConfig.host] and answers /v1/health, /v1/models and
 * /v1/chat/completions, streamed as server-sent events when the request asks for it, and /v1/images/generations when
 * [images] is given. When [ServerConfig.apiKey] is set,
 * every route except health needs it as a bearer token. Cross-origin browser access is deliberately not enabled, so a
 * web page cannot read replies from the device.
 */
class ModelMistressServer(
    private val router: ModelRouter,
    private val config: ServerConfig,
    private val images: ImageCatalog? = null,
    /**
     * The router's own features: the agent that answers chats and Misty, and the `/mm/v1` routes. Without them the server
     * answers the OpenAI routes only, as a plain chat.
     */
    private val services: RouterServices? = null,
    private val log: (String) -> Unit = {},
) : Closeable {
    private var listener: ServerSocket? = null
    private var workers: ExecutorService? = null
    private val mmRoutes = services?.let { MmRoutes(it, ::nowSeconds) }

    /** The port the server listens on; zero until [start]. Pass port 0 to let the system choose one. */
    @Volatile
    var port: Int = 0
        private set

    val isRunning: Boolean
        get() = listener?.isClosed == false

    @Synchronized
    fun start(): Int {
        check(!isRunning) { "The server is already running." }
        val socket = ServerSocket()
        try {
            socket.reuseAddress = true
            socket.bind(InetSocketAddress(InetAddress.getByName(config.host), config.port), BACKLOG)
        } catch (error: IOException) {
            socket.close()
            throw error
        }
        val pool = Executors.newCachedThreadPool { task -> Thread(task, "mm-http").apply { isDaemon = true } }
        listener = socket
        workers = pool
        port = socket.localPort
        Thread({ acceptLoop(socket, pool) }, "mm-accept").apply {
            isDaemon = true
            start()
        }
        log("Listening on ${config.host}:$port")
        return port
    }

    @Synchronized
    override fun close() {
        listener?.close()
        listener = null
        workers?.shutdownNow()
        workers = null
    }

    private fun acceptLoop(server: ServerSocket, pool: ExecutorService) {
        while (!server.isClosed) {
            val client = try {
                server.accept()
            } catch (error: IOException) {
                break
            }
            try {
                pool.execute { handle(client) }
            } catch (error: RejectedExecutionException) {
                client.close()
            }
        }
    }

    private fun handle(client: Socket) {
        client.use { socket ->
            socket.soTimeout = READ_TIMEOUT_MS
            val output = BufferedOutputStream(socket.getOutputStream())
            val writer = HttpResponseWriter(output)
            try {
                val request = HttpReader.read(BufferedInputStream(socket.getInputStream()), config.maxBodyBytes)
                route(request, writer)
            } catch (error: HttpException) {
                fail(writer, error.status, "invalid_request", error.message.orEmpty())
            } catch (error: ApiException) {
                fail(writer, error.status, error.code, error.message.orEmpty())
            } catch (error: SocketTimeoutException) {
                // A client that stopped sending gets no reply.
            } catch (error: IOException) {
                // The client went away; there is no one left to answer.
            } catch (error: IllegalArgumentException) {
                fail(writer, 400, "invalid_request", error.message.orEmpty())
            } catch (error: RuntimeException) {
                log("Request failed: ${error.message}")
                fail(writer, 500, "internal_error", "The request failed on this device.")
            }
            try {
                output.flush()
            } catch (error: IOException) {
                // Already closed by the client.
            }
        }
    }

    private fun route(request: HttpRequest, writer: HttpResponseWriter) {
        val path = request.path
        if (path == MM_PREFIX || path.startsWith("$MM_PREFIX/")) {
            val routes = mmRoutes ?: throw ApiException(404, "route_not_found", "This server has no router API.")
            authorize(request)
            routes.handle(request, path, parseQuery(request.query), writer)
            return
        }
        when (path) {
            "/v1/health", "/health" -> {
                requireMethod(request, "GET")
                val body = Json.write(
                    mapOf("ok" to true, "version" to config.version, "models" to router.models().size),
                )
                writer.json(200, body)
            }
            "/v1/models", "/models" -> {
                requireMethod(request, "GET")
                authorize(request)
                writer.json(200, OpenAi.models(router.models(), nowSeconds()))
            }
            "/v1/chat/completions", "/chat/completions" -> {
                requireMethod(request, "POST")
                authorize(request)
                chat(request, writer)
            }
            "/v1/images/generations", "/images/generations" -> {
                requireMethod(request, "POST")
                authorize(request)
                imageGeneration(request, writer)
            }
            else -> throw ApiException(404, "route_not_found", "There is no route ${request.path} on this server.")
        }
    }

    /** Makes one PNG for each image asked for, one after another, and returns them base64-encoded. */
    private fun imageGeneration(request: HttpRequest, writer: HttpResponseWriter) {
        val catalog = images ?: throw ApiException(404, "route_not_found", "This server has no image models.")
        val parsed = ImageApi.parse(String(request.body, Charsets.UTF_8))
        val model = parsed.model ?: catalog.defaultModelId()
            ?: throw ApiException(404, "model_not_found", "No image model is installed on this device.")
        if (model !in catalog.models()) {
            throw ApiException(404, "model_not_found", "The image model '$model' is not installed on this device.")
        }
        val made = (0 until parsed.count).map { index -> catalog.generate(model, parsed.spec(index)) }
        writer.json(200, ImageApi.response(nowSeconds(), made))
    }

    private fun requireMethod(request: HttpRequest, expected: String) {
        if (request.method != expected) {
            throw ApiException(405, "method_not_allowed", "Use $expected for ${request.path}.")
        }
    }

    private fun authorize(request: HttpRequest) {
        val key = config.apiKey?.takeIf { it.isNotEmpty() } ?: return
        val given = request.header("authorization").orEmpty()
        val expected = "Bearer $key"
        val matches = MessageDigest.isEqual(given.toByteArray(Charsets.UTF_8), expected.toByteArray(Charsets.UTF_8))
        if (!matches) {
            throw ApiException(401, "invalid_api_key", "Send the device's API key as 'Authorization: Bearer <key>'.")
        }
    }

    private fun chat(request: HttpRequest, writer: HttpResponseWriter) {
        val parsed = OpenAi.parseChatRequest(String(request.body, Charsets.UTF_8))
        val agent = services
        if (agent != null) {
            agentChat(parsed, request, writer, agent)
            return
        }
        val route = router.resolve(parsed.model)
        val id = OpenAi.newId()
        val created = nowSeconds()
        if (parsed.stream) {
            streamReply(route, parsed.messages, parsed.options, id, created, writer)
        } else {
            val text = StringBuilder()
            val stats = route.backend.complete(parsed.messages, parsed.options) { piece ->
                text.append(piece)
                true
            }
            writer.json(200, OpenAi.completion(id, route.modelId, created, text.toString(), stats))
        }
    }

    /**
     * A chat through the router's agent. `misty` or `misty:<model>` asks for Misty; any other name is a model as usual. Tools
     * follow the owner's settings, and are offered to chats and to Misty unless `mm.tools` says otherwise.
     */
    private fun agentChat(parsed: ChatRequest, request: HttpRequest, writer: HttpResponseWriter, services: RouterServices) {
        val requested = parsed.model?.trim().orEmpty()
        val misty = requested == MISTY || requested.startsWith("$MISTY:")
        val modelName = if (misty) requested.removePrefix(MISTY).removePrefix(":").ifEmpty { null } else requested
        val route = router.resolve(modelName)
        val agentRequest = AgentRequest(
            modelId = route.modelId,
            model = route.backend,
            messages = parsed.messages,
            options = parsed.options,
            client = request.header(CLIENT_HEADER).orEmpty(),
            chatId = parsed.mm.chatId,
            sandboxId = parsed.mm.sandboxId,
            misty = misty,
            toolsAllowed = parsed.mm.toolsAllowed ?: (misty || parsed.mm.chatId != null),
        )
        services.agent.preflight(agentRequest)
        val id = OpenAi.newId()
        val created = nowSeconds()
        if (parsed.stream) {
            streamAgentReply(services.agent, agentRequest, id, created, writer)
        } else {
            val result = services.agent.run(agentRequest) { true }
            val extra = mapOf(
                "mm" to mapOf(
                    "chat" to result.chatId,
                    "misty" to result.misty,
                    "tool_calls" to result.toolCalls,
                    "offered" to result.offered,
                    "calls" to result.calls,
                ),
            )
            val stats = GenerationStats(promptTokens = 0, completionTokens = 0, finishReason = FinishReason.STOP)
            writer.json(200, OpenAi.completion(id, route.modelId, created, result.answer, stats, extra))
        }
    }

    private fun streamAgentReply(
        agent: AgentService,
        request: AgentRequest,
        id: String,
        created: Long,
        writer: HttpResponseWriter,
    ) {
        val model = request.modelId
        writer.startEventStream()
        writer.event(OpenAi.chunk(id, model, created, mapOf("role" to "assistant", "content" to ""), null))
        var clientGone = false
        try {
            agent.run(request) { event ->
                try {
                    writer.event(OpenAi.chunk(id, model, created, deltaOf(event), null))
                    true
                } catch (error: IOException) {
                    clientGone = true
                    false
                }
            }
            if (!clientGone) {
                writer.event(OpenAi.chunk(id, model, created, emptyMap(), FinishReason.STOP.wire))
            }
        } catch (error: ApiException) {
            writer.event(OpenAi.error(error.message.orEmpty(), "invalid_request_error", error.code))
        } catch (error: IllegalArgumentException) {
            writer.event(OpenAi.error(error.message.orEmpty(), "invalid_request_error", "invalid_request"))
        } catch (error: RuntimeException) {
            log("Stream failed: ${error.message}")
            writer.event(OpenAi.error("The reply failed on this device.", "server_error", "internal_error"))
        }
        if (!clientGone) writer.event("[DONE]")
    }

    /**
     * The stream's delta for one agent event. Text is `content`, as in OpenAI. Everything the router adds is under `mm_event`,
     * which a standard client ignores: tool starts and finishes, Misty's notes, and a reply the router had to replace.
     */
    private fun deltaOf(event: AgentEvent): Map<String, Any?> = when (event) {
        is AgentEvent.Text -> mapOf("content" to event.piece)
        is AgentEvent.ToolStarted -> mapOf("mm_event" to mapOf("type" to "tool_started", "name" to event.name))
        is AgentEvent.ToolFinished -> mapOf(
            "mm_event" to mapOf("type" to "tool_finished", "name" to event.name, "outcome" to event.outcome),
        )
        is AgentEvent.Note -> mapOf("mm_event" to mapOf("type" to "note", "text" to event.line))
        is AgentEvent.Replaced -> mapOf("mm_event" to mapOf("type" to "replaced", "text" to event.text))
    }

    private fun streamReply(
        route: ModelRouter.Route,
        messages: List<ChatMessage>,
        options: GenerationOptions,
        id: String,
        created: Long,
        writer: HttpResponseWriter,
    ) {
        val model = route.modelId
        writer.startEventStream()
        writer.event(OpenAi.chunk(id, model, created, mapOf("role" to "assistant", "content" to ""), null))
        var clientGone = false
        try {
            val stats = route.backend.complete(messages, options) { piece ->
                try {
                    writer.event(OpenAi.chunk(id, model, created, mapOf("content" to piece), null))
                    true
                } catch (error: IOException) {
                    clientGone = true
                    false
                }
            }
            if (!clientGone) {
                writer.event(OpenAi.chunk(id, model, created, emptyMap(), stats.finishReason.wire))
            }
        } catch (error: ApiException) {
            writer.event(OpenAi.error(error.message.orEmpty(), "invalid_request_error", error.code))
        } catch (error: IllegalArgumentException) {
            writer.event(OpenAi.error(error.message.orEmpty(), "invalid_request_error", "invalid_request"))
        } catch (error: RuntimeException) {
            log("Stream failed: ${error.message}")
            writer.event(OpenAi.error("The reply failed on this device.", "server_error", "internal_error"))
        }
        if (!clientGone) writer.event("[DONE]")
    }

    private fun fail(writer: HttpResponseWriter, status: Int, code: String, message: String) {
        if (writer.committed) return
        val type = if (status >= 500) "server_error" else "invalid_request_error"
        try {
            writer.json(status, OpenAi.error(message, type, code))
        } catch (error: IOException) {
            // The client has gone.
        }
    }

    private fun nowSeconds(): Long = System.currentTimeMillis() / 1000

    private companion object {
        const val BACKLOG = 16
        const val READ_TIMEOUT_MS = 60_000
        const val MM_PREFIX = "/mm/v1"
        const val MISTY = "misty"
        const val CLIENT_HEADER = "x-mm-client"
    }
}
