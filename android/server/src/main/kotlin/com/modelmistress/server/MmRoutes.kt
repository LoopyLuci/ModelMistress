package com.modelmistress.server

import com.modelmistress.core.agent.AgentService
import com.modelmistress.core.agent.Approvals
import com.modelmistress.core.chat.ApiException
import com.modelmistress.core.chat.ChatStore
import com.modelmistress.core.environment.EnvironmentStore
import com.modelmistress.core.json.Json
import com.modelmistress.core.json.JsonException
import com.modelmistress.core.misty.FeedbackLabel
import com.modelmistress.core.misty.MistyFeedback
import com.modelmistress.core.misty.MistyTraining
import com.modelmistress.core.mm.MmDevice
import com.modelmistress.core.mm.MmDownload
import com.modelmistress.core.mm.MmHost
import com.modelmistress.core.mm.MmModel
import com.modelmistress.core.mm.MmSettings
import com.modelmistress.core.text.Base64
import com.modelmistress.core.tools.Workspace

/** What the `/mm/v1` routes use from the device. The runtime builds it once and the server keeps it for its lifetime. */
class RouterServices(
    val agent: AgentService,
    val host: MmHost,
    val approvals: Approvals,
    val chats: ChatStore,
    val environments: EnvironmentStore,
    val feedback: MistyFeedback,
    val version: String,
    /** True when the router has image models, so `/v1/images/generations` answers. */
    val images: Boolean,
)

/**
 * The router's own API under `/mm/v1`: what the phone can do beyond the OpenAI routes. Reads are answered at once. Anything that
 * changes the device (a model removed or made default, a download, a sandbox file, a chat removed, the training export, the
 * benchmark) first asks the owner through [RouterServices.approvals], and is refused when the owner does not allow it.
 */
internal class MmRoutes(private val services: RouterServices, private val nowSeconds: () -> Long) {

