package com.modelmistress.core.text

import java.nio.charset.StandardCharsets

/**
 * Turns byte chunks that may end inside a UTF-8 character into text. The unfinished bytes wait for the next chunk, so a
 * character split across two tokens is never shown as replacement characters.
 */
class Utf8Chunker {
    private var pending = ByteArray(0)

    fun push(bytes: ByteArray): String {
        if (bytes.isEmpty() && pending.isEmpty()) return ""
        val all = if (pending.isEmpty()) bytes else pending + bytes
        val cut = completeLength(all)
        pending = all.copyOfRange(cut, all.size)
        return String(all, 0, cut, StandardCharsets.UTF_8)
    }

    /** What is left at the end. An unfinished character comes out as the replacement character. */
    fun finish(): String {
        val rest = pending
        pending = ByteArray(0)
        return String(rest, StandardCharsets.UTF_8)
    }

    companion object {
        /** The length of the prefix of [bytes] that ends on a character boundary. */
        fun completeLength(bytes: ByteArray): Int {
            val n = bytes.size
            var back = 1
            while (back <= minOf(4, n)) {
                val b = bytes[n - back].toInt() and 0xFF
                if ((b and 0xC0) == 0x80) {
                    back++
                    continue
                }
                val needed = when {
                    b < 0x80 -> 1
                    (b and 0xE0) == 0xC0 -> 2
                    (b and 0xF0) == 0xE0 -> 3
                    (b and 0xF8) == 0xF0 -> 4
                    else -> 1
                }
                return if (back >= needed) n else n - back
            }
            return n
        }
    }
}
