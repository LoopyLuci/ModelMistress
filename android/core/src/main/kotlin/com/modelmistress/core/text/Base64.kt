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

    /**
     * Decodes standard Base64 with padding. Throws [IllegalArgumentException] for text that is not valid Base64, such as a
     * length that is not a multiple of four or a character outside the alphabet.
     */
    fun decode(text: String): ByteArray {
        require(text.length % 4 == 0) { "The Base64 text does not have a length that is a multiple of four." }
        val padding = text.takeLastWhile { it == '=' }.length
        require(padding <= 2 && text.dropLast(padding).none { it == '=' }) { "The Base64 padding is not valid." }
        val out = java.io.ByteArrayOutputStream(text.length / 4 * 3)
        var index = 0
        while (index < text.length) {
            var group = 0
            var pads = 0
            for (offset in 0 until 4) {
                val char = text[index + offset]
                val value = if (char == '=') {
                    pads++
                    0
                } else {
                    val found = ALPHABET.indexOf(char)
                    require(found >= 0) { "The Base64 text contains a character that is not allowed." }
                    found
                }
                group = (group shl 6) or value
            }
            out.write(group shr 16 and 0xFF)
            if (pads < 2) out.write(group shr 8 and 0xFF)
            if (pads < 1) out.write(group and 0xFF)
            index += 4
        }
        return out.toByteArray()
    }

    private fun byteAt(bytes: ByteArray, index: Int): Int = bytes[index].toInt() and 0xFF

    /** Appends the first [count] six-bit groups of the 24-bit [group], most significant first. */
    private fun appendSextets(out: StringBuilder, group: Int, count: Int) {
        for (step in 0 until count) {
            out.append(ALPHABET[(group shr (18 - 6 * step)) and 63])
        }
    }
}
