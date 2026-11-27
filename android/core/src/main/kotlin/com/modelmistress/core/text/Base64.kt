package com.modelmistress.core.text

/**
 * Standard Base64 (RFC 4648) with padding. It is written out here because java.util.Base64 needs Android API 26, and this
 * app runs from API 24.
 */
object Base64 {
    private const val ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"

    fun encode(bytes: ByteArray): String {
        val out = StringBuilder((bytes.size + 2) / 3 * 4)
        var i = 0
        while (i + 2 < bytes.size) {
            val group = byteAt(bytes, i) shl 16 or (byteAt(bytes, i + 1) shl 8) or byteAt(bytes, i + 2)
            appendSextets(out, group, 4)
            i += 3
        }
        when (bytes.size - i) {
            1 -> {
                appendSextets(out, byteAt(bytes, i) shl 16, 2)
                out.append("==")
            }
            2 -> {
                appendSextets(out, (byteAt(bytes, i) shl 16) or (byteAt(bytes, i + 1) shl 8), 3)
                out.append('=')
            }
        }
        return out.toString()
    }

    private fun byteAt(bytes: ByteArray, index: Int): Int = bytes[index].toInt() and 0xFF

    /** Appends the first [count] six-bit groups of the 24-bit [group], most significant first. */
    private fun appendSextets(out: StringBuilder, group: Int, count: Int) {
        for (step in 0 until count) {
            out.append(ALPHABET[(group shr (18 - 6 * step)) and 63])
        }
    }
}
