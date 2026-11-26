package com.modelmistress.core.chat

import com.modelmistress.core.json.Json
import com.modelmistress.core.json.JsonException
import java.io.File
import java.io.IOException
import java.util.UUID

/** A conversation the owner kept on this device. [id] names its file in a [ChatStore], so it must match [ID_PATTERN]. */
data class SavedChat(
    val id: String,
    val title: String,
    val modelId: String?,
    val createdAt: Long,
    val updatedAt: Long,
    val messages: List<ChatMessage>,
    /** The sandbox this chat works in. Null for a chat saved before sandboxes existed; it then uses the older files. */
    val environmentId: String? = null,
) {
    init {
        require(ID_PATTERN.matches(id)) { "A chat id uses 1 to 64 letters, digits, '.', '_' or '-'." }
    }

    fun summary(): ChatSummary = ChatSummary(id, title, modelId, updatedAt, messages.size, environmentId)

    companion object {
        val ID_PATTERN = Regex("[A-Za-z0-9._-]{1,64}")
        const val MAX_TITLE = 60

        fun newId(): String = UUID.randomUUID().toString()

        /** The first line of the first question, cut to [MAX_TITLE] characters. "New chat" when nothing was asked yet. */
        fun titleFor(messages: List<ChatMessage>): String {
            val question = messages.firstOrNull { it.role == Role.USER }?.content.orEmpty()
            val line = question.lineSequence().map { it.trim() }.firstOrNull { it.isNotEmpty() }
                ?: return "New chat"
            return if (line.length <= MAX_TITLE) line else line.take(MAX_TITLE - 1).trimEnd() + "…"
        }
    }
}

/** One line of the history list. Enough to show the list without reading every message. */
data class ChatSummary(
    val id: String,
    val title: String,
    val modelId: String?,
    val updatedAt: Long,
    val messageCount: Int,
    val environmentId: String? = null,
)

/** Where saved chats are kept. The app keeps them as files; the tests keep them in a folder they make. */
interface ChatStore {
    /** Newest first. A chat that cannot be read is left out, not reported. */
    fun list(): List<ChatSummary>

    /** The chat with [id], or null when there is none or it cannot be read. */
    fun load(id: String): SavedChat?

    fun save(chat: SavedChat)

    /** Removes the chat. Returns false when there was none. */
    fun delete(id: String): Boolean
}

/** Turns a [SavedChat] into JSON and back. A file that is not a chat this version wrote is refused. */
object ChatCodec {
    const val VERSION = 1

    fun encode(chat: SavedChat): String = Json.write(
        linkedMapOf(
            "version" to VERSION,
            "id" to chat.id,
            "title" to chat.title,
            "model" to chat.modelId,
            "created_at" to chat.createdAt,
            "updated_at" to chat.updatedAt,
            "environment" to chat.environmentId,
            "messages" to chat.messages.map { message ->
                linkedMapOf<String, Any?>(
                    "role" to message.role.wire,
                    "content" to message.content,
                ).also { item ->
                    if (message.attachments.isNotEmpty()) {
                        item["attachments"] = message.attachments.map { attachment ->
                            linkedMapOf(
                                "label" to attachment.label,
                                "file" to attachment.file,
                                "kind" to attachment.kind.wire,
                                "text" to attachment.text,
                            )
                        }
                    }
                }
            },
        ),
    )

    fun decode(text: String): SavedChat {
        val root = Json.parse(text) as? Map<*, *> ?: throw JsonException("A saved chat must be a JSON object.")
        val version = (root["version"] as? Number)?.toInt()
        if (version != VERSION) throw JsonException("This saved chat is from an unknown version.")
        val messages = (root["messages"] as? List<*>)?.map { entry ->
            val item = entry as? Map<*, *> ?: throw JsonException("A saved message must be a JSON object.")
            val role = Role.fromWire(item["role"] as? String ?: "")
                ?: throw JsonException("A saved message has an unknown role.")
            val attachments = (item["attachments"] as? List<*>)?.map { raw ->
                val part = raw as? Map<*, *> ?: throw JsonException("A saved attachment must be a JSON object.")
                ChatAttachment(
                    label = part["label"] as? String ?: throw JsonException("A saved attachment has no label."),
                    file = part["file"] as? String ?: throw JsonException("A saved attachment has no file."),
                    kind = AttachmentKind.fromWire(part["kind"] as? String ?: "")
                        ?: throw JsonException("A saved attachment has an unknown kind."),
                    text = part["text"] as? String ?: "",
                )
            } ?: emptyList()
            ChatMessage(
                role,
                item["content"] as? String ?: throw JsonException("A saved message has no text."),
                attachments,
            )
        } ?: throw JsonException("A saved chat has no message list.")
        return SavedChat(
            id = root["id"] as? String ?: throw JsonException("A saved chat has no id."),
            title = root["title"] as? String ?: "New chat",
            modelId = root["model"] as? String,
            createdAt = (root["created_at"] as? Number)?.toLong() ?: 0L,
            updatedAt = (root["updated_at"] as? Number)?.toLong() ?: 0L,
            messages = messages,
            environmentId = root["environment"] as? String,
        )
    }
}

/**
 * Keeps each chat in its own `<id>.json` file under [directory]. A write goes to a temporary file first, so a crash
 * leaves the previous version whole. When more than [limit] chats are kept, the oldest are removed.
 */
class FileChatStore(private val directory: File, private val limit: Int = DEFAULT_LIMIT) : ChatStore {
    init {
        require(limit >= 1) { "The history must keep at least one chat." }
        directory.mkdirs()
    }

    override fun list(): List<ChatSummary> = files()
        .mapNotNull { file -> readFile(file)?.summary() }
        .sortedByDescending { it.updatedAt }

    override fun load(id: String): SavedChat? = fileFor(id)?.takeIf { it.isFile }?.let(::readFile)

    @Synchronized
    override fun save(chat: SavedChat) {
        val target = fileFor(chat.id) ?: throw IOException("The chat id is not usable as a file name.")
        val partial = File(directory, target.name + PARTIAL)
        try {
            partial.writeText(ChatCodec.encode(chat), Charsets.UTF_8)
            if (target.exists() && !target.delete()) throw IOException("The previous copy of the chat could not be replaced.")
            if (!partial.renameTo(target)) throw IOException("The chat could not be saved.")
        } finally {
            partial.delete()
        }
        prune()
    }

    @Synchronized
    override fun delete(id: String): Boolean = fileFor(id)?.delete() ?: false

    private fun prune() {
        val kept = list()
        kept.drop(limit).forEach { delete(it.id) }
    }

    private fun files(): List<File> = directory.listFiles { file -> file.isFile && file.name.endsWith(EXTENSION) }
        .orEmpty()
        .toList()

    private fun fileFor(id: String): File? = if (SavedChat.ID_PATTERN.matches(id)) File(directory, id + EXTENSION) else null

    private fun readFile(file: File): SavedChat? = runCatching {
        ChatCodec.decode(file.readText(Charsets.UTF_8)).takeIf { file.name == it.id + EXTENSION }
    }.getOrNull()

    companion object {
        const val DEFAULT_LIMIT = 200
        private const val EXTENSION = ".json"
        private const val PARTIAL = ".part"
    }
}
