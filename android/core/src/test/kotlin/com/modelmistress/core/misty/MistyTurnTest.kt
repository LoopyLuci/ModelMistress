package com.modelmistress.core.misty

import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.Role
import com.modelmistress.core.tools.ToolCall
import com.modelmistress.core.tools.ToolClass
import com.modelmistress.core.tools.ToolListener
import com.modelmistress.core.tools.ToolPolicies
import com.modelmistress.core.tools.ToolPolicy
import com.modelmistress.core.tools.Toolbox
import com.modelmistress.core.tools.Workspace
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File
import java.nio.file.Files

class MistyTurnTest {
    private fun temp(): File = Files.createTempDirectory("misty-turn").toFile()

    private fun turn(
        chat: ScriptedChat,
        tools: Toolbox,
        policies: ToolPolicies = ToolPolicies(),
        reflex: MistyReflex? = null,
        feedback: MistyFeedback? = null,
        persona: Boolean = true,
        notes: MutableList<String> = mutableListOf(),
    ) = MistyTurn(chat, tools, policies, reflex, feedback, persona) { notes += it }

    private fun ask(
        turn: MistyTurn,
        text: String,
        confirm: (ToolCall) -> Boolean = { false },
    ) = turn.run(listOf(user(text)), GenerationOptions(), { true }, confirm, object : ToolListener {})

    private val calcProposal =
        """{"function_calls":[{"name":"calc","arguments":{"expression":"17*23"}}],"confidence":0.9}"""

    @Test
    fun aGreetingIsAnsweredWithNoToolOffered() {
        val chat = ScriptedChat("Hello. How can I help?")
        val result = ask(turn(chat, Toolbox(Workspace(temp()), writable = true)), "hello")

        assertEquals("Hello. How can I help?", result.answer)
        assertEquals(0, result.toolCalls)
        // The model was not even told the tools exist, so a greeting cannot start a tool call.
        assertFalse(chat.seen.single().any { it.content.contains("List of tools") })
    }

    @Test
    fun aGreetingCannotRunAToolEvenWhenTheModelWritesOne() {
        val chat = ScriptedChat("[datetime()]", "Hello.")
        val result = ask(turn(chat, Toolbox(), reflex = null), "hello")

        assertEquals(0, result.toolCalls)
        assertEquals(1, chat.calls)
    }

    @Test
    fun aClockQuestionRunsTheClockAndAnswersFromIt() {
        val chat = ScriptedChat("[datetime()]", "It is noon.")
        val result = ask(turn(chat, Toolbox()), "what time is it")

        assertEquals(1, result.toolCalls)
        assertEquals("It is noon.", result.answer)
        assertEquals(listOf("datetime"), result.route.tools)
        assertTrue(chat.seen.last().any { it.content.contains("<tool_result name=\"datetime\" status=\"ok\">") })
    }

    @Test
    fun aCallToAToolThatWasNotRoutedIsRefused() {
        val root = temp()
        val chat = ScriptedChat("[fs_write(path=\"a.txt\", content=\"x\")]", "Not written.")
        val result = ask(turn(chat, Toolbox(Workspace(root), writable = true)), "what time is it")

        assertEquals(1, result.toolCalls)
        assertFalse(File(root, "a.txt").exists())
        assertTrue(chat.seen.last().any { it.content.contains("there is no tool called fs_write") })
    }

    @Test
    fun aSaveRequestAsksTheOwnerAndWritesOnAllow() {
        val root = temp()
        val chat = ScriptedChat("[fs_write(path=\"notes.txt\", content=\"hello\")]", "Saved.")
        var asked = 0
        val policies = ToolPolicies(mapOf(ToolClass.WRITE to ToolPolicy.ASK))
        val result = ask(
            turn(chat, Toolbox(Workspace(root), writable = true), policies),
            "save hello to notes.txt",
            confirm = {
                asked++
                true
            },
        )

        assertEquals(1, asked)
        assertEquals(1, result.toolCalls)
        assertEquals("hello", File(root, "notes.txt").readText())
    }

    @Test
    fun aRefusedWriteIsNotAskedAgainInTheSameReply() {
        val root = temp()
        val chat = ScriptedChat(
            "[fs_write(path=\"a.txt\", content=\"x\")]",
            "[fs_write(path=\"b.txt\", content=\"y\")]",
            "Not saved.",
        )
        var asked = 0
        val policies = ToolPolicies(mapOf(ToolClass.WRITE to ToolPolicy.ASK))
        ask(
            turn(chat, Toolbox(Workspace(root), writable = true), policies),
            "save hello to notes.txt",
            confirm = {
                asked++
                false
            },
        )

        assertEquals(1, asked)
        assertFalse(File(root, "a.txt").exists())
        assertFalse(File(root, "b.txt").exists())
        assertTrue(chat.seen.last().any { it.content.contains("Do not try it again") })
    }

    @Test
    fun aNeedleProposalRunsOnlyWhenTheRouterOffersTools() {
        var proposals = 0
        val reflex = MistyReflex { _, _ ->
            proposals++
            calcProposal
        }
        val chat = ScriptedChat("It is 391.", "It is 391.")
        ask(turn(chat, Toolbox(), reflex = reflex), "hello")
        assertEquals(0, proposals)

        val result = ask(turn(chat, Toolbox(), reflex = reflex), "what is 17 times 23")
        assertEquals(1, proposals)
        assertEquals(1, result.toolCalls)
        assertEquals(listOf("calc"), result.route.tools)
    }

    @Test
    fun theOwnersVerdictKeepsAToolAwayFromASimilarQuestion() {
        val store = FileMistyFeedback(File(temp(), "feedback.jsonl"))
        store.add(FeedbackExample("what time is it", FeedbackLabel.NO_TOOLS, emptyList(), emptyList(), "", 1L))
        val notes = mutableListOf<String>()
        val chat = ScriptedChat("It is a good time.")
        val result = ask(turn(chat, Toolbox(), feedback = store, notes = notes), "what time is it")

        assertEquals(0, result.toolCalls)
        assertTrue(chat.seen.single().none { it.role == Role.SYSTEM && it.content.contains("List of tools") })
        assertTrue(notes.any { it.contains("owner taught") })
    }

    @Test
    fun thePlainChatHasNoMistyPersona() {
        val plain = ScriptedChat("Hello.")
        ask(turn(plain, Toolbox(), persona = false), "hello")
        assertTrue(plain.seen.single().none { it.content.contains("You are Misty") })

        val misty = ScriptedChat("Hello.")
        ask(turn(misty, Toolbox(), persona = true), "hello")
        assertTrue(misty.seen.single().any { it.content.contains("You are Misty") })
    }

    @Test
    fun aRoutedToolsAreShownInTheTurnsNotes() {
        val notes = mutableListOf<String>()
        ask(turn(ScriptedChat("It is noon."), Toolbox(), notes = notes), "what time is it")
        assertTrue(notes.any { it.contains("datetime") })
    }
}
