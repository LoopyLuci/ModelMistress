package com.modelmistress.core.text

import org.junit.Assert.assertEquals
import org.junit.Test

class Utf8ChunkerTest {
    @Test
    fun holdsBackACharacterSplitAcrossChunks() {
        val chunker = Utf8Chunker()
        val euro = "€".toByteArray(Charsets.UTF_8)
        assertEquals("", chunker.push(euro.copyOfRange(0, 1)))
        assertEquals("", chunker.push(euro.copyOfRange(1, 2)))
        assertEquals("€", chunker.push(euro.copyOfRange(2, 3)))
    }

    @Test
    fun emitsWhatIsCompleteAndFlushesTheRest() {
        val chunker = Utf8Chunker()
        val text = "a😀b".toByteArray(Charsets.UTF_8)
        assertEquals("a", chunker.push(text.copyOfRange(0, 3)))
        assertEquals("😀b", chunker.push(text.copyOfRange(3, text.size)))
        assertEquals("", chunker.finish())

        val cut = Utf8Chunker()
        cut.push(byteArrayOf(0xE2.toByte()))
        assertEquals("�", cut.finish())
    }

    @Test
    fun completeLengthFindsTheBoundary() {
        assertEquals(3, Utf8Chunker.completeLength("abc".toByteArray()))
        assertEquals(1, Utf8Chunker.completeLength(byteArrayOf(0x61, 0xC3.toByte())))
    }
}
