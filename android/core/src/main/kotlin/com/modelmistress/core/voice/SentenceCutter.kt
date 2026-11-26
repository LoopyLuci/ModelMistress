package com.modelmistress.core.voice

/**
 * Cuts a reply that is still being written into sentences that can be spoken as they complete, so speech starts before the
 * whole reply is done. Text inside `<think>` tags, and markdown marks, are never spoken.
 *
 * Call [cut] with the reply so far each time it grows. Each sentence comes back once. Call it once more with [final] set when
 * the reply is finished, which also returns the unfinished last piece.
 */
class SentenceCutter {
    /** How much of the visible reply has been cut so far. */
    private var spokenUpTo = 0

    fun cut(reply: String, final: Boolean = false): List<String> {
        val visible = withoutThinking(reply)
        // A reply that shrank (a tool step cleared the text) starts again from its beginning.
        if (visible.length < spokenUpTo) spokenUpTo = 0
        val out = mutableListOf<String>()
        var index = spokenUpTo
        while (index < visible.length) {
            val c = visible[index]
            val ends = when {
                c == '\n' -> true
                c == '.' || c == '!' || c == '?' -> {
                    val next = visible.getOrNull(index + 1)
                    if (next == null) final else next.isWhitespace()
                }
                else -> false
            }
            if (ends) {
                clean(visible.substring(spokenUpTo, index + 1))?.let { out += it }
                spokenUpTo = index + 1
            }
            index++
        }
        if (final && spokenUpTo < visible.length) {
            clean(visible.substring(spokenUpTo))?.let { out += it }
            spokenUpTo = visible.length
        }
        return out
    }

    companion object {
        private val THINK_BLOCK = Regex("<think>[\\s\\S]*?</think>")
        private val MARKDOWN_MARKS = Regex("[*#`~_|]")

        /** The reply without its thinking: complete `<think>` blocks are removed, and an unfinished one hides the rest. */
        fun withoutThinking(reply: String): String {
            val closed = reply.replace(THINK_BLOCK, "")
            val open = closed.indexOf("<think>")
            return if (open >= 0) closed.substring(0, open) else closed
        }

        /** The text as it is said: markdown marks dropped, spaces collapsed. Null when nothing is left to say. */
        fun clean(piece: String): String? {
            val said = MARKDOWN_MARKS.replace(piece, "").replace(Regex("\\s+"), " ").trim()
            return said.takeIf { it.any { c -> c.isLetterOrDigit() } }
        }
    }
}
