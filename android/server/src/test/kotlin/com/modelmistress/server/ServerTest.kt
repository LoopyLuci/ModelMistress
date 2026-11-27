package com.modelmistress.server

import com.modelmistress.core.chat.ChatBackend
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.FinishReason
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.GenerationStats
import com.modelmistress.core.chat.ModelCatalog
import com.modelmistress.core.chat.ModelInfo
import com.modelmistress.core.chat.ModelRouter
import com.modelmistress.core.chat.TokenSink
import com.modelmistress.core.json.Json
import java.net.HttpURLConnection
import java.net.URL
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class ServerTest {
    private val backend = object : ChatBackend {
        override fun complete(
            messages: List<ChatMessage>,
            options: GenerationOptions,
            sink: TokenSink,
        ): GenerationStats {
            for (piece in listOf("Echo: ", messages.last().content)) {
                if (!sink.onText(piece)) break
            }
            return GenerationStats(promptTokens = messages.size, completionTokens = 2, finishReason = FinishReason.STOP)
        }
    }

    private val catalog = object : ModelCatalog {
        override fun models(): List<ModelInfo> = listOf(ModelInfo("tiny", 2048, 1000L, "llama Q4_K_M"))
        override fun defaultModelId(): String? = "tiny"
        override fun backend(modelId: String): ChatBackend? = if (modelId == "tiny") backend else null
    }

    private lateinit var server: ModelMistressServer

    @Before
    fun start() {
        server = ModelMistressServer(ModelRouter(catalog), ServerConfig(port = 0, apiKey = KEY))
        server.start()
    }

    @After
    fun stop() {
        server.close()
    }

    @Test
    fun healthIsOpenButModelsNeedTheKey() {
        val health = call("GET", "/v1/health", key = null)
        assertEquals(200, health.first)
        assertEquals(true, (Json.parse(health.second) as Map<*, *>)["ok"])

        assertEquals(401, call("GET", "/v1/models", key = null).first)
        assertEquals(401, call("GET", "/v1/models", key = "wrong").first)
        val models = call("GET", "/v1/models")
        assertEquals(200, models.first)
        assertTrue(models.second.contains("\"id\":\"tiny\""))
    }

    @Test
    fun answersAChatRequestWithOneJsonReply() {
        val (status, body) = call("POST", "/v1/chat/completions", CHAT_BODY)
        assertEquals(200, status)
        val root = Json.parse(body) as Map<*, *>
        val choice = (root["choices"] as List<*>)[0] as Map<*, *>
        assertEquals("Echo: hi", (choice["message"] as Map<*, *>)["content"])
        assertEquals("stop", choice["finish_reason"])
        assertEquals("tiny", root["model"])
    }

    @Test
    fun streamsTheReplyAsServerSentEvents() {
        val (status, body) = call("POST", "/v1/chat/completions", """{"stream":true,"messages":[{"role":"user","content":"hi"}]}""")
        assertEquals(200, status)
        val lines = body.lines().filter { it.startsWith("data: ") }.map { it.removePrefix("data: ") }
        assertEquals("[DONE]", lines.last())
        val text = lines.dropLast(1).mapNotNull { payload ->
            val choice = ((Json.parse(payload) as Map<*, *>)["choices"] as List<*>)[0] as Map<*, *>
            (choice["delta"] as Map<*, *>)["content"] as String?
        }.joinToString("")
        assertEquals("Echo: hi", text)
    }

    @Test
    fun refusesUnknownModelsBadJsonAndRoutes() {
        assertEquals(404, call("POST", "/v1/chat/completions", """{"model":"nope","messages":[{"role":"user","content":"x"}]}""").first)
        assertEquals(400, call("POST", "/v1/chat/completions", "{").first)
        assertEquals(404, call("GET", "/v1/unknown").first)
        assertEquals(405, call("GET", "/v1/chat/completions").first)
    }

    private fun call(method: String, path: String, body: String? = null, key: String? = KEY): Pair<Int, String> {
        val connection = URL("http://127.0.0.1:${server.port}$path").openConnection() as HttpURLConnection
        connection.requestMethod = method
        connection.connectTimeout = 5_000
        connection.readTimeout = 10_000
        if (key != null) connection.setRequestProperty("Authorization", "Bearer $key")
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

    private companion object {
        const val KEY = "test-key"
        const val CHAT_BODY = """{"messages":[{"role":"user","content":"hi"}]}"""
    }
}
