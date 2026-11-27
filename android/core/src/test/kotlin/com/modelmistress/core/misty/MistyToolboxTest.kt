package com.modelmistress.core.misty

import com.modelmistress.core.chat.GenerationOptions
import com.modelmistress.core.tools.ToolCall
import com.modelmistress.core.tools.ToolClass
import com.modelmistress.core.tools.ToolListener
import com.modelmistress.core.tools.ToolLoop
import com.modelmistress.core.tools.ToolOutcome
import com.modelmistress.core.tools.ToolPolicies
import com.modelmistress.core.tools.ToolPolicy
import com.modelmistress.core.tools.Toolbox
import com.modelmistress.core.tools.Workspace
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File
import java.nio.file.Files

class MistyToolboxTest {
    private fun temp(): File = Files.createTempDirectory("misty-toolbox").toFile()

    private val writeBlock = """<tool>{"name": "fs_write", "arguments": {"path": "notes/todo.txt", "content": "buy milk"}}</tool>"""

    @Test
    fun deviceStatusAndInstalledModelsAreReads() {
        val toolbox = MistyToolbox(Toolbox(), { "Free memory: 4.0 GB." }, { "local chat: lfm2" })
        assertEquals(ToolClass.READ, toolbox.spec("device_status")?.toolClass)
        assertEquals(ToolClass.READ, toolbox.spec("models_list")?.toolClass)
        assertEquals(ToolOutcome.Ok("Free memory: 4.0 GB."), toolbox.run(ToolCall("device_status", emptyMap())))
        assertEquals(ToolOutcome.Ok("local chat: lfm2"), toolbox.run(ToolCall("models_list", emptyMap())))
    }

    @Test
    fun aFailingStatusReportIsAFailureNotACrash() {
        val toolbox = MistyToolbox(Toolbox(), { error("no status") }, { "" })
        val outcome = toolbox.run(ToolCall("device_status", emptyMap()))
        assertTrue(outcome is ToolOutcome.Failed)
    }

    @Test
    fun fileWriteIsOnlyOfferedWhenTheToolboxIsWritable() {
        assertNull(Toolbox(Workspace(temp())).spec("fs_write"))
        assertEquals(ToolClass.WRITE, Toolbox(Workspace(temp()), writable = true).spec("fs_write")?.toolClass)
    }

    @Test
    fun aWriteNeedsTheOwnersOkAndLandsOnlyWhenGiven() {
        val refusedRoot = temp()
        val refused = ToolLoop(MistyToolbox(Toolbox(Workspace(refusedRoot), writable = true), { "" }, { "" })).run(
            ScriptedChat(writeBlock, "Not written."),
            listOf(user("save it")),
            GenerationOptions(),
            { true },
            { false },
            object : ToolListener {},
        )
        assertEquals(1, refused.toolCalls)
        assertFalse(File(refusedRoot, "notes/todo.txt").exists())

        val allowedRoot = temp()
        val allowed = ToolLoop(MistyToolbox(Toolbox(Workspace(allowedRoot), writable = true), { "" }, { "" })).run(
            ScriptedChat(writeBlock, "Saved."),
            listOf(user("save it")),
            GenerationOptions(),
            { true },
            { true },
            object : ToolListener {},
        )
        assertEquals(1, allowed.toolCalls)
        assertEquals("buy milk", File(allowedRoot, "notes/todo.txt").readText())
    }

    @Test
    fun theWriteRefusesSecretsEscapesAndBinaryData() {
        val root = temp()
        val toolbox = Toolbox(Workspace(root), writable = true)
        fun write(path: String, content: String) = toolbox.run(ToolCall("fs_write", mapOf("path" to path, "content" to content)))

        assertTrue(write(".env", "KEY=1") is ToolOutcome.Failed)
        assertTrue(write("keys/id_rsa.pem", "x") is ToolOutcome.Failed)
        assertTrue(write("../outside.txt", "x") is ToolOutcome.Failed)
        assertTrue(write("notes.txt", "bad\u0000byte") is ToolOutcome.Failed)
        assertFalse(File(root, ".env").exists())
    }

    @Test
    fun aDeniedWriteNeverReachesTheFileSystem() {
        val root = temp()
        val toolbox = Toolbox(Workspace(root), writable = true)
        val run = ToolLoop(toolbox, ToolPolicies(mapOf(ToolClass.WRITE to ToolPolicy.DENY))).run(
            ScriptedChat(writeBlock, "Okay."),
            listOf(user("save it")),
            GenerationOptions(),
            { true },
            { true },
            object : ToolListener {},
        )
        assertEquals(1, run.toolCalls)
        assertFalse(File(root, "notes/todo.txt").exists())
    }
}
