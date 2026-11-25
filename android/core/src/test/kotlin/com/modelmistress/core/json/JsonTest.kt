package com.modelmistress.core.json

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class JsonTest {
    @Test
    fun readsObjectsArraysNumbersAndEscapes() {
        val value = Json.parse("""{"a":[1,2.5,"x\né"],"b":true,"c":null,"d":-7}""") as Map<*, *>
        val list = value["a"] as List<*>
        assertEquals(1L, list[0])
        assertEquals(2.5, list[1])
        assertEquals("x\né", list[2])
        assertEquals(true, value["b"])
        assertNull(value["c"])
        assertEquals(-7L, value["d"])
    }

    @Test
    fun rejectsTrailingTextBadSyntaxAndDeepNesting() {
        expectFailure("{} x")
        expectFailure("{\"a\" 1}")
        expectFailure("[".repeat(200) + "]".repeat(200))
    }

    @Test
    fun writesEscapedStringsAndNumbers() {
        val text = Json.write(mapOf("k" to "a\"b\\c\n", "n" to 3, "f" to 1.5, "list" to listOf(true, null)))
        assertEquals("{\"k\":\"a\\\"b\\\\c\\n\",\"n\":3,\"f\":1.5,\"list\":[true,null]}", text)
    }

    @Test
    fun roundTripsWhatItWrites() {
        val original = mapOf("text" to "héllo \u0001", "items" to listOf(1L, "two"))
        assertEquals(original, Json.parse(Json.write(original)))
    }

    private fun expectFailure(text: String) {
        try {
            Json.parse(text)
            fail("Expected a JsonException for: $text")
        } catch (expected: JsonException) {
            assertTrue(expected.message.orEmpty().isNotEmpty())
        }
    }
}
