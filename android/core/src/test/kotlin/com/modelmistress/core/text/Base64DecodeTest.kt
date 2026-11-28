package com.modelmistress.core.text

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class Base64DecodeTest {
    @Test
    fun decodesWhatEncodeWrote() {
        for (length in 0..9) {
            val bytes = ByteArray(length) { (it * 37 + 11).toByte() }
            assertArrayEquals("length $length", bytes, Base64.decode(Base64.encode(bytes)))
        }
    }

    @Test
    fun decodesTheRfcExamples() {
        assertArrayEquals("".toByteArray(), Base64.decode(""))
        assertArrayEquals("f".toByteArray(), Base64.decode("Zg=="))
        assertArrayEquals("fo".toByteArray(), Base64.decode("Zm8="))
        assertArrayEquals("foo".toByteArray(), Base64.decode("Zm9v"))
        assertArrayEquals("foobar".toByteArray(), Base64.decode("Zm9vYmFy"))
    }

    @Test
    fun refusesAWrongLength() {
        assertThrows(IllegalArgumentException::class.java) { Base64.decode("Zm9") }
    }

    @Test
    fun refusesACharacterOutsideTheAlphabet() {
        assertThrows(IllegalArgumentException::class.java) { Base64.decode("Zm9!") }
    }

    @Test
    fun refusesPaddingInTheMiddle() {
        assertThrows(IllegalArgumentException::class.java) { Base64.decode("Z=9v") }
    }
}
