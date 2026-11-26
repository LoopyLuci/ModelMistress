package com.modelmistress.core.misty

import com.modelmistress.core.json.Json
import com.modelmistress.core.tools.ToolSpec

/** Proposes one tool call for a question. The Needle model writes the raw JSON that [MistyReflexProposal.parse] reads. */
fun interface MistyReflex {
    fun propose(input: String, toolsJson: String): String
}

/** A Needle proposal. [confidence] is the model's own score; a proposal below [MIN_CONFIDENCE] never runs. */
data class MistyReflexProposal(val name: String, val arguments: Map<String, String>, val confidence: Double) {
    companion object {
        /** The least confidence a proposal needs before it runs. */
        const val MIN_CONFIDENCE = 0.10

        /** Reads `{"function_calls":[{"name":...,"arguments":{...}}],"confidence":0.7}`. Anything else is null. */
        fun parse(raw: String): MistyReflexProposal? {
            val fields = runCatching { Json.parse(raw) as? Map<*, *> }.getOrNull() ?: return null
            val call = (fields["function_calls"] as? List<*>)?.firstOrNull() as? Map<*, *> ?: return null
            val name = (call["name"] as? String)?.trim()?.takeIf { it.isNotEmpty() } ?: return null
            val arguments = (call["arguments"] as? Map<*, *>).orEmpty().entries.associate { (key, value) ->
                key.toString() to (value?.toString() ?: "")
            }
            val confidence = (fields["confidence"] as? Number)?.toDouble() ?: 0.0
            return MistyReflexProposal(name, arguments, confidence)
        }
    }
}

/** The tool schemas in the JSON shape the Needle runtime reads: name, description, and an object of typed parameters. */
internal fun toolsJsonFor(specs: List<ToolSpec>): String = Json.write(
    specs.map { spec ->
        mapOf(
            "name" to spec.name,
            "description" to spec.description,
            "parameters" to mapOf(
                "type" to "object",
                "properties" to spec.parameters.associate { parameter ->
                    parameter.name to mapOf("type" to "string", "description" to parameter.description)
                },
                "required" to spec.parameters.filter { it.required }.map { it.name },
            ),
        )
    },
)
