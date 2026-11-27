package com.modelmistress.core.tools

import com.modelmistress.core.chat.ChatBackend
import com.modelmistress.core.chat.ChatMessage
import com.modelmistress.core.chat.FinishReason
import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.GenerationStats
import com.modelmistress.core.chat.Role
import com.modelmistress.core.chat.TokenSink
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ToolLoopTest {
    private val calcBlock = "<tool>{\"name\": \"calc\", \"arguments\": {\"expression\": \"17*23\"}}</tool>"

    @Test
    fun answersDirectlyWhenNoToolIsNeeded() {
        val backend = ScriptedBackend("Hello there.")
        val run = loop().run(backend, listOf(ChatMessage(Role.USER, "Hi")), options(), { true }, { false }, RecordingListener())
        assertEquals(ToolRun("Hello there.", 0), run)
        assertEquals(Role.SYSTEM, backend.seen.single().first().role)
    }

    @Test
    fun runsAToolCallAndAnswersFromItsResult() {
        val backend = ScriptedBackend("Let me compute. $calcBlock", "It is 391.")
        val listener = RecordingListener()
        val run = loop().run(backend, listOf(ChatMessage(Role.USER, "17 times 23?")), options(), { true }, { false }, listener)

        assertEquals(ToolRun("It is 391.", 1), run)
        assertEquals(listOf("calc"), listener.started)
        assertEquals(listOf("ok:391"), listener.done)
        val secondCall = backend.seen[1]
        assertTrue(secondCall.any { it.role == Role.ASSISTANT && it.content == calcBlock })
        assertTrue(secondCall.any {
            it.role == Role.USER &&
                it.content == "<|tool_response_start|><tool_result name=\"calc\" status=\"ok\">\n391\n</tool_result><|tool_response_end|>"
        })
    }

    @Test
    fun deniedToolsAreNotOfferedAndTheirBlocksDoNotRun() {
        val policies = ToolPolicies(mapOf(ToolClass.READ to ToolPolicy.DENY))
        val backend = ScriptedBackend("Let me compute. $calcBlock")
        val listener = RecordingListener()
        val run = ToolLoop(Toolbox(), policies).run(backend, listOf(ChatMessage(Role.USER, "x")), options(), { true }, { false }, listener)

        assertEquals(ToolRun("Let me compute.", 0), run)
        assertTrue(listener.started.isEmpty())
        assertFalse(backend.seen.single().any { it.role == Role.SYSTEM })
    }

    @Test
    fun askPolicyWaitsForConfirmationAndRefusesWithoutIt() {
        val policies = ToolPolicies(mapOf(ToolClass.READ to ToolPolicy.ASK))
        val asked = mutableListOf<String>()
        val backend = ScriptedBackend(calcBlock, "Not allowed, sorry.")
        val run = ToolLoop(Toolbox(), policies).run(
            backend,
            listOf(ChatMessage(Role.USER, "x")),
            options(),
            { true },
            { call -> asked += call.name; false },
            RecordingListener(),
        )
        assertEquals(listOf("calc"), asked)
        assertEquals(1, run.toolCalls)
        assertTrue(backend.seen[1].any { it.content.contains("status=\"refused\"") && it.content.contains("the owner did not allow calc") })
    }

    @Test
    fun theCallLimitEndsWithAnAnswerWrittenWithoutTools() {
        val backend = ScriptedBackend(calcBlock, "Final answer, without tools.")
        val run = ToolLoop(Toolbox(), maxCalls = 1).run(
            backend,
            listOf(ChatMessage(Role.USER, "x")),
            options(),
            { true },
            { false },
            RecordingListener(),
        )
        assertEquals(ToolRun("Final answer, without tools.", 1), run)
        // After the limit the last reply is written with no tool instruction, so it cannot start another call.
        assertFalse(backend.seen.last().any { it.role == Role.SYSTEM })
    }

    @Test
    fun stoppingBeforeTheReplyStartsWritesNothing() {
        val backend = ScriptedBackend("never")
        val run = loop().run(backend, listOf(ChatMessage(Role.USER, "x")), options(), { false }, { false }, RecordingListener())
        assertEquals(ToolRun("", 0), run)
        assertTrue(backend.seen.isEmpty())
    }

    private fun loop() = ToolLoop(Toolbox(), ToolPolicies())

    private fun options() = GenerationOptions(maxTokens = 64)

    private class RecordingListener : ToolListener {
        val started = mutableListOf<String>()
        val done = mutableListOf<String>()

        override fun onToolStart(call: ToolCall) {
            started += call.name
        }

        override fun onToolDone(call: ToolCall, outcome: ToolOutcome) {
            done += when (outcome) {
                is ToolOutcome.Ok -> "ok:${outcome.text}"
                is ToolOutcome.Refused -> "refused:${outcome.reason}"
                is ToolOutcome.Failed -> "failed:${outcome.reason}"
            }
        }
    }

    /** Replies with each scripted text in turn, in small pieces, and remembers the messages it was sent. */
    private class ScriptedBackend(vararg replies: String) : ChatBackend {
        private val replies = replies.toList()
        val seen = mutableListOf<List<ChatMessage>>()
        private var next = 0

        override fun complete(messages: List<ChatMessage>, options: GenerationOptions, sink: TokenSink): GenerationStats {
            seen += messages.toList()
            val reply = replies.getOrElse(next) { "" }
            next++
            for (piece in reply.chunked(9)) {
                if (!sink.onText(piece)) break
            }
            return GenerationStats(0, 0, FinishReason.STOP)
        }
    }
}