    fun handle(request: HttpRequest, path: String, query: Map<String, String>, writer: HttpResponseWriter) {
        val segments = path.removePrefix(PREFIX).split('/').filter { it.isNotEmpty() }.map(::decodeSegment)
        val host = services.host
        when {
            segments == listOf("capabilities") -> dispatch(request, "GET" to { writer.json(200, Json.write(capabilities())) })
            segments == listOf("settings") -> dispatch(request, "GET" to { writer.json(200, Json.write(settingsJson(host.settings()))) })
            segments == listOf("device") -> dispatch(request, "GET" to { writer.json(200, Json.write(deviceJson(host.device()))) })
            segments == listOf("approvals") -> dispatch(
                request,
                "GET" to {
                    val waiting = services.approvals.pending().map { approval ->
                        mapOf("client" to approval.client, "title" to approval.title, "detail" to approval.detail, "asked_at" to approval.askedAt)
                    }
                    writer.json(200, Json.write(mapOf("pending" to waiting)))
                },
            )

            segments == listOf("models") -> dispatch(
                request,
                "GET" to { writer.json(200, Json.write(mapOf("data" to host.models().map(::modelJson)))) },
            )
            segments == listOf("models", "default") -> dispatch(
                request,
                "POST" to {
                    val id = string(body(request), "id")
                    ownerMust(request, "Make $id the default model?", "New chats and requests without a model name use it.")
                    if (!host.setDefault(id)) throw ApiException(404, "model_not_found", "The model '$id' is not installed on this device.")
                    writer.json(200, Json.write(mapOf("ok" to true, "default" to id)))
                },
            )
            segments.size == 2 && segments[0] == "models" -> dispatch(
                request,
                "DELETE" to {
                    val id = segments[1]
                    ownerMust(request, "Delete the model $id from this device?", "The file is removed. It can be downloaded again.")
                    if (!host.deleteModel(id)) throw ApiException(404, "model_not_found", "The model '$id' is not installed on this device.")
                    writer.json(200, Json.write(mapOf("ok" to true, "deleted" to id)))
                },
            )

            segments == listOf("hf", "files") -> dispatch(
                request,
                "GET" to {
                    val repo = query["repo"]?.takeIf { it.isNotBlank() }
                        ?: throw ApiException(400, "invalid_request", "Give the repository as ?repo=owner/name.")
                    val files = host.repoFiles(repo)
                    writer.json(200, Json.write(mapOf("repo" to repo, "files" to files)))
                },
            )

            segments == listOf("downloads") -> dispatch(
                request,
                "GET" to { writer.json(200, Json.write(mapOf("data" to host.downloads().map(::downloadJson)))) },
                "POST" to {
                    val fields = body(request)
                    val repo = string(fields, "repo")
                    val file = string(fields, "file")
                    ownerMust(request, "Download $file from $repo?", "The model file is saved to this device's model library.")
                    val started = try {
                        host.startDownload(repo, file)
                    } catch (busy: IllegalStateException) {
                        throw ApiException(409, "download_busy", busy.message ?: "A download is already running.")
                    }
                    writer.json(202, Json.write(downloadJson(started)))
                },
            )
            segments == listOf("downloads", "stop") -> dispatch(
                request,
                "POST" to {
                    host.stopDownloads()
                    writer.json(200, Json.write(mapOf("ok" to true)))
                },
            )

            segments == listOf("chats") -> dispatch(
                request,
                "GET" to {
                    val chats = services.chats.list().map { chat ->
                        mapOf(
                            "id" to chat.id,
                            "title" to chat.title,
                            "model" to chat.modelId,
                            "updated_at" to chat.updatedAt,
                            "messages" to chat.messageCount,
                            "sandbox" to chat.environmentId,
                        )
                    }
                    writer.json(200, Json.write(mapOf("data" to chats)))
                },
            )
            segments.size == 2 && segments[0] == "chats" -> dispatch(
                request,
                "GET" to {
                    val chat = services.chats.load(segments[1])
                        ?: throw ApiException(404, "chat_not_found", "There is no chat '${segments[1]}' on this device.")
                    val messages = chat.messages.map { mapOf("role" to it.role.wire, "content" to it.promptText()) }
                    writer.json(
                        200,
                        Json.write(
                            mapOf(
                                "id" to chat.id,
                                "title" to chat.title,
                                "model" to chat.modelId,
                                "sandbox" to chat.environmentId,
                                "messages" to messages,
                            ),
                        ),
                    )
                },
                "DELETE" to {
                    val id = segments[1]
                    ownerMust(request, "Delete the chat $id?", "Its messages are removed from this device.")
                    if (!services.chats.delete(id)) throw ApiException(404, "chat_not_found", "There is no chat '$id' on this device.")
                    writer.json(200, Json.write(mapOf("ok" to true, "deleted" to id)))
                },
            )

            segments == listOf("sandboxes") -> dispatch(
                request,
                "GET" to {
                    val sandboxes = services.environments.list().map { env ->
                        mapOf("id" to env.id, "name" to env.name, "created_at" to env.createdAt, "links" to env.links)
                    }
                    writer.json(200, Json.write(mapOf("data" to sandboxes)))
                },
                "POST" to {
                    val name = string(body(request), "name")
                    ownerMust(request, "Make a sandbox called $name?", "A folder on this device that chats' tools can use.")
                    val made = services.environments.create(name)
                    writer.json(201, Json.write(mapOf("id" to made.id, "name" to made.name)))
                },
            )
            segments.size == 3 && segments[0] == "sandboxes" && segments[2] == "files" -> dispatch(
                request,
                "GET" to {
                    val workspace = workspaceOf(segments[1])
                    val files = workspace.files().filter { it.isFile }.map { file ->
                        mapOf("name" to file.name, "bytes" to file.length())
                    }
                    writer.json(200, Json.write(mapOf("data" to files)))
                },
                "POST" to {
                    val workspace = workspaceOf(segments[1])
                    val fields = body(request)
                    val name = string(fields, "name")
                    val content = decodeContent(string(fields, "content_base64"))
                    if (content.size > MAX_UPLOAD_BYTES) {
                        throw ApiException(413, "file_too_large", "A file uploaded here may be at most 1 MB.")
                    }
                    ownerMust(request, "Save $name in sandbox ${segments[1]}?", "${content.size} bytes of text.")
                    workspace.store(name, content)
                    writer.json(201, Json.write(mapOf("name" to name, "bytes" to content.size)))
                },
            )
            segments.size == 4 && segments[0] == "sandboxes" && segments[2] == "files" -> dispatch(
                request,
                "DELETE" to {
                    val workspace = workspaceOf(segments[1])
                    val name = segments[3]
                    ownerMust(request, "Delete $name from sandbox ${segments[1]}?", "The file is removed.")
                    if (!workspace.remove(name)) throw ApiException(404, "file_not_found", "There is no file '$name' in that sandbox.")
                    writer.json(200, Json.write(mapOf("ok" to true, "deleted" to name)))
                },
            )

            segments == listOf("feedback") -> dispatch(
                request,
                "POST" to {
                    val fields = body(request)
                    val chat = string(fields, "chat")
                    val label = FeedbackLabel.of(string(fields, "label"))
                        ?: throw ApiException(400, "invalid_request", "label must be good, bad, no_tools, needs_tools or corrected.")
                    val tools = (fields["tools"] as? List<*>).orEmpty().mapNotNull { it as? String }
                    val correction = fields["correction"] as? String
                    if (!services.agent.rate(chat, label, tools, correction)) {
                        throw ApiException(404, "no_reply", "There is no reply in chat '$chat' to rate.")
                    }
                    writer.json(200, Json.write(mapOf("ok" to true)))
                },
                "DELETE" to {
                    ownerMust(request, "Forget every rating of Misty's replies?", "Routing learned from them is lost.")
                    services.feedback.clear()
                    writer.json(200, Json.write(mapOf("ok" to true)))
                },
            )
            segments == listOf("training") -> dispatch(
                request,
                "GET" to {
                    ownerMust(request, "Export Misty's training examples?", "They contain your chats' questions and answers.")
                    val lines = MistyTraining.lines(services.feedback.examples()).joinToString("\n", postfix = "\n")
                    writer.send(200, "application/x-ndjson", lines.toByteArray(Charsets.UTF_8))
                },
            )

            segments == listOf("benchmark") -> dispatch(
                request,
                "GET" to {
                    val report = host.benchmarkReport()
                        ?: throw ApiException(404, "no_benchmark", "No benchmark has run on this device yet.")
                    writer.json(200, report)
                },
                "POST" to {
                    ownerMust(request, "Run the device benchmark?", "It keeps the phone busy for several minutes.")
                    if (!host.startBenchmark()) throw ApiException(409, "benchmark_busy", "The benchmark is already running.")
                    writer.json(202, Json.write(mapOf("started" to true)))
                },
            )

            else -> throw ApiException(404, "route_not_found", "There is no route ${request.path} on this server.")
        }
    }

