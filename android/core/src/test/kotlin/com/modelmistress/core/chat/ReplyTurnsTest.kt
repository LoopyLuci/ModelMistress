package com.modelmistress.core.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ReplyTurnsTest {
    @Test
    fun systemTextIsJoinedAndKeptOutOfTheTurns() {
        val split = splitForReply(
            listOf(
                ChatMessage(Role.SYSTEM, "Be brief."),
                ChatMessage(Role.USER, "Hi"),
                ChatMessage(Role.ASSISTANT, "Hello."),
                ChatMessage(Role.SYSTEM, "Use tools."),
                ChatMessage(Role.USER, "17 times 23?"),
            ),
        )
        assertEquals("Be brief.\n\nUse tools.", split.system)
        assertEquals(listOf(ChatMessage(Role.USER, "Hi"), ChatMessage(Role.ASSISTANT, "Hello.")), split.earlier)
        assertEquals("17 times 23?", split.latest)
    }

    @Test
    fun withoutSystemTextTheSystemIsNull() {
        val split = splitForReply(listOf(ChatMessage(Role.USER, "Hi")))
        assertNull(split.system)
        assertEquals(emptyList<ChatMessage>(), split.earlier)
        assertEquals("Hi", split.latest)
    }

    @Test
    fun blankSystemTextIsIgnored() {
        assertNull(splitForReply(listOf(ChatMessage(Role.SYSTEM, "  "), ChatMessage(Role.USER, "Hi"))).system)
    }

    @Test(expected = IllegalArgumentException::class)
    fun aReplyMustAnswerAUserMessage() {
        splitForReply(listOf(ChatMessage(Role.USER, "Hi"), ChatMessage(Role.ASSISTANT, "Hello.")))
    }

    @Test(expected = IllegalArgumentException::class)
    fun aChatWithNoUserMessageIsRefused() {
        splitForReply(listOf(ChatMessage(Role.SYSTEM, "Be brief.")))
    }
}
