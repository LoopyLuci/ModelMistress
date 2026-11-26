package com.modelmistress.core.misty

import com.modelmistress.core.json.Json

/**
 * Turns the owner's verdicts into training data: one chat example per line, as JSON, in the messages format that chat
 * fine-tuning tools read. Only answers the owner accepted ([FeedbackLabel.GOOD]) and corrections the owner gave by voice
 * ([FeedbackLabel.CORRECTED]) are exported. The routing verdicts are not answers, so they stay on the device.
 *
 * A corrected example's assistant message is the correction; the reply the owner corrected is kept under `metadata.rejected`,
 * so both halves can be used. What the camera showed is added to the user's message, as the model was given it.
 */
object MistyTraining {
    /** The examples that become training data, in the order given. */
    fun examples(all: List<FeedbackExample>): List<FeedbackExample> = all.filter { example ->
        example.label == FeedbackLabel.GOOD ||
            (example.label == FeedbackLabel.CORRECTED && !example.correction.isNullOrBlank())
    }

    /** One JSON object per training example. Empty when there is nothing to train on. */
    fun lines(all: List<FeedbackExample>): List<String> = examples(all).map { example ->
        val corrected = example.label == FeedbackLabel.CORRECTED
        val question = listOfNotNull(example.question, example.camera).joinToString("\n\n")
        val answer = if (corrected) example.correction.orEmpty() else example.answer
        val metadata = linkedMapOf<String, Any>(
            "label" to example.label.wire,
            "tools" to example.tools,
            "calls" to example.calls,
            "at" to example.at,
        )
        if (corrected) metadata["rejected"] = example.answer
        Json.write(
            mapOf(
                "messages" to listOf(
                    mapOf("role" to "user", "content" to question),
                    mapOf("role" to "assistant", "content" to answer),
                ),
                "metadata" to metadata,
            ),
        )
    }
}