    private fun capabilities(): Map<String, Any?> {
        val settings = services.host.settings()
        return linkedMapOf(
            "api" to "mm/1",
            "version" to services.version,
            "chat" to linkedMapOf(
                "streaming" to true,
                "history" to true,
                "sandboxes" to true,
                "misty_model" to MISTY_MODEL,
                "tools" to linkedMapOf(
                    "owner_tool_use" to settings.toolUse,
                    "owner_file_writes" to settings.fileWrites,
                    "names" to TOOL_NAMES,
                    "default_for_plain_requests" to false,
                    "default_for_chats_and_misty" to true,
                ),
            ),
            "misty" to linkedMapOf(
                "enabled" to settings.mistyEnabled,
                "reflex" to settings.mistyReflex,
                "ratings" to true,
                "training_export" to true,
            ),
            "images" to linkedMapOf("available" to services.images, "endpoint" to "/v1/images/generations"),
            "models" to linkedMapOf("list" to true, "default" to true, "delete" to true, "download" to true, "desktops" to true),
            "benchmark" to true,
            "attachments" to linkedMapOf("text" to true, "image" to false, "document" to false),
            "approvals" to linkedMapOf(
                "timeout_seconds" to Approvals.DEFAULT_TIMEOUT_MILLIS / 1000,
                "pending" to "/mm/v1/approvals",
                "note" to "Changes to this device wait for the owner. The owner's refusal, or no answer in time, is a 403.",
            ),
            "not_available_over_api" to listOf("speech", "live", "camera", "steer", "queue"),
        )
    }

    private fun settingsJson(settings: MmSettings): Map<String, Any?> = linkedMapOf(
        "tool_use" to settings.toolUse,
        "file_writes" to settings.fileWrites,
        "misty_enabled" to settings.mistyEnabled,
        "misty_reflex" to settings.mistyReflex,
        "default_model" to settings.defaultModel,
        "allow_network" to settings.allowNetwork,
        "port" to settings.port,
    )

