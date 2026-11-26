package com.modelmistress.core.tools

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File
import java.nio.file.Files

/** fs_write says whether it created a file or replaced one, so the chat cannot say the wrong thing. */
class WriteWordingTest {
    private fun text(outcome: ToolOutcome): String {
        assertTrue("expected Ok but got $outcome", outcome is ToolOutcome.Ok)
        return (outcome as ToolOutcome.Ok).text
    }

    @Test
    fun aNewFileIsReportedAsCreated() {
        val root = Files.createTempDirectory("write-wording").toFile()
        val toolbox = Toolbox(Workspace(root), writable = true)

        val result = text(toolbox.run(ToolCall("fs_write", mapOf("path" to "notes/new.txt", "content" to "hi"))))

        assertTrue(result, result.startsWith("Created notes/new.txt"))
        assertEquals("hi", File(root, "notes/new.txt").readText())
    }

    @Test
    fun anExistingFileIsReportedAsReplaced() {
        val root = Files.createTempDirectory("write-wording-replace").toFile()
        File(root, "notes").mkdirs()
        File(root, "notes/test.txt").writeText("hello")
        val toolbox = Toolbox(Workspace(root), writable = true)

        val result = text(toolbox.run(ToolCall("fs_write", mapOf("path" to "notes/test.txt", "content" to "bye"))))

        assertTrue(result, result.startsWith("Replaced notes/test.txt"))
        assertEquals("bye", File(root, "notes/test.txt").readText())
    }
}
