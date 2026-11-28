package com.modelmistress.core.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class OpenAiMmOptionsTest {
    private val messages = "\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]"

    @Test
    fun aPlainRequestHasNoRouterOptions() {
        val parsed = OpenAi.parseChatRequest("{\"model\":\"tiny\",$messages}")
        assertEquals(MmOptions(), parsed.mm)
        assertNull(parsed.mm.toolsAllowed)
    }

    @Test
    fun theMmObjectCarriesTheChatSandboxAndToolSwitch() {
        val parsed = OpenAi.parseChatRequest(
            "{\"model\":\"misty\",$messages,\"mm\":{\"chat\":\"chat-1\",\"sandbox\":\"box\",\"tools\":false}}",
        )
        assertEquals(MmOptions(chatId = "chat-1", sandboxId = "box", toolsAllowed = false), parsed.mm)
    }

    @Test
    fun toolDefinitionsFromTheClientAreRefusedWithAHelpfulMessage() {
        val error = assertThrows(ApiException::class.java) {
            OpenAi.parseChatRequest("{\"model\":\"tiny\",$messages,\"tools\":[]}")
        }
        assertEquals("tools_unsupported", error.code)
        assertEquals(400, error.status)
        assertTrue(error.message.orEmpty().contains("misty"))
    }

    @Test
    fun aBadRouterOptionIsRefused() {
        val error = assertThrows(ApiException::class.java) {
            OpenAi.parseChatRequest("{\"model\":\"tiny\",$messages,\"mm\":{\"tools\":\"yes\"}}")
        }
        assertEquals("invalid_request", error.code)
    }

    @Test
    fun aCompletionCarriesTheRouterKeysUnderTheirOwnName() {
        val json = OpenAi.completion(
            id = "chatcmpl-1",
            model = "tiny",
            created = 1L,
            text = "Hello",
            stats = GenerationStats(1, 2, FinishReason.STOP),
            extra = mapOf("mm" to mapOf("tool_calls" to 0)),
        )
        assertTrue(json.contains("\"mm\":{\"tool_calls\":0}"))
        assertTrue(json.contains("\"content\":\"Hello\""))
    }
}
