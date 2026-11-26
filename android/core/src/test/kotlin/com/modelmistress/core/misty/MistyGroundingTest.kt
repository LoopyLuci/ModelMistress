package com.modelmistress.core.misty

import com.modelmistress.core.chat.GenerationOptions
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

/** Needle's proposals for writes and edits are run only when they fit the question the owner asked. */
class MistyGroundingTest {
    private fun root(): File = Files.createTempDirectory("misty-grounding").toFile()

    private fun turn(root: File, reflex: MistyReflex, chat: ScriptedChat, notes: MutableList<String>) = MistyTurn(
        chat = chat,
        tools = Toolbox(Workspace(root), writable = true),
        policies = ToolPolicies(mapOf(ToolClass.WRITE to ToolPolicy.ASK)),
        reflex = reflex,
        feedback = null,
        persona = true,
    ) { notes += it }

    private fun ask(turn: MistyTurn, text: String, confirm: (ToolCall) -> Boolean) =
        turn.run(listOf(user(text)), GenerationOptions(), { true }, confirm, object : ToolListener {})

    private fun proposal(name: String, arguments: String) =
        """{"function_calls":[{"name":"$name","arguments":$arguments}],"confidence":0.42}"""

    @Test
    fun aWriteToAPathTheQuestionDoesNotNameIsNeverAsked() {
        val root = root()
        val notes = mutableListOf<String>()
        val reflex = MistyReflex { _, _ ->
            proposal("fs_write", """{"path":"goodbye","content":"hello to goodbye in notes/test.txt"}""")
        }
        var asked = 0
        val result = ask(turn(root, reflex, ScriptedChat("Nothing was saved."), notes), "save hello to notes/test.txt") {
            asked++
            true
        }

        assertEquals(0, asked)
        assertEquals(0, result.toolCalls)
        assertFalse(File(root, "goodbye").exists())
        assertTrue(notes.any { it.contains("does not match the question") })
    }

    @Test
    fun aWriteToTheNamedPathIsAskedFor() {
        val root = root()
        val reflex = MistyReflex { _, _ -> proposal("fs_write", """{"path":"notes/test.txt","content":"hello"}""") }
        var asked = 0
        val result = ask(turn(root, reflex, ScriptedChat("Saved."), mutableListOf()), "save hello to notes/test.txt") {
            asked++
            true
        }

        assertEquals(1, asked)
        assertEquals(1, result.toolCalls)
        assertEquals("hello", File(root, "notes/test.txt").readText())
    }

    @Test
    fun anEditOfTheNamedFileIsAskedForAndRuns() {
        val root = root()
        File(root, "notes").mkdirs()
        File(root, "notes/test.txt").writeText("hello there")
        val reflex = MistyReflex { _, _ ->
            proposal("fs_edit", """{"path":"notes/test.txt","old_text":"hello","new_text":"goodbye"}""")
        }
        var asked = 0
        val result = ask(
            turn(root, reflex, ScriptedChat("Changed."), mutableListOf()),
            "change hello to goodbye in notes/test.txt",
        ) {
            asked++
            true
        }

        assertEquals(1, asked)
        assertEquals(1, result.toolCalls)
        assertEquals("goodbye there", File(root, "notes/test.txt").readText())
    }

    @Test
    fun anEditOfTextTheQuestionDoesNotContainIsNeverAsked() {
        val root = root()
        File(root, "notes").mkdirs()
        File(root, "notes/test.txt").writeText("hello there")
        val reflex = MistyReflex { _, _ ->
            proposal("fs_edit", """{"path":"notes/test.txt","old_text":"banana","new_text":"apple"}""")
        }
        var asked = 0
        ask(
            turn(root, reflex, ScriptedChat("Not changed."), mutableListOf()),
            "change hello to goodbye in notes/test.txt",
        ) {
            asked++
            true
        }

        assertEquals(0, asked)
        assertEquals("hello there", File(root, "notes/test.txt").readText())
    }
}
