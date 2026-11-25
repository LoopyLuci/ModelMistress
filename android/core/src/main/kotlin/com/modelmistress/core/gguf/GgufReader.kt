package com.modelmistress.core.gguf

import java.io.BufferedInputStream
import java.io.EOFException
import java.io.File
import java.io.InputStream

/** What a GGUF model's header says about it. Only the header is read; the weights are never loaded. */
data class GgufInfo(
    val version: Int,
    val tensorCount: Long,
    val metadata: Map<String, Any?>,
) {
    val architecture: String? get() = metadata["general.architecture"] as? String
    val name: String? get() = metadata["general.name"] as? String
    val fileType: Int? get() = integer("general.file_type")
    val contextLength: Int? get() = architecture?.let { integer("$it.context_length") }
    val blockCount: Int? get() = architecture?.let { integer("$it.block_count") }
    val embeddingLength: Int? get() = architecture?.let { integer("$it.embedding_length") }
    val quantization: String? get() = fileType?.let { GgufFileTypes.nameOf(it) }

    private fun integer(key: String): Int? {
        val value = (metadata[key] as? Number)?.toLong() ?: return null
        return if (value >= 0 && value <= Int.MAX_VALUE) value.toInt() else null
    }
}

/** A metadata array, kept as its element type and count. The elements themselves are skipped, not loaded. */
data class GgufArray(val elementType: Int, val count: Long)

/** The quantisation names llama.cpp writes as general.file_type. */
object GgufFileTypes {
    private val names = mapOf(
        0 to "F32", 1 to "F16", 2 to "Q4_0", 3 to "Q4_1", 7 to "Q8_0", 8 to "Q5_0", 9 to "Q5_1",
        10 to "Q2_K", 11 to "Q3_K_S", 12 to "Q3_K_M", 13 to "Q3_K_L", 14 to "Q4_K_S", 15 to "Q4_K_M",
        16 to "Q5_K_S", 17 to "Q5_K_M", 18 to "Q6_K", 19 to "IQ2_XXS", 20 to "IQ2_XS", 21 to "Q2_K_S",
        22 to "IQ3_XS", 23 to "IQ3_XXS", 24 to "IQ1_S", 25 to "IQ4_NL", 26 to "IQ3_S", 27 to "IQ3_M",
        28 to "IQ2_S", 29 to "IQ2_M", 30 to "IQ4_XS", 31 to "IQ1_M", 32 to "BF16",
    )

    fun nameOf(fileType: Int): String? = names[fileType]
}

/** Reads the header of a GGUF file (versions 2 and 3) without loading its tensors. */
object GgufReader {
    private const val MAGIC = 0x46554747L
    private const val MAX_KEY_BYTES = 4 * 1024
    private const val MAX_VALUE_BYTES = 4 * 1024 * 1024
    private const val MAX_KEYS = 1_000_000L

    fun read(file: File): GgufInfo = file.inputStream().use { read(it) }

    fun read(input: InputStream): GgufInfo = Parser(BufferedInputStream(input)).header()

    private class Parser(private val input: InputStream) {
        private val scratch = ByteArray(8)

        fun header(): GgufInfo {
            require(u32() == MAGIC) { "This file is not a GGUF model." }
            val version = u32()
            require(version == 2L || version == 3L) { "GGUF version $version is not supported." }
            val tensorCount = u64()
            val keyCount = u64()
            require(keyCount >= 0 && keyCount <= MAX_KEYS) { "The GGUF header lists too many keys." }
            val metadata = LinkedHashMap<String, Any?>()
            var index = 0L
            while (index < keyCount) {
                val key = string(MAX_KEY_BYTES)
                metadata[key] = value(u32().toInt())
                index++
            }
            return GgufInfo(version.toInt(), tensorCount, metadata)
        }

        private fun value(type: Int): Any? = when (type) {
            0 -> u8().toLong()
            1 -> u8().toByte().toLong()
            2 -> u16().toLong()
            3 -> u16().toShort().toLong()
            4 -> u32()
            5 -> u32().toInt().toLong()
            6 -> Float.fromBits(u32().toInt()).toDouble()
            7 -> u8() != 0
            8 -> string(MAX_VALUE_BYTES)
            9 -> array()
            10, 11 -> u64()
            12 -> Double.fromBits(u64())
            else -> throw IllegalArgumentException("Unknown GGUF value type $type.")
        }

        private fun array(): GgufArray {
            val elementType = u32().toInt()
            val count = u64()
            require(count >= 0) { "A GGUF array has a bad length." }
            when (elementType) {
                8 -> {
                    var index = 0L
                    while (index < count) {
                        skip(u64())
                        index++
                    }
                }
                9 -> throw IllegalArgumentException("Nested GGUF arrays are not supported.")
                else -> skip(count * fixedSize(elementType))
            }
            return GgufArray(elementType, count)
        }

        private fun fixedSize(type: Int): Long = when (type) {
            0, 1, 7 -> 1L
            2, 3 -> 2L
            4, 5, 6 -> 4L
            10, 11, 12 -> 8L
            else -> throw IllegalArgumentException("Unknown GGUF array type $type.")
        }

        private fun skip(count: Long) {
            var remaining = count
            while (remaining > 0) {
                val skipped = input.skip(remaining)
                if (skipped > 0) {
                    remaining -= skipped
                } else {
                    if (input.read() < 0) throw EOFException("The GGUF file ends early.")
                    remaining--
                }
            }
        }

        private fun string(maxBytes: Int): String {
            val length = u64()
            require(length >= 0 && length <= maxBytes) { "A GGUF string is longer than the reader allows." }
            return String(bytes(length.toInt()), Charsets.UTF_8)
        }

        private fun bytes(count: Int): ByteArray {
            val out = ByteArray(count)
            var read = 0
            while (read < count) {
                val n = input.read(out, read, count - read)
                if (n < 0) throw EOFException("The GGUF file ends early.")
                read += n
            }
            return out
        }

        private fun fill(count: Int) {
            var read = 0
            while (read < count) {
                val n = input.read(scratch, read, count - read)
                if (n < 0) throw EOFException("The GGUF file ends early.")
                read += n
            }
        }

        private fun u8(): Int {
            fill(1)
            return scratch[0].toInt() and 0xFF
        }

        private fun u16(): Int {
            fill(2)
            return (scratch[0].toInt() and 0xFF) or ((scratch[1].toInt() and 0xFF) shl 8)
        }

        private fun u32(): Long {
            fill(4)
            return littleEndian(4)
        }

        private fun u64(): Long {
            fill(8)
            return littleEndian(8)
        }

        private fun littleEndian(count: Int): Long {
            var value = 0L
            for (i in count - 1 downTo 0) {
                value = (value shl 8) or (scratch[i].toLong() and 0xFF)
            }
            return value
        }
    }
}
