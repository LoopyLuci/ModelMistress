package com.modelmistress.core.misty

import com.modelmistress.core.chat.ChatBackend
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.Role
import com.modelmistress.core.tools.ScopedToolSource
import com.modelmistress.core.tools.ToolCall
import com.modelmistress.core.tools.ToolListener
import com.modelmistress.core.tools.ToolLoop
import com.modelmistress.core.tools.ToolPolicies
import com.modelmistress.core.tools.ToolRun
import com.modelmistress.core.tools.ToolSource

/** One answered turn: the reply, how many tool calls it took, and the route the question was given. */
data class MistyTurnResult(
    val answer: String,
    val toolCalls: Int,
    val route: MistyRoute,
    /** True when the reply's claim of a file change had no tool behind it, so [answer] replaces what was streamed. */
    val replaced: Boolean = false,
)

/**
 * Answers one owner turn. The router picks the tools the question needs; only those are offered, so a greeting is answered
 * with no tool in sight. The tool loop then runs any call through the policy gate.
 *
 * [persona] is true for Misty, which adds her instructions and the Needle reflex. The plain chat passes false and gets the
 * same routing and tools without them.
 */
class MistyTurn(
    private val chat: ChatBackend,
    private val tools: ToolSource,
    private val policies: ToolPolicies,
    private val reflex: MistyReflex?,
    private val feedback: MistyFeedback?,
    private val persona: Boolean,
    private val notes: (String) -> Unit,
) {
    /**
     * Answers [history], whose last user message is the question. [question] overrides that message for routing, so text
     * the owner attached (a document or a photo) does not change which tools the question is offered. [confirm] answers
     * for the tools whose policy is ASK.
     */
    fun run(
        history: List<ChatMessage>,
        options: GenerationOptions,
        keepGoing: () -> Boolean,
        confirm: (ToolCall) -> Boolean,
        listener: ToolListener,
        question: String? = null,
    ): MistyTurnResult {
        val routed = question ?: history.lastOrNull { it.role == Role.USER }?.content.orEmpty()
        val route = MistyRouter.route(
            question = routed,
            available = tools.specs.map { it.name }.toSet(),
            examples = feedback?.examples().orEmpty(),
            reflexReady = reflex != null,
        )
        route.reasons.forEach { notes("Misty: $it") }
        val offered = ScopedToolSource(tools, route.tools)
        val backend: ChatBackend = if (persona) {
            MistyBackend(chat, offered, reflex.takeIf { route.reflex }, notes, question = routed)
        } else {
            chat
        }
        val run: ToolRun = ToolLoop(offered, policies).run(backend, history, options, keepGoing, confirm, listener)
        if (run.toolCalls == 0 && claimsFileChange(run.answer)) {
            notes("Misty: the reply said a file changed, but no file tool ran; the reply was replaced.")
            return MistyTurnResult(NO_FILE_REPLY, 0, route, replaced = true)
        }
        return MistyTurnResult(run.answer, run.toolCalls, route)
    }

    companion object {
        /** The reply that stands in for a file claim no tool backed. */
        const val NO_FILE_REPLY = "No file tool ran for this request, so no file was changed. Name the file and what should go in it."

        private const val PAST_VERB = """(saved|created|wrote|written|updated|replaced|changed|edited|renamed|deleted)"""
        private const val FILE_NAME = """\.(txt|md|csv|json|log)\b"""

        /** A past-tense claim that a file was saved, written or changed, with a file name before or after the verb. */
        private val FILE_CHANGE_CLAIM = Regex(
            """\b$PAST_VERB\b[^\n]{0,80}$FILE_NAME|$FILE_NAME[^\n]{0,40}\b$PAST_VERB\b""",
            RegexOption.IGNORE_CASE,
        )

        /** True when [answer] says a file was saved, written or changed. An offer such as "I can save it" is not a claim. */
        fun claimsFileChange(answer: String): Boolean = FILE_CHANGE_CLAIM.containsMatchIn(answer)
    }
}
