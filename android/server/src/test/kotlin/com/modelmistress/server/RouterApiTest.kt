package com.modelmistress.server

import com.modelmistress.core.agent.AgentService
import com.modelmistress.core.agent.AgentSettings
import com.modelmistress.core.agent.Approvals
import com.modelmistress.core.chat.ChatBackend
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.FileChatStore
import com.modelmistress.core.chat.FinishReason
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.GenerationStats
import com.modelmistress.core.chat.ModelCatalog
import com.modelmistress.core.chat.ModelInfo
import com.modelmistress.core.chat.ModelRouter
import com.modelmistress.core.chat.TokenSink
import com.modelmistress.core.environment.EnvironmentStore
import com.modelmistress.core.json.Json
import com.modelmistress.core.misty.FileMistyFeedback
import com.modelmistress.core.mm.MmDevice
import com.modelmistress.core.mm.MmDownload
import com.modelmistress.core.mm.MmHost
import com.modelmistress.core.mm.MmModel
import com.modelmistress.core.mm.MmSettings
import com.modelmistress.core.tools.FileWrites
import com.modelmistress.core.tools.ToolUse
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class RouterApiTest {
    @get:Rule
    val folder = TemporaryFolder()

    /** Replies to each request in turn with the next script, or echoes the last message when the scripts run out. */
    private var scripts = mutableListOf<String>()
    private val backend = object : ChatBackend {
        override fun complete(messages: List<ChatMessage>, options: GenerationOptions, sink: TokenSink): GenerationStats {
            val reply = if (scripts.isNotEmpty()) scripts.removeAt(0) else "Echo: ${messages.last().content}"
            for (piece in reply.chunked(9)) if (!sink.onText(piece)) break
            return GenerationStats(0, 0, FinishReason.STOP)
        }
    }

    private val catalog = object : ModelCatalog {
        override fun models(): List<ModelInfo> = listOf(ModelInfo("tiny", 2048, 1000L, "a small model"))
        override fun defaultModelId(): String? = "tiny"
        override fun backend(modelId: String): ChatBackend? = if (modelId == "tiny") backend else null
    }

    private val host = FakeHost()
    private var mistyOn = true
    private var approvals = Approvals(timeoutMillis = 300)
    private lateinit var server: ModelMistressServer
    private var owner: AutoOwner? = null

    @Before
    fun start() {
        val root = folder.root
        val services = RouterServices(
            agent = AgentService(
                chats = FileChatStore(File(root, "chats").apply { mkdirs() }),
                environments = EnvironmentStore(root),
                feedback = FileMistyFeedback(File(root, "feedback.jsonl")),
                approvals = approvals,
                settings = { AgentSettings(ToolUse.ON, FileWrites.ON, mistyOn) },
                reflex = { null },
                deviceStatus = { "Free memory: 1 GB" },
                installedModels = { "tiny" },
            ),
            host = host,
            approvals = approvals,
            chats = FileChatStore(File(root, "chats")),
            environments = EnvironmentStore(root),
            feedback = FileMistyFeedback(File(root, "feedback.jsonl")),
            version = "test",
            images = false,
        )
        server = ModelMistressServer(ModelRouter(catalog), ServerConfig(port = 0, apiKey = KEY), services = services)
        server.start()
    }

    @After
    fun stop() {
        owner?.stop()
        server.close()
    }

    /** Answers every approval request with [allow], as an owner who keeps tapping the same answer would. */
    private fun ownerAnswers(allow: Boolean) {
        owner = AutoOwner(approvals, allow).also { it.start() }
    }

    @Test
    fun capabilitiesListTheFeaturesAndTheOwnersToolSettings() {
        val (status, body) = call("GET", "/mm/v1/capabilities")
        assertEquals(200, status)
        val root = Json.parse(body) as Map<*, *>
        assertEquals("mm/1", root["api"])
        val chat = root["chat"] as Map<*, *>
        val tools = chat["tools"] as Map<*, *>
        assertEquals("ON", tools["owner_tool_use"])
        assertTrue((tools["names"] as List<*>).contains("calc"))
        assertTrue((tools["names"] as List<*>).contains("fs_write"))
        assertEquals(true, (root["misty"] as Map<*, *>)["enabled"])
        assertEquals(true, (root["models"] as Map<*, *>)["delete"])
    }

    @Test
    fun theRouterApiNeedsTheKeyAndTheSettingsNeverShowIt() {
        assertEquals(401, call("GET", "/mm/v1/capabilities", key = null).first)
        val (status, body) = call("GET", "/mm/v1/settings")
        assertEquals(200, status)
        assertFalse(body.contains(KEY))
        assertEquals("ON", (Json.parse(body) as Map<*, *>)["tool_use"])
    }

    @Test
    fun theModelListShowsTheDefault() {
        val (_, body) = call("GET", "/mm/v1/models")
        val data = (Json.parse(body) as Map<*, *>)["data"] as List<*>
        assertEquals(true, (data.single() as Map<*, *>)["default"])
    }

    @Test
    fun aPlainRequestIsAnEchoWithNoToolsAndNoMisty() {
        val (status, body) = call("POST", "/v1/chat/completions", CHAT_BODY)
        assertEquals(200, status)
        assertTrue(body.contains("Echo: hi"))
        assertTrue(body.contains("\"misty\":false"))
        assertTrue(body.contains("\"offered\":[]"))
    }

    @Test
    fun mistyAnswersWhenAskedAndSaysSo() {
        val (status, body) = call("POST", "/v1/chat/completions", """{"model":"misty","messages":[{"role":"user","content":"hi"}]}""")
        assertEquals(200, status)
        assertTrue(body.contains("\"misty\":true"))
    }

    @Test
    fun mistyRefusesWhileTheOwnerHasHerOff() {
        mistyOn = false
        val (status, body) = call("POST", "/v1/chat/completions", """{"model":"misty","messages":[{"role":"user","content":"hi"}]}""")
        assertEquals(409, status)
        assertTrue(body.contains("misty_disabled"))
    }

    @Test
    fun aChatStreamsItsToolUseAsMmEventsAndSavesTheReply() {
        scripts += listOf(
            "Let me work it out. <tool>{\"name\": \"calc\", \"arguments\": {\"expression\": \"17*23\"}}</tool>",
            "It is 391.",
        )
        val body = """{"stream":true,"model":"tiny","mm":{"chat":"chat-a"},
            "messages":[{"role":"user","content":"calculate 17 times 23"}]}"""
        val (status, text) = call("POST", "/v1/chat/completions", body)
        assertEquals(200, status)
        val events = text.lines().filter { it.startsWith("data: ") }.map { it.removePrefix("data: ") }
        assertEquals("[DONE]", events.last())
        val payloads = events.dropLast(1).map { Json.parse(it) as Map<*, *> }
        val deltas = payloads.map { ((it["choices"] as List<*>)[0] as Map<*, *>)["delta"] as Map<*, *> }
        val content = deltas.mapNotNull { it["content"] as String? }.joinToString("")
        assertTrue(content.contains("It is 391."))
        val mmTypes = deltas.mapNotNull { (it["mm_event"] as Map<*, *>?)?.get("type") as String? }
        assertTrue(mmTypes.contains("tool_started"))
        assertTrue(mmTypes.contains("tool_finished"))

        val (_, chat) = call("GET", "/mm/v1/chats/chat-a")
        assertTrue(chat.contains("It is 391."))
    }

    @Test
    fun aDeleteWaitsForTheOwnerAndIsRefusedWithoutAnAnswer() {
        val (status, body) = call("DELETE", "/mm/v1/models/tiny")
        assertEquals(403, status)
        assertTrue(body.contains("owner_declined"))
        assertEquals(listOf("tiny"), host.installed)
    }

    @Test
    fun aDeleteTheOwnerAllowsRemovesTheModel() {
        ownerAnswers(allow = true)
        val (status, _) = call("DELETE", "/mm/v1/models/tiny")
        assertEquals(200, status)
        assertTrue(host.installed.isEmpty())
    }

    @Test
    fun aDeleteTheOwnerRefusesLeavesTheModel() {
        ownerAnswers(allow = false)
        assertEquals(403, call("DELETE", "/mm/v1/models/tiny").first)
        assertEquals(listOf("tiny"), host.installed)
    }

    @Test
    fun anUnknownModelIsNotFoundOnceTheOwnerAllows() {
        ownerAnswers(allow = true)
        assertEquals(404, call("DELETE", "/mm/v1/models/nope").first)
    }

    @Test
    fun aDownloadAsksTheOwnerThenStarts() {
        ownerAnswers(allow = true)
        val (status, body) = call("POST", "/mm/v1/downloads", """{"repo":"owner/model","file":"model-Q4.gguf"}""")
        assertEquals(202, status)
        assertTrue(body.contains("model-Q4.gguf"))
        assertEquals(1, host.startedDownloads)
    }

    @Test
    fun aSecondDownloadWhileOneRunsIsRefused() {
        ownerAnswers(allow = true)
        call("POST", "/mm/v1/downloads", """{"repo":"owner/model","file":"a.gguf"}""")
        val (status, body) = call("POST", "/mm/v1/downloads", """{"repo":"owner/model","file":"b.gguf"}""")
        assertEquals(409, status)
        assertTrue(body.contains("download_busy"))
    }

    @Test
    fun repoFilesNeedARepoName() {
        assertEquals(400, call("GET", "/mm/v1/hf/files").first)
        val (status, body) = call("GET", "/mm/v1/hf/files?repo=owner%2Fmodel")
        assertEquals(200, status)
        assertTrue(body.contains("model-Q4.gguf"))
    }

    @Test
    fun aSandboxFileIsMadeReadAndRemovedWithTheOwnersOk() {
        ownerAnswers(allow = true)
        assertEquals(201, call("POST", "/mm/v1/sandboxes", """{"name":"Notes"}""").first)
        val sandbox = (Json.parse(call("GET", "/mm/v1/sandboxes").second) as Map<*, *>)["data"] as List<*>
        val id = (sandbox.last() as Map<*, *>)["id"] as String

        assertEquals(201, call("POST", "/mm/v1/sandboxes/$id/files", """{"name":"a.txt","content_base64":"aGVsbG8="}""").first)
        val listed = (Json.parse(call("GET", "/mm/v1/sandboxes/$id/files").second) as Map<*, *>)["data"] as List<*>
        assertEquals(5L, (listed.single() as Map<*, *>)["bytes"])

        assertEquals(200, call("DELETE", "/mm/v1/sandboxes/$id/files/a.txt").first)
        assertEquals(404, call("DELETE", "/mm/v1/sandboxes/$id/files/a.txt").first)
    }

    @Test
    fun aSandboxFileWithBadBase64IsRefused() {
        ownerAnswers(allow = true)
        val sandboxes = (Json.parse(call("GET", "/mm/v1/sandboxes").second) as Map<*, *>)["data"] as List<*>
        val id = (sandboxes.first() as Map<*, *>)["id"] as String
        val (status, _) = call("POST", "/mm/v1/sandboxes/$id/files", """{"name":"a.txt","content_base64":"!!!"}""")
        assertEquals(400, status)
    }

    @Test
    fun aRatingNeedsAReplyInTheChat() {
        call("POST", "/v1/chat/completions", """{"model":"tiny","mm":{"chat":"chat-r"},"messages":[{"role":"user","content":"hi"}]}""")
        assertEquals(200, call("POST", "/mm/v1/feedback", """{"chat":"chat-r","label":"good"}""").first)
        assertEquals(404, call("POST", "/mm/v1/feedback", """{"chat":"no-chat","label":"good"}""").first)
        assertEquals(400, call("POST", "/mm/v1/feedback", """{"chat":"chat-r","label":"meh"}""").first)
    }

    @Test
    fun theTrainingExportAsksTheOwnerFirst() {
        ownerAnswers(allow = true)
        val (status, _) = call("GET", "/mm/v1/training")
        assertEquals(200, status)
    }

    @Test
    fun theBenchmarkIsReadOnceRunAndRefusedWhileRunning() {
        assertEquals(404, call("GET", "/mm/v1/benchmark").first)
        ownerAnswers(allow = true)
        assertEquals(202, call("POST", "/mm/v1/benchmark").first)
        assertEquals(409, call("POST", "/mm/v1/benchmark").first)
    }

    @Test
    fun anUnknownRouterRouteIsNotFound() {
        assertEquals(404, call("GET", "/mm/v1/nothing-here").first)
        assertEquals(405, call("POST", "/mm/v1/capabilities").first)
    }

    private fun call(
        method: String,
        path: String,
        body: String? = null,
        key: String? = KEY,
    ): Pair<Int, String> {
        val connection = URL("http://127.0.0.1:${server.port}$path").openConnection() as HttpURLConnection
        connection.requestMethod = method
        connection.connectTimeout = 5_000
        connection.readTimeout = 10_000
        if (key != null) connection.setRequestProperty("Authorization", "Bearer $key")
        connection.setRequestProperty("X-MM-Client", "test-app")
        if (body != null) {
            connection.doOutput = true
            connection.setRequestProperty("Content-Type", "application/json")
            connection.outputStream.use { it.write(body.toByteArray(Charsets.UTF_8)) }
        }
        val status = connection.responseCode
        val stream = if (status >= 400) connection.errorStream else connection.inputStream
        val text = stream.bufferedReader(Charsets.UTF_8).use { it.readText() }
        return status to text
    }

    /** Answers approval requests with one answer until stopped. */
    private class AutoOwner(private val approvals: Approvals, private val allow: Boolean) {
        @Volatile
        private var running = true
        private val thread = Thread {
            while (running) {
                approvals.pending().forEach { approvals.answer(it.id, allow) }
                Thread.sleep(5)
            }
        }.apply { isDaemon = true }

        fun start() = thread.start()
        fun stop() {
            running = false
        }
    }

    private class FakeHost : MmHost {
        private var defaultId: String? = "tiny"
        val installed = mutableListOf("tiny")
        var startedDownloads = 0
        private var benchmarkRunning = false
        private var benchmarkDone = false

        override fun settings() = MmSettings("ON", "ON", true, false, defaultId, false, 8080)
        override fun device() = MmDevice(8L shl 30, 4L shl 30, needleReady = false, loadedModel = null)
        override fun models(): List<MmModel> = installed.map { MmModel(it, "local", 1000, 2048, it == defaultId, "a small model") }
        override fun setDefault(id: String): Boolean = (id in installed).also { if (it) defaultId = id }
        override fun deleteModel(id: String): Boolean = installed.remove(id)
        override fun repoFiles(repo: String): List<String> {
            require(repo.contains('/')) { "The repository must be owner/name." }
            return listOf("model-Q4.gguf")
        }

        override fun startDownload(repo: String, file: String): MmDownload {
            check(startedDownloads == 0) { "A download is already running." }
            startedDownloads++
            return MmDownload("d1", repo, file, "downloading", 0, 0, null)
        }

        override fun downloads(): List<MmDownload> = emptyList()
        override fun stopDownloads() {}
        override fun benchmarkReport(): String? = if (benchmarkDone) "{\"cpu\":1}" else null
        override fun startBenchmark(): Boolean {
            if (benchmarkRunning) return false
            benchmarkRunning = true
            benchmarkDone = true
            return true
        }
    }

    private companion object {
        const val KEY = "test-key"
        const val CHAT_BODY = """{"messages":[{"role":"user","content":"hi"}]}"""
    }
}
