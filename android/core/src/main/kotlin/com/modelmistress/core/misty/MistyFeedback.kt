package com.modelmistress.core.misty

import com.modelmistress.core.json.Json
import java.io.File

/** The owner's verdict on one reply. */
enum class FeedbackLabel(val wire: String) {
    /** The reply was right. Similar questions keep the tools this one was offered. */
    GOOD("good"),

    /** The reply was wrong. Kept for review; it does not change routing. */
    BAD("bad"),

    /** The reply should have used no tool. Similar questions are answered with none offered. */
    NO_TOOLS("no_tools"),

    /** The reply should have used a tool. Similar questions are offered the tools in [FeedbackExample.tools]. */
    NEEDS_TOOLS("needs_tools"),
    ;

    companion object {
        fun of(wire: String): FeedbackLabel? = values().firstOrNull { it.wire == wire }
    }
}

/**
 * One rated reply. [tools] are the tools the router offered for the question, or the ones the owner chose for
 * [FeedbackLabel.NEEDS_TOOLS]. [calls] says what each tool call did, as `name:ran`, `name:refused` or `name:failed`.
 */
data class FeedbackExample(
    val question: String,
    val label: FeedbackLabel,
    val tools: List<String>,
    val calls: List<String>,
    val answer: String,
    val at: Long,
) {
    companion object {
        /** The reply text kept with an example. Longer replies are cut to this many characters. */
        const val MAX_ANSWER_CHARS = 1000
    }
}

/** The saved verdicts. Misty reads them to route similar questions, and they can be exported for training later. */
interface MistyFeedback {
    /** The saved examples, oldest first. */
    fun examples(): List<FeedbackExample>

    fun add(example: FeedbackExample)

    fun clear()
}

/** Keeps the verdicts as one JSON object per line in [file]. The newest [capacity] are kept. */
class FileMistyFeedback(private val file: File, private val capacity: Int = DEFAULT_CAPACITY) : MistyFeedback {
    @Synchronized
    override fun examples(): List<FeedbackExample> {
        if (!file.isFile) return emptyList()
        return file.readLines().mapNotNull { decode(it) }
    }

    @Synchronized
    override fun add(example: FeedbackExample) {
        val kept = (examples() + example).takeLast(capacity)
        file.parentFile?.mkdirs()
        file.writeText(kept.joinToString("") { Json.write(encode(it)) + "\n" })
    }

    @Synchronized
    override fun clear() {
        file.delete()
    }

    private fun encode(example: FeedbackExample): Map<String, Any> = mapOf(
        "question" to example.question,
        "label" to example.label.wire,
        "tools" to example.tools,
        "calls" to example.calls,
        "answer" to example.answer,
        "at" to example.at,
    )

    private fun decode(line: String): FeedbackExample? {
        if (line.isBlank()) return null
        val fields = runCatching { Json.parse(line) as? Map<*, *> }.getOrNull() ?: return null
        val question = fields["question"] as? String ?: return null
        val label = (fields["label"] as? String)?.let { FeedbackLabel.of(it) } ?: return null
        return FeedbackExample(
            question = question,
            label = label,
            tools = strings(fields["tools"]),
            calls = strings(fields["calls"]),
            answer = fields["answer"] as? String ?: "",
            at = (fields["at"] as? Number)?.toLong() ?: 0L,
        )
    }

    private fun strings(value: Any?): List<String> = (value as? List<*>).orEmpty().mapNotNull { it as? String }

    companion object {
        const val DEFAULT_CAPACITY = 500
    }
}

/** Matches a new question to the verdicts saved for similar ones. */
object MistyLearning {
    /** How much two questions must share, from 0 to 1, before the owner's verdict on one applies to the other. */
    const val SIMILAR = 0.6

    private val STOP_WORDS = setOf(
        "the", "a", "an", "is", "are", "it", "to", "of", "in", "on", "for", "and", "or", "my", "me", "i", "you", "do",
        "does", "can", "please", "what", "that", "this", "with",
    )

    /** The words of [text] that carry its meaning: lower case, no punctuation, no common small words. */
    fun words(text: String): Set<String> = text.lowercase()
        .split(Regex("""[^\p{L}\p{N}_.]+"""))
        .map { it.trim('.') }
        .filter { it.length > 1 && it !in STOP_WORDS }
        .toSet()

    /** Share of words the two questions have in common, from 0 to 1. */
    fun similarity(a: String, b: String): Double {
        val left = words(a)
        val right = words(b)
        if (left.isEmpty() || right.isEmpty()) return 0.0
        return (left intersect right).size.toDouble() / (left union right).size
    }

    /**
     * The verdict that applies to [question]: the most similar saved one that is close enough, the newest first among
     * equals. Verdicts marked [FeedbackLabel.BAD] never apply, because they say nothing about the route.
     */
    fun taught(question: String, examples: List<FeedbackExample>): FeedbackExample? = examples
        .filter { it.label != FeedbackLabel.BAD }
        .map { it to similarity(question, it.question) }
        .filter { it.second >= SIMILAR }
        .sortedWith(compareByDescending<Pair<FeedbackExample, Double>> { it.second }.thenByDescending { it.first.at })
        .firstOrNull()
        ?.first
}
