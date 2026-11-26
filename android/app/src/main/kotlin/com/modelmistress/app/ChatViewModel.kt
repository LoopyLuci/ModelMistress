package com.modelmistress.app

import android.content.Context
import android.net.Uri
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import com.modelmistress.core.chat.ChatAttachment
import com.modelmistress.core.chat.ChatBackend
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.ChatStore
import com.modelmistress.core.chat.ChatSummary
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.Role
import com.modelmistress.core.chat.SavedChat
import com.modelmistress.core.environment.EnvironmentStore
import com.modelmistress.core.misty.FeedbackExample
import com.modelmistress.core.misty.FeedbackLabel
import com.modelmistress.core.misty.MistyLearning
import com.modelmistress.core.misty.MistyToolbox
import com.modelmistress.core.misty.MistyTurn
import com.modelmistress.core.tools.Steering
import com.modelmistress.core.tools.ToolCall
import com.modelmistress.core.tools.ToolListener
import com.modelmistress.core.tools.ToolModes
import com.modelmistress.core.tools.ToolOutcome
import com.modelmistress.core.tools.ToolPolicies
import com.modelmistress.core.tools.ToolProtocol
import com.modelmistress.core.tools.ToolUse
import com.modelmistress.core.tools.Toolbox
import com.modelmistress.core.tools.Workspace
import com.modelmistress.runtime.ModelRuntime
import com.modelmistress.runtime.NeedleReflex
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ConcurrentLinkedQueue
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** A reply the owner can rate: its question, the tools the router offered, what each call did, and the reply text. */
data class RateableReply(
    val question: String,
    val offered: List<String>,
    val calls: List<String>,
    val answer: String,
)

/** A message the owner queued to send after the reply that is running. */
data class QueuedMessage(val text: String, val attachments: List<ChatAttachment>)

/** A file in the chat's sandbox, for the files panel. */
data class SandboxFile(val name: String, val bytes: Long)

/** A sandbox the chat can link to, with the chat that works in it when there is one. */
data class SandboxLink(val id: String, val name: String, val chat: String?)

/** What the files panel shows: this chat's sandbox, its files, and the sandboxes it is linked to or could be. */
data class SandboxView(
    val environmentId: String?,
    val name: String,
    val files: List<SandboxFile>,
    val linked: List<SandboxLink>,
    val linkable: List<SandboxLink>,
)

data class ChatUiState(
    val messages: List<ChatMessage> = emptyList(),
    val streaming: String? = null,
    val draft: String = "",
    val models: List<String> = emptyList(),
    val selectedId: String? = null,
    val busy: Boolean = false,
    val error: String? = null,
    val history: List<ChatSummary> = emptyList(),
    val historyOpen: Boolean = false,
    /** One line per tool the current reply used. Not saved with the chat. */
    val toolNotes: List<String> = emptyList(),
    /** A tool call waiting for the owner's OK, described for the dialog. Null when none is waiting. */
    val pendingCall: String? = null,
    /** The last reply, while the owner can still rate it. Null when there is none to rate. */
    val rateable: RateableReply? = null,
    /** Says that a rating was saved. Cleared when the next reply starts. */
    val feedbackStatus: String? = null,
    /** Files and photos waiting to go with the next message. */
    val attachments: List<ChatAttachment> = emptyList(),
    /** True while a picked file or photo is being read and kept. */
    val attaching: Boolean = false,
    /** Messages queued to send, in order, after the reply that is running. */
    val queue: List<QueuedMessage> = emptyList(),
    /** Notes sent while the reply runs, waiting to be taken in at its next step. */
    val steers: List<String> = emptyList(),
    /** The folder on this phone where the chat's sandbox keeps its files. Null until the chat has one. */
    val sandboxDir: String? = null,
    val filesOpen: Boolean = false,
    val sandbox: SandboxView? = null,
    val sandboxStatus: String? = null,
)

private const val DEFAULT_QUESTION = "Read the attachments and tell me what they say."

/** How much of an earlier reply a correction note keeps for the model. */
private const val CORRECTION_NOTE_CHARS = 300

