package com.modelmistress.core.gguf

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.EOFException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Test

class GgufReaderTest {
    @Test
    fun readsHeaderFieldsAndSkipsArrays() {
        val info = GgufReader.read(ByteArrayInputStream(sampleModel()))
        assertEquals(3, info.version)
        assertEquals(5L, info.tensorCount)
        assertEquals("llama", info.architecture)
        assertEquals("Tiny Test", info.name)
        assertEquals("Q4_K_M", info.quantization)
        assertEquals(4096, info.contextLength)
        assertEquals(32, info.blockCount)
        assertEquals(2048, info.embeddingLength)
        assertEquals(true, info.metadata["flag"])
        assertEquals(1.5, info.metadata["general.score"])
        assertEquals(GgufArray(elementType = 8, count = 3), info.metadata["tokenizer.ggml.tokens"])
    }

    @Test
    fun refusesOtherFilesAndTruncatedHeaders() {
        assertThrows(IllegalArgumentException::class.java) {
            GgufReader.read(ByteArrayInputStream(ByteArray(64)))
        }
        val cut = sampleModel().copyOf(40)
        assertThrows(EOFException::class.java) { GgufReader.read(ByteArrayInputStream(cut)) }
    }

    @Test
    fun unknownFileTypesHaveNoName() {
        assertNull(GgufFileTypes.nameOf(999))
    }

    private fun sampleModel(): ByteArray {
        val out = ByteArrayOutputStream()
        fun u32(value: Long) = repeat(4) { out.write(((value shr (8 * it)) and 0xFF).toInt()) }
        fun u64(value: Long) = repeat(8) { out.write(((value shr (8 * it)) and 0xFF).toInt()) }
        fun text(value: String) {
            val bytes = value.toByteArray(Charsets.UTF_8)
            u64(bytes.size.toLong())
            out.write(bytes)
        }
        fun key(name: String, type: Int) {
            text(name)
            u32(type.toLong())
        }

        u32(0x46554747L)
        u32(3)
        u64(5)
        u64(9)
        key("general.architecture", 8)
        text("llama")
        key("general.name", 8)
        text("Tiny Test")
        key("general.file_type", 4)
        u32(15)
        key("llama.context_length", 4)
        u32(4096)
        key("llama.block_count", 4)
        u32(32)
        key("llama.embedding_length", 4)
        u32(2048)
        key("flag", 7)
        out.write(1)
        key("general.score", 6)
        u32(java.lang.Float.floatToIntBits(1.5f).toLong())
        key("tokenizer.ggml.tokens", 9)
        u32(8)
        u64(3)
        text("a")
        text("bb")
        text("ccc")
        return out.toByteArray()
    }
}
