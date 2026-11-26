package com.modelmistress.core.misty

import com.modelmistress.core.chat.ChatBackend
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.FinishReason
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.GenerationStats
import com.modelmistress.core.chat.Role
import com.modelmistress.core.chat.TokenSink

/** A chat model that answers with the given replies in order, then repeats the last one. Records what it was sent. */
internal class ScriptedChat(vararg replies: String) : ChatBackend {
    private val replies = replies.toList()
    val seen = mutableListOf<List<ChatMessage>>()
    var calls = 0
        private set

    override fun complete(messages: List<ChatMessage>, options: GenerationOptions, sink: TokenSink): GenerationStats {
        seen += messages
        val reply = replies.getOrElse(calls) { replies.last() }
        calls++
        sink.onText(reply)
        return GenerationStats(promptTokens = 0, completionTokens = 0, finishReason = FinishReason.STOP)
    }
}

internal fun user(text: String): ChatMessage = ChatMessage(Role.USER, text)