private fun describePendingCall(call: ToolCall): String {
    val details = call.arguments.entries.joinToString("\n") { (key, value) -> "$key: ${value.take(400)}" }
    return "Misty wants to run ${call.name}.\n$details"
}

private fun toolNote(call: ToolCall, outcome: ToolOutcome): String {
    val what = call.arguments.values.joinToString(", ").take(60)
    val label = if (what.isEmpty()) call.name else "${call.name}($what)"
    return when (outcome) {
        is ToolOutcome.Ok -> "$label: ${outcome.text.replace('\n', ' ').take(160)}"
        is ToolOutcome.Refused -> "$label not run: ${outcome.reason}"
        is ToolOutcome.Failed -> "$label failed: ${outcome.reason}"
    }
}

private fun outcomeWord(outcome: ToolOutcome): String = when (outcome) {
    is ToolOutcome.Ok -> "ran"
    is ToolOutcome.Refused -> "refused"
    is ToolOutcome.Failed -> "failed"
}

/**
 * The chat on this device. Each conversation is saved on this device after every reply, stopped reply or failed one, so
 * it is still there after the app closes. Each chat works in its own sandbox: a folder on this phone that its tools and its
 * attachments use. The app opens on a new chat; older ones are opened from the history list.
 */
class ChatViewModel(private val runtime: ModelRuntime) : ViewModel() {
    private val state = MutableStateFlow(ChatUiState())
    val uiState: StateFlow<ChatUiState> = state.asStateFlow()
    private val store: ChatStore = runtime.chats
    private var reply: Job? = null
    private var chatId = SavedChat.newId()
    private var chatCreatedAt = System.currentTimeMillis()

    /** The sandbox this chat works in. Null until the chat needs one, which is at its first message or file. */
    @Volatile
    private var environmentId: String? = null

    /** Notes the owner sent while a reply runs. The reply's thread takes them in at a step boundary. */
    private val steerInbox = ConcurrentLinkedQueue<String>()

    private val steering = object : Steering {
        override fun pending(): Boolean = steerInbox.isNotEmpty()
        override fun take(): List<String> = generateSequence { steerInbox.poll() }.toList()
    }

    /** The owner's answer the reply thread is waiting for, while a Misty tool call needs approval. */
    @Volatile
    private var pendingAnswer: CompletableFuture<Boolean>? = null

    init {
        refreshModels()
        refreshHistory()
    }

    fun refreshModels() {
        viewModelScope.launch {
            val ids = withContext(Dispatchers.IO) { runtime.catalog.models().map { it.id } }
            state.update { current ->
                val selected = listOfNotNull(current.selectedId, runtime.settings.defaultModelId, ids.firstOrNull())
                    .firstOrNull { it in ids }
                current.copy(models = ids, selectedId = selected)
            }
        }
    }

    fun selectModel(id: String) {
        state.update { it.copy(selectedId = id) }
    }

    fun setDraft(text: String) {
        state.update { it.copy(draft = text) }
    }

    /** Shows a message the screen could not act on, such as a missing camera or speech app. */
    fun reportError(message: String) {
        state.update { it.copy(error = message) }
    }

    fun toggleHistory() {
        val opening = !state.value.historyOpen
        state.update { it.copy(historyOpen = opening) }
        if (opening) refreshHistory()
    }

    fun refreshHistory() {
        viewModelScope.launch {
            val list = withContext(Dispatchers.IO) { store.list() }
            state.update { it.copy(history = list) }
        }
    }

    /** Starts an empty conversation with a sandbox of its own. The sandbox is made when the chat first needs it. */
    fun newChat() {
        if (state.value.busy) return
        startFreshConversation()
        state.update {
            it.copy(
                messages = emptyList(),
                draft = "",
                error = null,
                historyOpen = false,
                rateable = null,
                feedbackStatus = null,
                attachments = emptyList(),
                queue = emptyList(),
                sandboxDir = null,
                sandbox = null,
                sandboxStatus = null,
            )
        }
        if (state.value.filesOpen) refreshSandbox()
    }

