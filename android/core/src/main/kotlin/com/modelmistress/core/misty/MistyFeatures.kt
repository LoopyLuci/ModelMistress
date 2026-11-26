package com.modelmistress.core.misty

/**
 * A kind of question that needs a tool, and the tools that answer it. A question offers only the tools of the families it
 * points at; a greeting points at none, so it is answered with no tool offered.
 */
enum class ToolFamily(val label: String, val tools: List<String>) {
    TIME("Time and date", listOf("datetime")),
    CALCULATION("Calculator", listOf("calc")),
    FILES_READ("Read files", listOf("fs_list", "fs_read", "fs_search")),
    FILES_CREATE("Create or replace a file", listOf("fs_list", "fs_read", "fs_search", "fs_write")),
    FILES_CHANGE("Change text in a file", listOf("fs_list", "fs_read", "fs_search", "fs_edit")),
    DEVICE("Device status", listOf("device_status")),
    MODELS("Installed models", listOf("models_list")),
}

/**
 * What the wording of one question points at. Computed from the text alone, with no model call. The word lists are
 * heuristics. [MistyRouter] names the reason for each choice, and the owner's feedback can override them.
 */
data class MistyFeatures(
    /** How much the question reads as needing reasoning, 0 to 1. */
    val reasoning: Double,
    /** The tool families the wording points at, in the order they were found. Empty for a question that needs no tool. */
    val families: Set<ToolFamily>,
) {
    companion object {
        private val REASON_WORDS = listOf(
            "prove", "derive", "step by step", "analyze", "analyse", "trade-off", "tradeoff", "compare", "design",
            "architecture", "optimi", "why does", "why is", "explain in depth", "pros and cons", "plan", "strategy",
            "evaluate", "theorem", "complexity", "algorithm", "reason about", "root cause",
        )
        private val TIME_WORDS = Regex("""\b(time|date|clock|weekday|what day)\b""")
        private val CALC_WORDS = Regex("""\b(calculate|calc|compute|sqrt|square root|how much is)\b|\bwhat('s| is) \d""")
        private val ARITHMETIC_WORDS = listOf(
            "calculate", "compute", "percent", "%", "plus", "minus", "divided", "multipl", "square root",
        )
        private val SUM = Regex("""\d+(\.\d+)?(\*|x|×|/|\+|-|times|plus|minus|dividedby)\d+""")
        private val FILE_NOUN = Regex(
            """\b(files?|folders?|director(y|ies)|notes|documents?|workspace|text file)\b|[a-z0-9_-]+\.(txt|md|csv|json|log)\b""",
        )
        /** Words that ask for text in an existing file to change: fs_edit, which replaces one exact piece of text. */
        private val CHANGE_VERB = Regex("""\b(change|replace|edit|update|fix|modify|rename|correct|append|insert|add)\b""")

        /** Words that ask for a file to be made or replaced whole: fs_write. */
        private val CREATE_VERB = Regex("""\b(write|save|create|make|new|overwrite|rewrite)\b""")
        private val DEVICE_WORDS = Regex("""\b(memory|ram|storage|battery|needle|free space|device status)\b""")
        private val MODEL_WORD = Regex("""\bmodels?\b""")
        private val MODEL_ASK = Regex("""\b(which|what|list|installed|available|have|loaded)\b""")

        /** Questions longer than this many tokens count as long for reasoning. */
        private const val LONG_TOKENS = 400

        fun of(text: String): MistyFeatures {
            val lower = text.trim().lowercase()
            val tokens = text.length / 4 + 1
            val families = linkedSetOf<ToolFamily>()
            if (TIME_WORDS.containsMatchIn(lower)) families += ToolFamily.TIME
            if (isArithmetic(lower) || CALC_WORDS.containsMatchIn(lower)) families += ToolFamily.CALCULATION
            val fileWords = FILE_NOUN.containsMatchIn(lower)
            if (fileWords) families += ToolFamily.FILES_READ
            if (fileWords && CHANGE_VERB.containsMatchIn(lower)) families += ToolFamily.FILES_CHANGE
            if (fileWords && CREATE_VERB.containsMatchIn(lower)) families += ToolFamily.FILES_CREATE
            if (DEVICE_WORDS.containsMatchIn(lower)) families += ToolFamily.DEVICE
            if (MODEL_WORD.containsMatchIn(lower) && MODEL_ASK.containsMatchIn(lower)) families += ToolFamily.MODELS
            return MistyFeatures(
                reasoning = (score(lower, REASON_WORDS, 0.35) + if (tokens > LONG_TOKENS) 0.2 else 0.0)
                    .coerceAtMost(1.0),
                families = families,
            )
        }

        fun isArithmetic(text: String): Boolean {
            val lower = text.lowercase()
            return ARITHMETIC_WORDS.any { lower.contains(it) } ||
                SUM.containsMatchIn(lower.filter { !it.isWhitespace() })
        }

        private fun score(text: String, words: List<String>, perHit: Double): Double =
            (words.count { text.contains(it) } * perHit).coerceAtMost(1.0)
    }
}
