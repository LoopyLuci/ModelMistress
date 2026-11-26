package com.modelmistress.core.tools

import com.modelmistress.core.chat.ChatBackend
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.Role
import com.modelmistress.core.chat.TokenSink

/**
 * What the screen shows while a reply uses tools. Tool use is not saved with the chat; only the final answer is.
 */
interface ToolListener {
    /** The step's reply text so far, tool blocks included. Show [ToolProtocol.visibleText] of it. */
    fun onStepText(stepText: String) {}

    fun onToolStart(call: ToolCall) {}

    fun onToolDone(call: ToolCall, outcome: ToolOutcome) {}
}

/** The answer to one user turn and how many tool calls it took. */
data class ToolRun(val answer: String, val toolCalls: Int)

/**
 * Answers one user turn. The model may call a tool. The call goes through the policy gate, its result goes back to the
 * model as data, and the model writes again. After [maxCalls] calls, the next answer is written without tools.
 */
class ToolLoop(
    private val toolbox: ToolSource,
    private val policies: ToolPolicies = ToolPolicies(),
    private val maxCalls: Int = DEFAULT_MAX_CALLS,
) {
    init {
        require(maxCalls >= 0) { "maxCalls must not be negative." }
    }

    /**
     * [keepGoing] is checked while text streams; false stops the reply. [confirm] answers for tools whose policy is ASK.
     */
    fun run(
        backend: ChatBackend,
        history: List<ChatMessage>,
        options: GenerationOptions,
        keepGoing: () -> Boolean,
        confirm: (ToolCall) -> Boolean,
        listener: ToolListener,
    ): ToolRun {
        val offered = toolbox.specs.filter { policies.of(it.toolClass) != ToolPolicy.DENY }
        val instruction = if (offered.isEmpty()) null else ChatMessage(Role.SYSTEM, ToolProtocol.instruction(offered))
        val context = history.toMutableList()
        val declined = mutableSetOf<String>()
        var calls = 0
        while (true) {
            if (!keepGoing()) return ToolRun("", calls)
            val system: ChatMessage? = instruction?.takeIf { calls < maxCalls }
            val messages = if (system != null) listOf(system) + context else context
            val step = StringBuilder()
            backend.complete(messages, options, TokenSink { piece ->
                step.append(piece)
                listener.onStepText(step.toString())
                keepGoing()
            })
            val text = step.toString()
            val parsed = if (system != null) ToolProtocol.parse(text) else null
            if (parsed == null) return ToolRun(ToolProtocol.visibleText(text), calls)
            calls++
            listener.onToolStart(parsed.call)
            val outcome = execute(parsed.call, confirm, declined)
            listener.onToolDone(parsed.call, outcome)
            context += ChatMessage(Role.ASSISTANT, parsed.raw)
            context += ChatMessage(Role.USER, ToolProtocol.resultBlock(parsed.call.name, outcome))
        }
    }

    /**
     * Runs one call through the policy. A tool the owner refused once in this reply is refused again without asking, so
     * the owner is not asked the same question over and over.
     */
    private fun execute(call: ToolCall, confirm: (ToolCall) -> Boolean, declined: MutableSet<String>): ToolOutcome {
        val spec = toolbox.spec(call.name) ?: return ToolOutcome.Refused("there is no tool called ${call.name}")
        return when (policies.of(spec.toolClass)) {
            ToolPolicy.DENY -> ToolOutcome.Refused("${call.name} is turned off")
            ToolPolicy.ASK -> when {
                call.name in declined -> ToolOutcome.Refused(declinedReason(call.name))
                confirm(call) -> toolbox.run(call)
                else -> {
                    declined += call.name
                    ToolOutcome.Refused(declinedReason(call.name))
                }
            }
            ToolPolicy.AUTO -> toolbox.run(call)
        }
    }

    private fun declinedReason(name: String): String =
        "the owner did not allow $name. Do not try it again in this reply; tell the owner it was not done."

    companion object {
        const val DEFAULT_MAX_CALLS = 3
    }
}