    /** Opens a saved conversation, with its sandbox, and selects the model it was last answered with when that model is still here. */
    fun openChat(id: String) {
        if (state.value.busy) return
        viewModelScope.launch {
            val chat = withContext(Dispatchers.IO) { store.load(id) }
            if (chat == null) {
                state.update { it.copy(error = "That chat could not be opened.") }
                refreshHistory()
                return@launch
            }
            chatId = chat.id
            chatCreatedAt = chat.createdAt
            // A chat saved before sandboxes existed works in the older files, which were the only files there were.
            environmentId = chat.environmentId ?: EnvironmentStore.DEFAULT_ID
            val dir = withContext(Dispatchers.IO) { sandboxDirOf(environmentId) }
            state.update { current ->
                current.copy(
                    messages = chat.messages,
                    draft = "",
                    error = null,
                    historyOpen = false,
                    selectedId = chat.modelId?.takeIf { it in current.models } ?: current.selectedId,
                    rateable = null,
                    feedbackStatus = null,
                    attachments = emptyList(),
                    queue = emptyList(),
                    sandboxDir = dir,
                    sandboxStatus = null,
                )
            }
            if (state.value.filesOpen) refreshSandbox()
        }
    }

    /** Deletes a saved conversation. Deleting the one on screen clears the screen too. Its sandbox stays, in the files panel. */
    fun deleteChat(id: String) {
        if (state.value.busy) return
        viewModelScope.launch {
            withContext(Dispatchers.IO) { store.delete(id) }
            if (id == chatId) {
                startFreshConversation()
                state.update {
                    it.copy(messages = emptyList(), draft = "", error = null, rateable = null, feedbackStatus = null, queue = emptyList(), sandboxDir = null)
                }
            }
            refreshHistory()
        }
    }

    /** Adds text to the message box, after what is there, for example words the owner dictated. */
    fun appendToDraft(text: String) {
        state.update { current ->
            val joined = listOf(current.draft.trim(), text.trim()).filter { it.isNotEmpty() }.joinToString(" ")
            current.copy(draft = joined)
        }
    }

    /** Copies the picked documents and pictures into the chat's sandbox and reads them. Each one that fails is named in the error. */
    fun attachDocuments(context: Context, uris: List<Uri>) {
        if (uris.isEmpty()) return
        viewModelScope.launch {
            state.update { it.copy(attaching = true, error = null) }
            val results = withContext(Dispatchers.IO) {
                uris.map { uri -> runCatching { storeAttachment(context, uri) } }
            }
            addResults(results)
        }
    }

    /** Keeps a photo the camera took in the chat's sandbox, reads it, and adds it to the next message. */
    fun attachPhoto(file: File) {
        viewModelScope.launch {
            state.update { it.copy(attaching = true, error = null) }
            // The camera's copy is only needed to keep and read the photo, so it is removed afterwards.
            val result = withContext(Dispatchers.IO) {
                runCatching {
                    val workspace = sandboxWorkspace()
                    val name = "Photo " + SimpleDateFormat("yyyy-MM-dd HH.mm.ss", Locale.US).format(Date()) + ".jpg"
                    val stored = file.inputStream().use { workspace.importFile(name, it, Attachments.MAX_FILE_BYTES) }
                    readStored(workspace, stored, "Photo", "image/jpeg")
                }.also { file.delete() }
            }
            addResults(listOf(result))
        }
    }

    /** Copies a picked file into the chat's sandbox and reads it for the model. The file stays in the sandbox either way. */
    private fun storeAttachment(context: Context, uri: Uri): ChatAttachment {
        val name = Attachments.nameOf(context, uri)
        val mime = context.contentResolver.getType(uri).orEmpty()
        if (!Attachments.supports(name, mime)) {
            throw IllegalArgumentException("$name is not supported yet. Text files, PDFs and pictures can be attached.")
        }
        val workspace = sandboxWorkspace()
        val input = context.contentResolver.openInputStream(uri) ?: throw IllegalArgumentException("$name could not be opened.")
        val stored = input.use { workspace.importFile(name, it, Attachments.MAX_FILE_BYTES) }
        return readStored(workspace, stored, name, mime)
    }

