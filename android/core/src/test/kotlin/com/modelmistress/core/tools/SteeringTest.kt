package com.modelmistress.core.tools

import com.modelmistress.core.chat.ChatBackend
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.FinishReason
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.GenerationStats
import com.modelmistress.core.chat.Role
import com.modelmistress.core.chat.TokenSink
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** Replies one scripted answer per call, a character at a time, so a stop can land between any two characters. */
private class CharByCharBackend(private val replies: List<String>) : ChatBackend {
    val seen = mutableListOf<List<ChatMessage>>()
    private var next = 0

    override fun complete(messages: List<ChatMessage>, options: GenerationOptions, sink: TokenSink): GenerationStats {
        seen += messages
        val reply = replies[next++]
        for (character in reply) {
            if (!sink.onText(character.toString())) break
        }
        return GenerationStats(0, 0, FinishReason.STOP)
    }
}

/** A queue of owner notes: pending while any is waiting, and taken in the order sent. */
private class NoteQueue(vararg notes: String) : Steering {
    private val waiting = ArrayDeque(notes.toList())
    override fun pending(): Boolean = waiting.isNotEmpty()
    override fun take(): List<String> = waiting.toList().also { waiting.clear() }
}

private class Recording : ToolListener {
    var steeredPartial: String? = null
    var steeredNotes: List<String> = emptyList()

    override fun onSteered(partial: String, notes: List<String>) {
        steeredPartial = partial
        steeredNotes = notes
    }
}

class SteeringTest {
    private val calcBlock = "<tool>{\"name\": \"calc\", \"arguments\": {\"expression\": \"17*23\"}}</tool>"

    @Test
    fun aNoteIsTakenInAtTheNextSentenceEndWithoutRestartingTheReply() {
        val backend = CharByCharBackend(listOf("Hello there. How are you?", "Sure, I will be brief."))
        val listener = Recording()
        val run = ToolLoop(Toolbox()).run(
            backend = backend,
            history = listOf(ChatMessage(Role.USER, "Hi")),
            options = GenerationOptions(),
            keepGoing = { true },
            confirm = { false },
            listener = listener,
            steering = NoteQueue("Keep it short"),
        )

        assertEquals("Hello there.", listener.steeredPartial)
        assertEquals(listOf("Keep it short"), listener.steeredNotes)
        assertEquals("Sure, I will be brief.", run.answer)
        val second = backend.seen[1]
        assertEquals("Hello there.", second.first { it.role == Role.ASSISTANT }.content)
        assertTrue(second.last().content.contains("Keep it short"))
        assertEquals(Role.USER, second.last().role)
    }

    @Test
    fun aNoteIsTakenInAfterAToolResult() {
        val backend = CharByCharBackend(listOf(calcBlock, "It is 391."))
        val listener = Recording()
        val run = ToolLoop(Toolbox()).run(
            backend = backend,
            history = listOf(ChatMessage(Role.USER, "17 times 23?")),
            options = GenerationOptions(),
            keepGoing = { true },
            confirm = { false },
            listener = listener,
            steering = NoteQueue("Use the calculator"),
        )

        assertEquals("", listener.steeredPartial)
        assertEquals("It is 391.", run.answer)
        assertTrue(backend.seen[1].last().content.contains("Use the calculator"))
    }

    @Test
    fun aNoteThatComesTooLateIsLeftForTheCallerToSend() {
        val steering = NoteQueue()
        val backend = CharByCharBackend(listOf("Done."))
        ToolLoop(Toolbox()).run(
            backend = backend,
            history = listOf(ChatMessage(Role.USER, "Hi")),
            options = GenerationOptions(),
            keepGoing = { true },
            confirm = { false },
            listener = Recording(),
            steering = steering,
        )
        assertEquals(false, steering.pending())
    }

    @Test
    fun endsSentenceRecognisesTheEndOfAThoughtOnly() {
        assertTrue(ToolLoop.endsSentence("Hello."))
        assertTrue(ToolLoop.endsSentence("Hello! "))
        assertTrue(ToolLoop.endsSentence("Line\n"))
        assertEquals(false, ToolLoop.endsSentence("Hello"))
        assertEquals(false, ToolLoop.endsSentence(""))
    }
}
