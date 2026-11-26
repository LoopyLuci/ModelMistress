package com.modelmistress.core.chat

/**
 * A chat split into what a model needs to write its next reply: the system text, the turns before the one being answered,
 * and that message. Engines that keep their own conversation state start from this form.
 */
data class ReplyTurns(
    val system: String?,
    val earlier: List<ChatMessage>,
    val latest: String,
)

/**
 * Splits [messages] for a reply. System messages are joined into one system text, and the last message that is not system
 * text must be from the user.
 */
fun splitForReply(messages: List<ChatMessage>): ReplyTurns {
    val system = messages.filter { it.role == Role.SYSTEM }.map { it.content }.filter { it.isNotBlank() }
    val turns = messages.filter { it.role != Role.SYSTEM }
    val last = turns.lastOrNull() ?: throw IllegalArgumentException("Chat history must contain the user message.")
    require(last.role == Role.USER) { "The final chat message must be from the user." }
    return ReplyTurns(
        system = system.takeIf { it.isNotEmpty() }?.joinToString("\n\n"),
        earlier = turns.dropLast(1),
        latest = last.content,
    )
}
