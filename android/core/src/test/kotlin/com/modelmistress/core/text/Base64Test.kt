package com.modelmistress.core.text

import org.junit.Assert.assertEquals
import org.junit.Test

class Base64Test {
    @Test
    fun encodesTheRfcVectors() {
        assertEquals("", Base64.encode(ByteArray(0)))
        assertEquals("Zg==", Base64.encode("f".toByteArray()))
        assertEquals("Zm8=", Base64.encode("fo".toByteArray()))
        assertEquals("Zm9v", Base64.encode("foo".toByteArray()))
        assertEquals("Zm9vYmFy", Base64.encode("foobar".toByteArray()))
    }

    @Test
    fun matchesTheJdkForEveryLengthAndByteValue() {
        for (length in 0..300) {
            val bytes = ByteArray(length) { index -> ((index * 37 + length * 11) % 256).toByte() }
            assertEquals(java.util.Base64.getEncoder().encodeToString(bytes), Base64.encode(bytes))
        }
    }
}
