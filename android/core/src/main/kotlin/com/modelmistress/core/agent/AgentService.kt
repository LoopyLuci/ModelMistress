package com.modelmistress.core.agent

import com.modelmistress.core.chat.ApiException
import com.modelmistress.core.chat.ChatBackend
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.ChatStore
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.Role
import com.modelmistress.core.chat.SavedChat
import com.modelmistress.core.environment.EnvironmentStore
import com.modelmistress.core.misty.FeedbackExample
import com.modelmistress.core.misty.FeedbackLabel
import com.modelmistress.core.misty.MistyFeedback
import com.modelmistress.core.misty.MistyReflex
import com.modelmistress.core.misty.MistyToolbox
import com.modelmistress.core.misty.MistyTurn
import com.modelmistress.core.tools.Steering
import com.modelmistress.core.tools.ToolCall
import com.modelmistress.core.tools.ToolClass
import com.modelmistress.core.tools.ToolListener
import com.modelmistress.core.tools.ToolModes
import com.modelmistress.core.tools.ToolOutcome
import com.modelmistress.core.tools.ToolPolicies
import com.modelmistress.core.tools.ToolPolicy
import com.modelmistress.core.tools.ToolProtocol
import com.modelmistress.core.tools.ToolSource
import com.modelmistress.core.tools.ToolSpec
import com.modelmistress.core.tools.ToolUse
import com.modelmistress.core.tools.Toolbox
import com.modelmistress.core.tools.FileWrites
import com.modelmistress.core.tools.Workspace

/** The owner's settings that decide what a turn may do. They are read again for every turn. */
data class AgentSettings(
    val toolUse: ToolUse,
    val fileWrites: FileWrites,
    val mistyEnabled: Boolean,
)

/**
 * One request to answer. [model] answers; [modelId] names it in the saved chat. [chatId] is the conversation the request belongs
 * to: its sandbox and its history are kept under that id. [misty] asks for Misty's instructions and routing; it needs Misty on.
 * [toolsAllowed] false offers no tool at all for this request. A request can narrow what the owner allowed, never widen it.
 */
class AgentRequest(
    val modelId: String,
    val model: ChatBackend,
    val messages: List<ChatMessage>,
    val options: GenerationOptions,
    val client: String,
    val chatId: String? = null,
    val sandboxId: String? = null,
    val misty: Boolean = false,
    val toolsAllowed: Boolean = true,
)

/** What a reply reports while it is written, in order. */
sealed interface AgentEvent {
    /** The next piece of the reply's text. */
    data class Text(val piece: String) : AgentEvent

    /** A tool started running. */
    data class ToolStarted(val name: String) : AgentEvent

    /** A tool finished. [outcome] is `ran`, `refused` or `failed`. */
    data class ToolFinished(val name: String, val outcome: String) : AgentEvent

    /** A line from Misty about how she answered, such as the route she chose. */
    data class Note(val line: String) : AgentEvent

    /** The streamed text was replaced by [text], because it claimed a file change that no tool made. */
    data class Replaced(val text: String) : AgentEvent
}

/** The finished reply. [calls] says what each tool call did, as `name:outcome`, and [offered] lists the tools it was offered. */
data class AgentResult(
    val chatId: String?,
    val answer: String,
    val toolCalls: Int,
    val misty: Boolean,
    val offered: List<String>,
    val calls: List<String>,
)

/**
 * Answers requests the way the phone's chat does, for the router's clients: the owner's tool policy, the chat's sandbox, the
 * device's own tools, and Misty when she is on. A tool that needs the owner's OK asks through [approvals].
 *
 * The chat is saved under [AgentRequest.chatId] after each reply, so its history and sandbox survive. Ratings of a reply go to
 * [feedback] through [rate].
 */
