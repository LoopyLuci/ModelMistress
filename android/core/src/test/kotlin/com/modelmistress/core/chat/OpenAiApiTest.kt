package com.modelmistress.core.chat

import com.modelmistress.core.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Test

class OpenAiApiTest {
    @Test
    fun parsesMessagesAndOptions() {
        val request = OpenAi.parseChatRequest(
            """
            {"model":"tiny","stream":true,"max_tokens":64,"temperature":0.2,"top_p":0.9,"top_k":20,"seed":7,
             "messages":[{"role":"developer","content":"Be brief."},
                         {"role":"user","content":[{"type":"text","text":"Hi "},{"type":"text","text":"there"}]},
                         {"role":"assistant"}]}
            """.trimIndent(),
        )
        assertEquals("tiny", request.model)
        assertEquals(true, request.stream)
        assertEquals(64, request.options.maxTokens)
        assertEquals(0.2f, request.options.temperature, 0.0001f)
        assertEquals(20, request.options.topK)
        assertEquals(7L, request.options.seed)
        assertEquals(listOf(Role.SYSTEM, Role.USER, Role.ASSISTANT), request.messages.map { it.role })
        assertEquals("Hi there", request.messages[1].content)
        assertEquals("", request.messages[2].content)
    }

    @Test
    fun usesDefaultsWhenOptionsAreMissing() {
        val request = OpenAi.parseChatRequest("""{"messages":[{"role":"user","content":"Hi"}]}""")
        assertNull(request.model)
        assertEquals(false, request.stream)
        assertEquals(GenerationOptions(), request.options)
    }

    @Test
    fun refusesWhatTheDeviceCannotDo() {
        assertCode("tools_unsupported", """{"messages":[{"role":"user","content":"x"}],"tools":[]}""")
        assertCode(
            "unsupported_content",
            """{"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"x"}}]}]}""",
        )
        assertCode("unsupported_role", """{"messages":[{"role":"tool","content":"x"}]}""")
        assertCode("invalid_json", "{")
        assertCode("invalid_request", """{"messages":[]}""")
        assertCode("invalid_request", """{"messages":[{"role":"user","content":"x"}],"temperature":3}""")
        assertCode("invalid_request", """{"messages":[{"role":"user","content":"x"}],"max_tokens":0}""")
        assertCode("unsupported_parameter", """{"messages":[{"role":"user","content":"x"}],"n":2}""")
    }

    @Test
    fun writesCompletionsChunksModelsAndErrors() {
        val stats = GenerationStats(promptTokens = 5, completionTokens = 2, finishReason = FinishReason.STOP)
        val completion = Json.parse(OpenAi.completion("chatcmpl-1", "tiny", 100L, "Hi!", stats)) as Map<*, *>
        assertEquals("chat.completion", completion["object"])
        val choice = (completion["choices"] as List<*>)[0] as Map<*, *>
        assertEquals("Hi!", (choice["message"] as Map<*, *>)["content"])
        assertEquals("stop", choice["finish_reason"])
        assertEquals(7L, (completion["usage"] as Map<*, *>)["total_tokens"])

        val chunk = Json.parse(OpenAi.chunk("chatcmpl-1", "tiny", 100L, mapOf("content" to "Hi"), null)) as Map<*, *>
        assertEquals("chat.completion.chunk", chunk["object"])
        val delta = ((chunk["choices"] as List<*>)[0] as Map<*, *>)["delta"] as Map<*, *>
        assertEquals("Hi", delta["content"])

        val models = Json.parse(OpenAi.models(listOf(ModelInfo("tiny", 2048, 10L, "llama Q4_K_M")), 100L)) as Map<*, *>
        val first = (models["data"] as List<*>)[0] as Map<*, *>
        assertEquals("tiny", first["id"])
        assertEquals(2048L, first["context_length"])

        val error = Json.parse(OpenAi.error("Nope.", "invalid_request_error", "bad")) as Map<*, *>
        assertEquals("bad", (error["error"] as Map<*, *>)["code"])
    }

    private fun assertCode(code: String, body: String) {
        val error = assertThrows(ApiException::class.java) { OpenAi.parseChatRequest(body) }
        assertEquals(code, error.code)
        assertEquals(400, error.status)
    }
}