    private fun deviceJson(device: MmDevice): Map<String, Any?> = linkedMapOf(
        "total_memory_bytes" to device.totalMemoryBytes,
        "free_memory_bytes" to device.freeMemoryBytes,
        "needle_ready" to device.needleReady,
        "loaded_model" to device.loadedModel,
    )

    private fun modelJson(model: MmModel): Map<String, Any?> = linkedMapOf(
        "id" to model.id,
        "kind" to model.kind,
        "size_bytes" to model.sizeBytes,
        "context_tokens" to model.contextTokens,
        "default" to model.isDefault,
        "description" to model.description,
    )

    private fun downloadJson(download: MmDownload): Map<String, Any?> = linkedMapOf(
        "id" to download.id,
        "repo" to download.repo,
        "file" to download.file,
        "phase" to download.phase,
        "done" to download.done,
        "total" to download.total,
        "message" to download.message,
    )

    private fun workspaceOf(id: String): Workspace =
        services.environments.workspace(id) ?: throw ApiException(404, "sandbox_not_found", "There is no sandbox '$id'.")

    /** Asks the owner. The request is refused unless the owner allows it. */
    private fun ownerMust(request: HttpRequest, title: String, detail: String) {
        val client = request.header(CLIENT_HEADER).orEmpty()
        if (!services.approvals.ask(client, title, detail)) {
            throw ApiException(403, "owner_declined", "The owner did not allow this on the device, or did not answer in time.")
        }
    }

    private fun dispatch(request: HttpRequest, vararg handlers: Pair<String, () -> Unit>) {
        val match = handlers.firstOrNull { it.first == request.method }
            ?: throw ApiException(
                405,
                "method_not_allowed",
                "Use ${handlers.joinToString(" or ") { it.first }} for ${request.path.substringBefore('?')}.",
            )
        match.second()
    }

    private fun body(request: HttpRequest): Map<*, *> {
        val value = try {
            Json.parse(String(request.body, Charsets.UTF_8))
        } catch (error: JsonException) {
            throw ApiException(400, "invalid_json", "The request body is not valid JSON. ${error.message}")
        }
        return value as? Map<*, *> ?: throw ApiException(400, "invalid_request", "The request body must be a JSON object.")
    }

    private fun string(fields: Map<*, *>, key: String): String {
        val value = fields[key] as? String
        if (value.isNullOrBlank()) throw ApiException(400, "invalid_request", "$key is required.")
        return value
    }

    private companion object {
        const val PREFIX = "/mm/v1"
        const val CLIENT_HEADER = "x-mm-client"
        const val MISTY_MODEL = "misty"
        const val MAX_UPLOAD_BYTES = 1024 * 1024

        /** Every tool a chat can be offered: the device's own, the file tools, and Misty's two. */
        val TOOL_NAMES = listOf(
            "datetime", "calc", "fs_list", "fs_read", "fs_search", "fs_write", "fs_edit", "device_status", "models_list",
        )
    }
}

/** Percent-decodes one path segment. A plus sign stays a plus sign in a path. */
internal fun decodeSegment(segment: String): String = decodeEscapes(segment.replace("+", "%2B"))

/** Parses `a=1&b=two` into a map, percent-decoded. A key without a value maps to an empty string. */
internal fun parseQuery(query: String): Map<String, String> {
    if (query.isEmpty()) return emptyMap()
    return query.split('&').filter { it.isNotEmpty() }.associate { pair ->
        decodeEscapes(pair.substringBefore('=')) to decodeEscapes(pair.substringAfter('=', ""))
    }
}

private fun decodeEscapes(text: String): String = try {
    java.net.URLDecoder.decode(text, Charsets.UTF_8.name())
} catch (error: IllegalArgumentException) {
    throw ApiException(400, "invalid_request", "The address has an invalid escape.")
}

/** Decodes the file bytes an upload sends as Base64. A bad encoding is the caller's error. */
private fun decodeContent(text: String): ByteArray = try {
    Base64.decode(text)
} catch (error: IllegalArgumentException) {
    throw ApiException(400, "invalid_request", "content_base64 is not valid Base64.")
}