    /** Reads a file already in the sandbox. A file that cannot be read is removed again, so nothing unreadable is kept. */
    private fun readStored(workspace: Workspace, stored: String, label: String, mime: String): ChatAttachment {
        val file = workspace.resolve(stored)
        return try {
            Attachments.read(file, label, mime)
        } catch (error: Exception) {
            file.delete()
            throw error
        }
    }

    private fun addResults(results: List<Result<ChatAttachment>>) {
        val added = results.mapNotNull { it.getOrNull() }
        val errors = results.mapNotNull { it.exceptionOrNull()?.message }
        state.update {
            it.copy(
                attaching = false,
                attachments = it.attachments + added,
                error = errors.joinToString("\n").ifEmpty { null },
            )
        }
    }

    fun removeAttachment(index: Int) {
        state.update { current ->
            current.copy(attachments = current.attachments.filterIndexed { i, _ -> i != index })
        }
    }

    /**
     * Sends the message in the box, or [typed] when given, with [note] added for the model only. A note is not saved with the
     * chat. Nothing is sent while a reply runs.
     */
    fun send(typed: String? = null, note: String? = null) {
        val current = state.value
        if (current.busy || current.attaching) return
        val modelId = current.selectedId ?: return
        val fromDraft = typed == null
        val text = (typed ?: current.draft).trim()
        val attached = if (fromDraft) current.attachments else emptyList()
        if (text.isEmpty() && attached.isEmpty()) return
        startTurn(modelId, text, attached, note, clearDraft = fromDraft)
    }

    /** Holds the message in the box to send once the reply that is running has finished. Sends at once when nothing runs. */
    fun queueDraft() {
        val current = state.value
        if (!current.busy) {
            send()
            return
        }
        val text = current.draft.trim()
        if (text.isEmpty() && current.attachments.isEmpty()) return
        state.update {
            it.copy(queue = it.queue + QueuedMessage(text, it.attachments), draft = "", attachments = emptyList())
        }
    }

    /**
     * Gives the note in the box to the reply that is running. It is taken in at the next sentence end or tool step, without
     * stopping the reply. Sends at once when nothing runs.
     */
    fun steerDraft() {
        val current = state.value
        val text = current.draft.trim()
        if (text.isEmpty()) return
        if (!current.busy) {
            send()
            return
        }
        steerInbox.add(text)
        state.update { it.copy(steers = it.steers + text, draft = "") }
    }

    fun removeQueued(index: Int) {
        state.update { current -> current.copy(queue = current.queue.filterIndexed { i, _ -> i != index }) }
    }

    fun clearQueue() {
        state.update { it.copy(queue = emptyList()) }
    }

