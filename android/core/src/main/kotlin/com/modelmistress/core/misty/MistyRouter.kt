package com.modelmistress.core.misty

/** The router's decision for one question, with the reasons in the order they were decided. */
data class MistyRoute(
    /** The tools offered for this question, by name. Empty means the question is answered with no tool offered. */
    val tools: List<String>,
    /** Needle tries one tool call first. Only when tools are offered and the question does not read as reasoning. */
    val reflex: Boolean,
    val reasons: List<String>,
)

/**
 * Decides which tools a question is offered. A question that needs none, such as a greeting, is offered none, so no tool
 * can run on it. The owner's saved verdicts on similar questions come first; the wording decides the rest.
 */
object MistyRouter {
    /** A question that reads as reasoning at this level or more is answered by the chat model without Needle's first try. */
    const val MAX_REASONING_FOR_REFLEX = 0.35

    fun route(
        question: String,
        available: Set<String>,
        examples: List<FeedbackExample>,
        reflexReady: Boolean,
    ): MistyRoute {
        val features = MistyFeatures.of(question)
        val taught = MistyLearning.taught(question, examples)
        val reasons = mutableListOf<String>()
        val offered = if (taught != null) {
            val chosen = if (taught.label == FeedbackLabel.NO_TOOLS) emptyList() else taught.tools
            reasons += if (chosen.isEmpty()) {
                "the owner taught that questions like this need no tool"
            } else {
                "the owner taught that questions like this use ${chosen.joinToString()}"
            }
            chosen.filter { it in available }
        } else {
            features.families.flatMap { it.tools }.distinct().filter { it in available }
        }
        if (taught == null && offered.isNotEmpty()) reasons += "the question calls for ${offered.joinToString()}"
        val reflex = reflexReady && offered.isNotEmpty() && features.reasoning < MAX_REASONING_FOR_REFLEX
        when {
            offered.isEmpty() -> Unit
            !reflexReady -> reasons += "Needle is off or not installed, so the chat model chooses the call"
            features.reasoning >= MAX_REASONING_FOR_REFLEX ->
                reasons += "the question reads as reasoning, so the chat model chooses the call"
            else -> reasons += "Needle tries the call first"
        }
        return MistyRoute(tools = offered, reflex = reflex, reasons = reasons)
    }
}
