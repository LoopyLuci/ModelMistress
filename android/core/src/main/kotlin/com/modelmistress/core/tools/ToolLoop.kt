package com.modelmistress.core.tools

import com.modelmistress.core.chat.ChatBackend
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.Role
import com.modelmistress.core.chat.TokenSink

/**
 * What the screen shows while a reply uses tools. Tool use is not saved with the chat; only the final answer is, plus any
 * owner notes that were taken in while the reply ran.
 */
interface ToolListener {
    /** The step's reply text so far, tool blocks included. Show [ToolProtocol.visibleText] of it. */
    fun onStepText(stepText: String) {}

    fun onToolStart(call: ToolCall) {}

    fun onToolDone(call: ToolCall, outcome: ToolOutcome) {}

    /**
     * The owner's notes were taken in at a step boundary. [partial] is the reply text written before the boundary, which stays
     * part of the answer. The notes come after it, in the order sent.
     */
    fun onSteered(partial: String, notes: List<String>) {}
}

/**
 * Owner messages sent while a reply runs. The loop takes them in at a step boundary: after a sentence ends, or after a tool
 * result. Nothing is cut off mid-word.
 */
interface Steering {
    /** True when at least one message is waiting. */
    fun pending(): Boolean

    /** Takes every waiting message, in the order it was sent. */
    fun take(): List<String>

    companion object {
        val NONE: Steering = object : Steering {
            override fun pending(): Boolean = false
            override fun take(): List<String> = emptyList()
        }
    }
}

/** The answer to one user turn and how many tool calls it took. */
data class ToolRun(val answer: String, val toolCalls: Int)

/**
 * Answers one user turn. The model may call a tool. The call goes through the policy gate, its result goes back to the
 * model as data, and the model writes again. After [maxCalls] calls, the next answer is written without tools.
 *
 * While a reply runs, owner notes from [steering] are added to the conversation at the next sentence end or tool result. The
 * reply carries on from there; it is not restarted.
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
        steering: Steering = Steering.NONE,
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
            var yielded = false
            backend.complete(messages, options, TokenSink { piece ->
                step.append(piece)
                listener.onStepText(step.toString())
                if (!keepGoing()) return@TokenSink false
                // A sentence has just ended and the owner has a note: stop here, keep the text, and take the note in.
                val boundary = steering.pending() && endsSentence(step) &&
                    (system == null || ToolProtocol.parse(step.toString()) == null)
                if (boundary) yielded = true
                !boundary
            })
            val text = step.toString()
            if (yielded) {
                context += ChatMessage(Role.ASSISTANT, text)
                takeSteering(steering, context, listener, ToolProtocol.visibleText(text))
                continue
            }
            val parsed = if (system != null) ToolProtocol.parse(text) else null
            if (parsed == null) return ToolRun(ToolProtocol.visibleText(text), calls)
            calls++
            listener.onToolStart(parsed.call)
            val outcome = execute(parsed.call, confirm, declined)
            listener.onToolDone(parsed.call, outcome)
            context += ChatMessage(Role.ASSISTANT, parsed.raw)
            context += ChatMessage(Role.USER, ToolProtocol.resultBlock(parsed.call.name, outcome))
            takeSteering(steering, context, listener, partial = "")
        }
    }

    /** Adds the waiting owner notes to the conversation, after what came before them, and tells the listener. */
    private fun takeSteering(steering: Steering, context: MutableList<ChatMessage>, listener: ToolListener, partial: String) {
        val notes = steering.take()
        if (notes.isEmpty()) return
        notes.forEach { note -> context += ChatMessage(Role.USER, steerBlock(note)) }
        listener.onSteered(partial, notes)
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

        /** True when the reply so far ends a sentence or a line, so it is a place where a note can be taken in. */
        fun endsSentence(text: CharSequence): Boolean {
            val last = text.lastOrNull { !it.isWhitespace() || it == '\n' } ?: return false
            return last == '\n' || last == '.' || last == '!' || last == '?'
        }

        /** The owner's note as the model reads it: marked as coming from the owner, while it was answering. */
        fun steerBlock(note: String): String =
            "The owner added this while you were answering. Take it into account from here on:\n$note"
    }
}