    private fun startTurn(modelId: String, typed: String, attached: List<ChatAttachment>, note: String?, clearDraft: Boolean) {
        val text = typed.ifEmpty { DEFAULT_QUESTION }
        val history = state.value.messages + ChatMessage(Role.USER, text, attached)
        val id = chatId
        val created = chatCreatedAt
        state.update {
            it.copy(
                messages = history,
                draft = if (clearDraft) "" else it.draft,
                attachments = if (clearDraft) emptyList() else it.attachments,
                busy = true,
                error = null,
                streaming = "",
                toolNotes = emptyList(),
                rateable = null,
                feedbackStatus = null,
                steers = emptyList(),
            )
        }
        reply = viewModelScope.launch {
            val partial = StringBuilder()
            val calls = mutableListOf<String>()
            // What the reply added while it ran: the text written before each note, and the notes in the order sent.
            val added = mutableListOf<ChatMessage>()
            var rateable: RateableReply? = null
            var failure: String? = null
            try {
                withContext(Dispatchers.IO) {
                    val workspace = sandboxWorkspace()
                    val backend = runtime.catalog.backend(modelId)
                        ?: throw IllegalStateException("That model is no longer installed.")
                    val options = GenerationOptions(maxTokens = 512, temperature = 0.7f)
                    val result = turnFor(backend, workspace).run(
                        history = promptFor(history, noteWith(text, note)),
                        options = options,
                        keepGoing = { isActive },
                        confirm = { call -> askOwner(call) },
                        listener = replyListener(partial, calls, history, added),
                        question = text,
                        steering = steering,
                    )
                    if (result.replaced) {
                        // The streamed text claimed a file change no tool made; the saved reply is the corrected one.
                        partial.setLength(0)
                        partial.append(result.answer)
                    }
                    if (isActive && result.answer.isNotBlank()) {
                        rateable = RateableReply(text, result.route.tools, calls.toList(), result.answer)
                    }
                }
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                failure = error.message ?: "The reply failed."
            } finally {
                val tail = if (partial.isEmpty()) emptyList() else listOf(ChatMessage(Role.ASSISTANT, partial.toString()))
                val kept = history + added + tail
                // Notes that came too late for a step are not lost: they are sent next, ahead of the queue.
                val leftover = steering.take()
                state.update {
                    it.copy(messages = kept, streaming = null, busy = false, error = failure, rateable = rateable, steers = emptyList())
                }
                // Kept even when the reply was stopped or failed, so what was said is still in the history.
                val saved = withContext(NonCancellable + Dispatchers.IO) {
                    runCatching {
                        store.save(SavedChat(id, SavedChat.titleFor(kept), modelId, created, System.currentTimeMillis(), kept, environmentId))
                        store.list()
                    }
                }
                saved.onSuccess { list -> state.update { it.copy(history = list) } }
                    .onFailure { error ->
                        state.update { it.copy(error = "The chat could not be saved: ${error.message}") }
                    }
                // After a failed reply the queue waits, so the owner sees the error first. After a finished or stopped one it goes on.
                if (failure == null) {
                    continueQueue(leftover)
                } else {
                    state.update { current ->
                        current.copy(queue = leftover.map { QueuedMessage(it, emptyList()) } + current.queue)
                    }
                }
            }
        }
    }

    /** Sends the next queued message, after any notes that came too late. Nothing is sent when the queue is empty. */
    private fun continueQueue(leftover: List<String>) {
        val items = leftover.map { QueuedMessage(it, emptyList()) } + state.value.queue
        val next = items.firstOrNull() ?: return
        state.update { it.copy(queue = items.drop(1)) }
        val modelId = state.value.selectedId ?: return
        startTurn(modelId, next.text, next.attachments, note = null, clearDraft = false)
    }

    /**
     * Saves the owner's verdict on the last reply. [tools] are the tools the owner says the reply should have had, and are
     * used only for [FeedbackLabel.NEEDS_TOOLS]. Misty offers those tools for similar questions from now on.
     */
    fun rateReply(label: FeedbackLabel, tools: List<String> = emptyList()) {
        val reply = state.value.rateable ?: return
        val kept = when (label) {
            FeedbackLabel.GOOD -> reply.offered
            FeedbackLabel.NEEDS_TOOLS -> tools
            FeedbackLabel.NO_TOOLS, FeedbackLabel.BAD, FeedbackLabel.CORRECTED -> emptyList()
        }
        val example = FeedbackExample(
            question = reply.question,
            label = label,
            tools = kept,
            calls = reply.calls,
            answer = reply.answer.take(FeedbackExample.MAX_ANSWER_CHARS),
            at = System.currentTimeMillis(),
        )
        state.update { it.copy(rateable = null, feedbackStatus = "Saved. Misty uses this for similar questions.") }
        viewModelScope.launch {
            val saved = runCatching { withContext(Dispatchers.IO) { runtime.feedback.add(example) } }
            saved.onFailure { error ->
                state.update {
                    it.copy(feedbackStatus = null, error = "The rating could not be saved: ${error.message}")
                }
            }
        }
    }

