package com.modelmistress.core.misty

import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.chat.Role
import com.modelmistress.core.tools.ToolClass
import com.modelmistress.core.tools.ToolListener
import com.modelmistress.core.tools.ToolLoop
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

class MistyBackendTest {
    private val calcProposal =
        """{"function_calls":[{"name":"calc","arguments":{"expression":"17*23"}}],"confidence":0.9}"""
    private val question = "17 times 23?"

    private fun loop(toolbox: Toolbox, policies: ToolPolicies = ToolPolicies()) = ToolLoop(toolbox, policies)

    private fun ask(
        backend: MistyBackend,
        toolbox: Toolbox,
        policies: ToolPolicies = ToolPolicies(),
        confirm: (com.modelmistress.core.tools.ToolCall) -> Boolean = { false },
        text: String = question,
    ) = loop(toolbox, policies).run(
        backend,
        listOf(user(text)),
        GenerationOptions(),
        { true },
        confirm,
        object : ToolListener {},
    )

    @Test
    fun aConfidentProposalRunsThroughTheLoopAndTheChatModelAnswers() {
        val toolbox = Toolbox()
        val chat = ScriptedChat("It is 391.")
        val notes = mutableListOf<String>()
        val run = ask(MistyBackend(chat, toolbox, { _, _ -> calcProposal }, notes::add), toolbox)

        assertEquals(1, run.toolCalls)
        assertEquals("It is 391.", run.answer)
        assertEquals(1, chat.calls)
        assertTrue(chat.seen.single().any { it.role == Role.USER && it.content.contains("391") })
        assertTrue(notes.any { it.contains("Needle proposed calc") })
    }

    @Test
    fun aLowConfidenceProposalIsNotRunAndTheChatModelDecides() {
        val toolbox = Toolbox()
        val chat = ScriptedChat("I am not sure.")
        val notes = mutableListOf<String>()
        val unsure = """{"function_calls":[{"name":"calc","arguments":{"expression":"1+1"}}],"confidence":0.02}"""
        val run = ask(MistyBackend(chat, toolbox, { _, _ -> unsure }, notes::add), toolbox)

        assertEquals(0, run.toolCalls)
        assertEquals("I am not sure.", run.answer)
        assertTrue(notes.any { it.contains("not confident") })
    }

    @Test
    fun theReflexRunsOncePerQuestionNotAfterAToolResult() {
        val toolbox = Toolbox()
        var asked = 0
        val reflex = MistyReflex { _, _ ->
            asked++
            calcProposal
        }
        val run = ask(MistyBackend(ScriptedChat("It is 391."), toolbox, reflex, {}), toolbox)

        assertEquals(1, asked)
        assertEquals(1, run.toolCalls)
    }

    @Test
    fun aBackendWithoutAReflexNeverProposesACall() {
        val toolbox = Toolbox()
        val chat = ScriptedChat("It is 391.")
        val run = ask(MistyBackend(chat, toolbox, null, {}), toolbox)

        assertEquals(0, run.toolCalls)
        assertEquals("It is 391.", run.answer)
    }

    @Test
    fun aProposalNamingAToolThatIsNotOfferedIsNotRun() {
        val toolbox = Toolbox()
        val chat = ScriptedChat("Sorry, I cannot run that.")
        val notes = mutableListOf<String>()
        val shell = """{"function_calls":[{"name":"shell","arguments":{"command":"ls"}}],"confidence":0.9}"""
        val run = ask(MistyBackend(chat, toolbox, { _, _ -> shell }, notes::add), toolbox)

        assertEquals(0, run.toolCalls)
        assertTrue(notes.any { it.contains("not offered") })
    }

    @Test
    fun aProposalForATurnedOffToolIsRefusedByTheLoop() {
        val root = Files.createTempDirectory("misty-backend").toFile()
        val toolbox = Toolbox(Workspace(root), writable = true)
        val chat = ScriptedChat("Okay, I will not write it.")
        val write = """{"function_calls":[{"name":"fs_write","arguments":{"path":"notes.txt","content":"hello"}}],"confidence":0.9}"""
        val policies = ToolPolicies(mapOf(ToolClass.WRITE to ToolPolicy.DENY))
        val run = ask(MistyBackend(chat, toolbox, { _, _ -> write }, {}), toolbox, policies, text = "save hello to notes.txt")

        assertEquals(1, run.toolCalls)
        assertTrue(chat.seen.last().any { it.content.contains("fs_write is turned off") })
        assertFalse(File(root, "notes.txt").exists())
    }

    @Test
    fun aWriteProposalWaitsForTheOwnersOk() {
        val root = Files.createTempDirectory("misty-backend-ok").toFile()
        val toolbox = Toolbox(Workspace(root), writable = true)
        val chat = ScriptedChat("Done.")
        val write = """{"function_calls":[{"name":"fs_write","arguments":{"path":"notes.txt","content":"hello"}}],"confidence":0.9}"""
        var asked = 0
        val run = ask(
            MistyBackend(chat, toolbox, { _, _ -> write }, {}),
            toolbox,
            confirm = {
                asked++
                true
            },
            text = "save hello to notes.txt",
        )

        assertEquals(1, asked)
        assertEquals(1, run.toolCalls)
        assertEquals("hello", File(root, "notes.txt").readText())
    }
}