class AgentService(
    private val chats: ChatStore,
    private val environments: EnvironmentStore,
    private val feedback: MistyFeedback,
    private val approvals: Approvals,
    private val settings: () -> AgentSettings,
    private val reflex: () -> MistyReflex?,
    private val deviceStatus: () -> String,
    private val installedModels: () -> String,
    private val now: () -> Long = System::currentTimeMillis,
) {
    /** The offered tools and calls of the last reply in each chat, for a rating that comes after it. Bounded. */
    private val lastReplies = object : LinkedHashMap<String, LastReply>(MAX_REMEMBERED, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, LastReply>?): Boolean = size > MAX_REMEMBERED
    }

    private class LastReply(val offered: List<String>, val calls: List<String>)

    /**
     * Answers [request], passing each event to [sink] as it happens. [sink] returns false when the caller has gone, and the reply
     * then stops. Throws [ApiException] when Misty is asked for but is off.
     */
    fun run(request: AgentRequest, sink: (AgentEvent) -> Boolean): AgentResult {
        preflight(request)
        val current = settings()
        val saved = request.chatId?.let { chats.load(it) }
        val sandboxId = sandboxFor(saved, request.chatId, request.sandboxId)
        val workspace = environments.workspace(sandboxId)
            ?: throw IllegalStateException("The chat's sandbox could not be opened.")

        // Misty answers when she is asked for, and in a chat while the owner has her on. A plain request gets none of her.
        val persona = request.misty || (request.chatId != null && current.mistyEnabled)
        val writable = ToolModes.writesOffered(current.toolUse, current.fileWrites)
        val policies = if (request.toolsAllowed) {
            ToolModes.policies(current.toolUse, current.fileWrites)
        } else {
            ToolPolicies(ToolClass.values().associateWith { ToolPolicy.DENY })
        }
        val tools: ToolSource = if (!request.toolsAllowed) {
            NoTools
        } else {
            val base = Toolbox(workspace, writable = writable)
            if (persona) MistyToolbox(base, deviceStatus, installedModels) else base
        }
        val reflexRunner = if (persona && request.toolsAllowed && current.toolUse != ToolUse.OFF) reflex() else null

        var open = true
        var shown = "" // the visible text of the current step that has already been sent
        val calls = mutableListOf<String>()
        val listener = object : ToolListener {
            override fun onStepText(stepText: String) {
                val visible = ToolProtocol.visibleText(stepText)
                if (!visible.startsWith(shown)) return
                val piece = visible.substring(shown.length)
                shown = visible
                if (piece.isNotEmpty() && open) open = sink(AgentEvent.Text(piece))
            }

            override fun onToolStart(call: ToolCall) {
                shown = ""
                if (open) open = sink(AgentEvent.ToolStarted(call.name))
            }

            override fun onToolDone(call: ToolCall, outcome: ToolOutcome) {
                calls += "${call.name}:${outcomeWord(outcome)}"
                if (open) open = sink(AgentEvent.ToolFinished(call.name, outcomeWord(outcome)))
            }
        }
        val turn = MistyTurn(
            chat = request.model,
            tools = tools,
            policies = policies,
            reflex = reflexRunner,
            feedback = feedback,
            persona = persona,
            notes = { line -> if (open) open = sink(AgentEvent.Note(line)) },
        )
        val turned = turn.run(
            history = request.messages,
            options = request.options,
            keepGoing = { open },
            confirm = { call -> approvals.ask(request.client, "Allow ${call.name}?", describe(call)) },
            listener = listener,
            steering = Steering.NONE,
        )
        if (turned.replaced) {
            open = open && sink(AgentEvent.Replaced(turned.answer))
        } else if (turned.answer.startsWith(shown) && turned.answer.length > shown.length && open) {
            sink(AgentEvent.Text(turned.answer.substring(shown.length)))
        }

        val offered = turned.route.tools
        request.chatId?.let { chatId ->
            val messages = request.messages + ChatMessage(Role.ASSISTANT, turned.answer)
            chats.save(
                SavedChat(
                    id = chatId,
                    title = SavedChat.titleFor(messages),
                    modelId = request.modelId,
                    createdAt = saved?.createdAt ?: now(),
                    updatedAt = now(),
                    messages = messages,
                    environmentId = sandboxId,
                ),
            )
            synchronized(lastReplies) { lastReplies[chatId] = LastReply(offered, calls) }
        }
        return AgentResult(
            chatId = request.chatId,
            answer = turned.answer,
            toolCalls = turned.toolCalls,
            misty = persona,
            offered = offered,
            calls = calls,
        )
    }

    /**
     * Checks what [run] would refuse, before any text is written, so the caller can answer with the right status. Throws
     * [ApiException] when Misty is asked for but is off, or when the named sandbox does not exist, and
     * [IllegalArgumentException] for a chat id that cannot be kept.
     */
    fun preflight(request: AgentRequest) {
        if (request.misty && !settings().mistyEnabled) {
            throw ApiException(409, "misty_disabled", "Misty is off on this device. Turn her on in the router's settings.")
        }
        request.chatId?.let { require(SavedChat.ID_PATTERN.matches(it)) { "A chat id uses 1 to 64 letters, digits, '.', '_' or '-'." } }
        request.sandboxId?.let { id ->
            if (environments.get(id) == null) throw ApiException(404, "sandbox_not_found", "There is no sandbox '$id'.")
        }
    }

    /**
     * Saves the owner's verdict on the last reply in [chatId]. [tools] are the tools the owner says were needed, for
     * [FeedbackLabel.NEEDS_TOOLS]. [correction] is the right answer, for [FeedbackLabel.CORRECTED]. False when the chat is not
     * found or has no reply yet.
     */
    fun rate(
        chatId: String,
        label: FeedbackLabel,
        tools: List<String> = emptyList(),
        correction: String? = null,
    ): Boolean {
        val chat = chats.load(chatId) ?: return false
        val answer = chat.messages.lastOrNull { it.role == Role.ASSISTANT }?.content ?: return false
        val question = chat.messages.lastOrNull { it.role == Role.USER }?.content ?: return false
        val last = synchronized(lastReplies) { lastReplies[chatId] }
        feedback.add(
            FeedbackExample(
                question = question,
                label = label,
                tools = if (label == FeedbackLabel.NEEDS_TOOLS) tools else last?.offered.orEmpty(),
                calls = last?.calls.orEmpty(),
                answer = answer,
                at = now(),
                correction = correction?.takeIf { label == FeedbackLabel.CORRECTED && it.isNotBlank() },
            ),
        )
        return true
    }

    /**
     * The sandbox a request works in. A chat keeps its own, which is made on its first reply unless the request names an existing
     * sandbox for it. A request without a chat uses the named sandbox, or else the older shared files.
     */
    private fun sandboxFor(saved: SavedChat?, chatId: String?, requested: String?): String {
        val named = requested?.let { id ->
            if (environments.get(id) == null) throw ApiException(404, "sandbox_not_found", "There is no sandbox '$id'.")
            id
        }
        if (chatId == null) return named ?: EnvironmentStore.DEFAULT_ID
        val existing = saved?.environmentId?.takeIf { environments.get(it) != null }
        return existing ?: named ?: environments.create("Chat $chatId").id
    }

    companion object {
        private const val MAX_REMEMBERED = 64

        /** What a tool outcome is called in a saved rating: `ran`, `refused` or `failed`. */
        fun outcomeWord(outcome: ToolOutcome): String = when (outcome) {
            is ToolOutcome.Ok -> "ran"
            is ToolOutcome.Refused -> "refused"
            is ToolOutcome.Failed -> "failed"
        }

        /** What the owner is asked about a tool call: the tool and its arguments, as text. */
        fun describe(call: ToolCall): String =
            call.arguments.entries.joinToString(", ") { (key, value) -> "$key: ${value.take(MAX_ARGUMENT)}" }

        private const val MAX_ARGUMENT = 160
    }
}

/** Offers no tool at all. A request that turns tools off gets this. */
private object NoTools : ToolSource {
    override val specs: List<ToolSpec> = emptyList()
    override fun spec(name: String): ToolSpec? = null
    override fun run(call: ToolCall): ToolOutcome = ToolOutcome.Refused("there is no tool called ${call.name}")
}