    /**
     * Saves the owner's spoken correction of the reply to [question]: [answer] is the reply as the owner heard it, [correction]
     * is what is right, and [camera] is what the camera showed when the question was asked. Misty uses it for similar questions,
     * and it can be exported as training data.
     */
    fun recordCorrection(question: String, answer: String, correction: String, camera: String?) {
        val example = FeedbackExample(
            question = question,
            label = FeedbackLabel.CORRECTED,
            tools = emptyList(),
            calls = emptyList(),
            answer = answer.take(FeedbackExample.MAX_ANSWER_CHARS),
            at = System.currentTimeMillis(),
            correction = correction.take(FeedbackExample.MAX_ANSWER_CHARS),
            camera = camera?.take(FeedbackExample.MAX_ANSWER_CHARS),
        )
        viewModelScope.launch {
            val saved = runCatching { withContext(Dispatchers.IO) { runtime.feedback.add(example) } }
            saved.onFailure { error ->
                state.update { it.copy(error = "The correction could not be saved: ${error.message}") }
            }
        }
    }

    /** Opens the files panel, or closes it. Opening it reads the sandbox again. */
    fun toggleFiles() {
        val opening = !state.value.filesOpen
        state.update { it.copy(filesOpen = opening, sandboxStatus = null) }
        if (opening) refreshSandbox()
    }

    fun refreshSandbox() {
        viewModelScope.launch {
            val view = withContext(Dispatchers.IO) { sandboxView() }
            state.update { it.copy(sandbox = view) }
        }
    }

    /** Adds files from the device to this chat's sandbox, as they are. They can be opened from the chat afterwards. */
    fun addToSandbox(context: Context, uris: List<Uri>) {
        if (uris.isEmpty()) return
        viewModelScope.launch {
            state.update { it.copy(sandboxStatus = "Adding…") }
            val outcomes = withContext(Dispatchers.IO) {
                uris.map { uri ->
                    runCatching {
                        val name = Attachments.nameOf(context, uri)
                        val workspace = sandboxWorkspace()
                        val input = context.contentResolver.openInputStream(uri)
                            ?: throw IllegalArgumentException("$name could not be opened.")
                        input.use { workspace.importFile(name, it, Attachments.MAX_FILE_BYTES) }
                    }
                }
            }
            val added = outcomes.mapNotNull { it.getOrNull() }
            val errors = outcomes.mapNotNull { it.exceptionOrNull()?.message }
            val parts = listOfNotNull(
                added.takeIf { it.isNotEmpty() }?.let { "Added ${it.joinToString(", ")}." },
            ) + errors
            state.update { it.copy(sandboxStatus = parts.joinToString("\n").ifEmpty { null }) }
            refreshSandbox()
        }
    }

    fun removeSandboxFile(name: String) {
        val id = environmentId ?: return
        viewModelScope.launch {
            withContext(Dispatchers.IO) { runtime.environments.workspace(id)?.remove(name) }
            state.update { it.copy(sandboxStatus = "Removed $name. A chat that attached it can no longer open it.") }
            refreshSandbox()
        }
    }

    /** Links this chat's sandbox to another one, both ways, so the two chats can read and change each other's files. */
    fun linkSandbox(otherId: String) {
        viewModelScope.launch {
            val linked = withContext(Dispatchers.IO) {
                sandboxWorkspace()
                runtime.environments.link(environmentId ?: return@withContext false, otherId)
            }
            state.update {
                it.copy(
                    sandboxStatus = if (linked) {
                        "Linked. Both chats can now read and change each other's files."
                    } else {
                        "That sandbox could not be linked."
                    },
                )
            }
            refreshSandbox()
        }
    }

    fun unlinkSandbox(otherId: String) {
        val id = environmentId ?: return
        viewModelScope.launch {
            val removed = withContext(Dispatchers.IO) { runtime.environments.unlink(id, otherId) }
            state.update { it.copy(sandboxStatus = if (removed) "Unlinked." else "Those sandboxes were not linked.") }
            refreshSandbox()
        }
    }

    /** The full path of a file in this chat's sandbox, for opening it, or null when it is no longer there. */
    fun sandboxFile(name: String): File? {
        val dir = state.value.sandboxDir ?: return null
        val file = File(dir, name)
        return file.takeIf { it.isFile }
    }

