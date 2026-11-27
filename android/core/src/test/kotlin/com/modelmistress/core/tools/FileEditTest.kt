package com.modelmistress.core.tools

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File
import java.nio.file.Files

class FileEditTest {
    private val root = Files.createTempDirectory("file-edit").toFile()
    private val toolbox = Toolbox(Workspace(root), writable = true)

    private fun edit(path: String, old: String, new: String? = null): ToolOutcome {
        val arguments = mutableMapOf("path" to path, "old_text" to old)
        if (new != null) arguments["new_text"] = new
        return toolbox.run(ToolCall("fs_edit", arguments))
    }

    private fun write(name: String, text: String) = File(root, name).also { it.parentFile.mkdirs(); it.writeText(text) }

    @Test
    fun replacesTheOneMatchingPlaceAndLeavesTheRestAlone() {
        val file = write("a.txt", "one two three\nfour\n")
        val outcome = edit("a.txt", "two", "2")

        assertEquals(ToolOutcome.Ok("Edited a.txt: one place changed."), outcome)
        assertEquals("one 2 three\nfour\n", file.readText())
    }

    @Test
    fun multiLineTextCanBeReplaced() {
        val file = write("a.txt", "dear all,\nplease read\nthanks\n")
        assertTrue(edit("a.txt", "please read\nthanks", "please read this\nthank you") is ToolOutcome.Ok)
        assertEquals("dear all,\nplease read this\nthank you\n", file.readText())
    }

    @Test
    fun omittingNewTextDeletesTheOldText() {
        val file = write("a.txt", "keep this DRAFT then")
        assertTrue(edit("a.txt", " DRAFT") is ToolOutcome.Ok)
        assertEquals("keep this then", file.readText())
    }

    @Test
    fun aTextThatIsNotThereFailsAndChangesNothing() {
        val file = write("a.txt", "hello")
        val outcome = edit("a.txt", "missing")

        assertTrue(outcome is ToolOutcome.Failed)
        assertTrue((outcome as ToolOutcome.Failed).reason.contains("not found"))
        assertEquals("hello", file.readText())
    }

    @Test
    fun aTextThatAppearsTwiceFailsAndChangesNothing() {
        val file = write("a.txt", "x and x")
        val outcome = edit("a.txt", "x")

        assertTrue(outcome is ToolOutcome.Failed)
        assertTrue((outcome as ToolOutcome.Failed).reason.contains("appears 2 times"))
        assertEquals("x and x", file.readText())
    }

    @Test
    fun windowsLineEndingsAreKept() {
        val file = write("a.txt", "a\r\nb\r\nc\r\n")
        assertTrue(edit("a.txt", "a\nb", "x\ny") is ToolOutcome.Ok)
        assertEquals("x\r\ny\r\nc\r\n", file.readText())
    }

    @Test
    fun aMissingFileIsNotCreatedByEdit() {
        val outcome = edit("new.txt", "a", "b")
        assertTrue(outcome is ToolOutcome.Failed)
        assertTrue(!File(root, "new.txt").exists())
    }

    @Test
    fun aSecretFileIsNotEdited() {
        val file = write(".env", "TOKEN=abc")
        val outcome = edit(".env", "abc", "xyz")
        assertTrue(outcome is ToolOutcome.Failed)
        assertEquals("TOKEN=abc", file.readText())
    }

    @Test
    fun editIsOnlyOfferedWhenWritesAreAllowed() {
        val readOnly = Toolbox(Workspace(root))
        assertTrue(readOnly.spec("fs_edit") == null)
        assertEquals(ToolClass.WRITE, toolbox.spec("fs_edit")?.toolClass)
    }

    @Test
    fun writeCreatesTheFoldersAboveTheFile() {
        val outcome = toolbox.run(ToolCall("fs_write", mapOf("path" to "notes/todo/list.txt", "content" to "buy milk")))
        assertTrue(outcome is ToolOutcome.Ok)
        assertEquals("buy milk", File(root, "notes/todo/list.txt").readText())
    }
}
