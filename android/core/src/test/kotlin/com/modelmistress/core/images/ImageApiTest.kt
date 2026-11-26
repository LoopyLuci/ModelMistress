package com.modelmistress.core.images

import com.modelmistress.core.chat.ApiException
import com.modelmistress.core.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class ImageApiTest {
    @Test
    fun aPromptAloneGetsTheDesktopDefaults() {
        val request = ImageApi.parse("{\"prompt\": \"a red bicycle\"}")
        assertNull(request.model)
        assertEquals(1, request.count)
        assertEquals(ImageSpec("a red bicycle", "", 512, 512, 20, -1L), request.image)
    }

    @Test
    fun autoAndBlankModelsMeanTheDefault() {
        assertNull(ImageApi.parse("{\"prompt\": \"x\", \"model\": \"auto\"}").model)
        assertNull(ImageApi.parse("{\"prompt\": \"x\", \"model\": \"  \"}").model)
        assertEquals("sd15", ImageApi.parse("{\"prompt\": \"x\", \"model\": \"sd15\"}").model)
    }

    @Test
    fun readsEverySizeAndOption() {
        val request = ImageApi.parse(
            "{\"prompt\": \"a cat\", \"n\": 2, \"size\": \"256X384\", \"steps\": 8, \"seed\": 7, " +
                "\"negative_prompt\": \"blurry\", \"response_format\": \"b64_json\"}",
        )
        assertEquals(2, request.count)
        assertEquals(ImageSpec("a cat", "blurry", 256, 384, 8, 7L), request.image)
    }

    @Test
    fun aFixedSeedMovesOnForEachImageAndARandomOneStaysRandom() {
        val fixed = ImageApi.parse("{\"prompt\": \"x\", \"n\": 3, \"seed\": 10}")
        assertEquals(listOf(10L, 11L, 12L), (0 until 3).map { fixed.spec(it).seed })
        val random = ImageApi.parse("{\"prompt\": \"x\", \"n\": 3}")
        assertEquals(listOf(-1L, -1L, -1L), (0 until 3).map { random.spec(it).seed })
    }

    @Test
    fun refusesEachBadRequestWithItsCode() {
        assertRefused("{\"prompt\": \"  \"}", "invalid_request")
        assertRefused("{}", "invalid_request")
        assertRefused("{\"prompt\": \"${"x".repeat(ImageApi.MAX_PROMPT_CHARS + 1)}\"}", "invalid_request")
        assertRefused("{\"prompt\": \"x\", \"n\": 0}", "invalid_request")
        assertRefused("{\"prompt\": \"x\", \"n\": ${ImageApi.MAX_IMAGES + 1}}", "invalid_request")
        assertRefused("{\"prompt\": \"x\", \"n\": 1.5}", "invalid_request")
        assertRefused("{\"prompt\": \"x\", \"size\": \"100x100\"}", "invalid_request")
        assertRefused("{\"prompt\": \"x\", \"size\": \"512\"}", "invalid_request")
        assertRefused("{\"prompt\": \"x\", \"size\": \"4096x512\"}", "invalid_request")
        assertRefused("{\"prompt\": \"x\", \"steps\": 0}", "invalid_request")
        assertRefused("{\"prompt\": \"x\", \"seed\": -2}", "invalid_request")
        assertRefused("{\"prompt\": \"x\", \"response_format\": \"url\"}", "unsupported_parameter")
        assertRefused("[1, 2]", "invalid_json")
        assertRefused("not json", "invalid_json")
    }

    @Test
    fun theResponseCarriesEachImageAsBase64InOrder() {
        val first = byteArrayOf(1, 2, 3)
        val second = byteArrayOf(9)
        val body = ImageApi.response(1700000000L, listOf(first, second))
        val parsed = Json.parse(body) as Map<*, *>
        assertEquals(1700000000L, (parsed["created"] as Number).toLong())
        val data = parsed["data"] as List<*>
        assertEquals(listOf("AQID", "CQ=="), data.map { (it as Map<*, *>)["b64_json"] })
        assertTrue(body.contains("\"b64_json\""))
    }

    private fun assertRefused(body: String, code: String) {
        try {
            ImageApi.parse(body)
            fail("Expected $code for $body")
        } catch (error: ApiException) {
            assertEquals(code, error.code)
        }
    }
}