    /**
     * The chat's sandbox, made on first use. Runs off the main thread, since it reads and writes the sandbox list.
     * Records the chat's folder for the screen to show files from.
     */
    private fun sandboxWorkspace(): Workspace {
        val existing = environmentId?.takeIf { runtime.environments.get(it) != null }
        val id = existing ?: runtime.environments.create(newSandboxName()).id
        environmentId = id
        val dir = sandboxDirOf(id)
        state.update { it.copy(sandboxDir = dir) }
        return runtime.environments.workspace(id) ?: throw IllegalStateException("The sandbox could not be opened.")
    }

    private fun sandboxDirOf(id: String?): String? = id?.let { runtime.environments.folderOf(it)?.absolutePath }

    private fun newSandboxName(): String =
        "Chat, " + SimpleDateFormat("d MMM HH:mm", Locale.getDefault()).format(Date())

    private fun sandboxView(): SandboxView {
        val all = runtime.environments.list()
        val chats = runCatching { store.list() }.getOrDefault(emptyList())
        fun linkFor(envId: String): SandboxLink? = all.firstOrNull { it.id == envId }?.let { env ->
            SandboxLink(env.id, env.name, chats.firstOrNull { it.environmentId == env.id }?.title)
        }
        val id = environmentId?.takeIf { runtime.environments.get(it) != null }
            ?: return SandboxView(
                environmentId = null,
                name = "No sandbox yet",
                files = emptyList(),
                linked = emptyList(),
                linkable = all.mapNotNull { linkFor(it.id) },
            )
        val env = all.first { it.id == id }
        val files = runtime.environments.workspace(id)?.files().orEmpty().map { SandboxFile(it.name, it.length()) }
        return SandboxView(
            environmentId = id,
            name = env.name,
            files = files,
            linked = env.links.mapNotNull { linkFor(it) },
            linkable = all.filter { it.id != id && it.id !in env.links }.mapNotNull { linkFor(it.id) },
        )
    }

    /**
     * The turn that answers a question. The chat's sandbox is the only folder the file tools reach, plus the sandboxes it is
     * linked to. The tool use and file-write settings decide what runs: off, asked for, or at once. With Misty on: her
     * instructions, and Needle when it is ready. Both use the same routing and the same saved verdicts.
     */
    private fun turnFor(backend: ChatBackend, workspace: Workspace): MistyTurn {
        val feedback = runtime.feedback
        val toolUse = runtime.settings.toolUse
        val fileWrites = runtime.settings.fileWrites
        val policies: ToolPolicies = ToolModes.policies(toolUse, fileWrites)
        val writable = ToolModes.writesOffered(toolUse, fileWrites)
        if (!runtime.settings.mistyEnabled) {
            return MistyTurn(
                chat = backend,
                tools = Toolbox(workspace, writable = writable),
                policies = policies,
                reflex = null,
                feedback = feedback,
                persona = false,
                notes = {},
            )
        }
        val tools = MistyToolbox(
            base = Toolbox(workspace, writable = writable),
            deviceStatus = { runtime.misty.status() },
            installedModels = { runtime.misty.installedModels() },
        )
        val reflex = if (toolUse != ToolUse.OFF && runtime.misty.reflexReady(runtime.settings.mistyReflex)) {
            NeedleReflex(runtime.needle)
        } else {
            null
        }
        return MistyTurn(
            chat = backend,
            tools = tools,
            policies = policies,
            reflex = reflex,
            feedback = feedback,
            persona = true,
            notes = { line -> noteMisty(line) },
        )
    }

    /**
     * What the model reads for this turn: each attachment's text in its message, and [note] added to the newest message. The
     * chat keeps only what the owner typed; the note is for this reply.
     */
    private fun promptFor(history: List<ChatMessage>, note: String?): List<ChatMessage> {
        val read = history.map { if (it.attachments.isEmpty()) it else ChatMessage(it.role, it.promptText()) }
        if (note.isNullOrBlank()) return read
        val last = read.last()
        return read.dropLast(1) + ChatMessage(last.role, last.content + "\n\n" + note)
    }

    /**
     * The note for this turn: the owner's corrections of similar questions, then [note]. The saved verdicts are read from the
     * disk, so this runs off the main thread. Null when there is neither.
     */
    private fun noteWith(question: String, note: String?): String? {
        val corrections = MistyLearning.corrections(question, runtime.feedback.examples())
        val correctionNote = if (corrections.isEmpty()) {
            null
        } else {
            buildString {
                append("The owner has corrected similar questions before. Keep these corrections in mind:")
                for (example in corrections) {
                    append("\n- Asked: ").append(example.question)
                    example.camera?.let { append("\n  Camera at the time: ").append(it.substringAfter('\n')) }
                    append("\n  Reply given then, which was wrong: ").append(example.answer.take(CORRECTION_NOTE_CHARS))
                    append("\n  Correct answer: ").append(example.correction.orEmpty())
                }
            }
        }
        return listOfNotNull(correctionNote, note?.takeIf { it.isNotBlank() }).joinToString("\n\n").ifEmpty { null }
    }

    /**
     * Shows the reply as it is written and a line for each tool it uses. The kept text is the last step's answer. When owner
     * notes are taken in, the text before them is kept as it was written, and the notes follow it.
     */
    private fun replyListener(
        streamed: StringBuilder,
        calls: MutableList<String>,
        history: List<ChatMessage>,
        added: MutableList<ChatMessage>,
    ) = object : ToolListener {
        override fun onStepText(stepText: String) {
            streamed.setLength(0)
            streamed.append(ToolProtocol.visibleText(stepText))
            state.update { it.copy(streaming = streamed.toString()) }
        }

        override fun onToolStart(call: ToolCall) {
            streamed.setLength(0)
            state.update { it.copy(streaming = "", toolNotes = it.toolNotes + "Using ${call.name}…") }
        }

        override fun onToolDone(call: ToolCall, outcome: ToolOutcome) {
            calls += "${call.name}:${outcomeWord(outcome)}"
            state.update { it.copy(toolNotes = it.toolNotes.dropLast(1) + toolNote(call, outcome)) }
        }

        override fun onSteered(partial: String, notes: List<String>) {
            streamed.setLength(0)
            if (partial.isNotBlank()) added += ChatMessage(Role.ASSISTANT, partial)
            notes.forEach { added += ChatMessage(Role.USER, it) }
            state.update {
                it.copy(messages = history + added, streaming = "", steers = it.steers.drop(notes.size))
            }
        }
    }

    /** Shows a Misty note on the reply, as a line under the answer. Called from the reply's thread. */
    private fun noteMisty(line: String) {
        state.update { it.copy(toolNotes = it.toolNotes + line) }
    }

    /**
     * Blocks the reply's thread until the owner allows or refuses [call]. Stop refuses it, so a stopped reply never waits
     * on a dialog that nobody answers.
     */
    private fun askOwner(call: ToolCall): Boolean {
        val answer = CompletableFuture<Boolean>()
        pendingAnswer = answer
        state.update { it.copy(pendingCall = describePendingCall(call)) }
        return try {
            answer.get()
        } finally {
            pendingAnswer = null
            state.update { it.copy(pendingCall = null) }
        }
    }

    /** The owner's answer to the tool call waiting for approval. */
    fun answerPendingCall(allow: Boolean) {
        pendingAnswer?.complete(allow)
    }

    /** Stops the reply that runs. Queued messages go on after it, unless the owner clears them. */
    fun stop() {
        pendingAnswer?.complete(false)
        reply?.cancel()
    }

    private fun startFreshConversation() {
        chatId = SavedChat.newId()
        chatCreatedAt = System.currentTimeMillis()
        environmentId = null
        steerInbox.clear()
    }

    companion object {
        fun factory(runtime: ModelRuntime): ViewModelProvider.Factory = viewModelFactory {
            initializer { ChatViewModel(runtime) }
        }
    }
}
