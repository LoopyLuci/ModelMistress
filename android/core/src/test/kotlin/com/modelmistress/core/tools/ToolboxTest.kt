package com.modelmistress.core.tools

import java.io.File
import java.nio.file.Files
import java.util.TimeZone
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ToolboxTest {
    private val root: File = Files.createTempDirectory("mm-tools").toFile()
    private val workspace = Workspace(root)
    private val toolbox = Toolbox(workspace, now = { 0L }, zone = TimeZone.getTimeZone("UTC"))

    @Test
    fun withoutAWorkspaceOnlyTheClockAndCalculatorAreOffered() {
        val plain = Toolbox(workspace = null)
        assertEquals(listOf("datetime", "calc"), plain.specs.map { it.name })
        assertEquals(listOf("datetime", "calc", "fs_list", "fs_read", "fs_search"), toolbox.specs.map { it.name })
    }

    @Test
    fun datetimeReportsTheClockInTheDeviceZone() {
        val outcome = toolbox.run(ToolCall("datetime", emptyMap()))
        val text = (outcome as ToolOutcome.Ok).text
        assertTrue(text, text.startsWith("Local time: 1970-01-01 00:00, Thursday."))
        assertTrue(text, text.contains("Time zone: UTC (UTC offset +00:00)"))
    }

    @Test
    fun calcEvaluatesAndReportsWholeNumbersWithoutDecimals() {
        assertEquals(ToolOutcome.Ok("391"), toolbox.run(ToolCall("calc", mapOf("expression" to "17*23"))))
        assertEquals(ToolOutcome.Ok("2.5"), toolbox.run(ToolCall("calc", mapOf("expression" to "5/2"))))
    }

    @Test
    fun missingArgumentsAndUnknownToolsAreRefusedNotRun() {
        assertEquals(
            ToolOutcome.Refused("missing argument expression"),
            toolbox.run(ToolCall("calc", emptyMap())),
        )
        assertTrue(toolbox.run(ToolCall("shell_run", mapOf("command" to "rm -rf /"))) is ToolOutcome.Refused)
    }

    @Test
    fun calculatorErrorsAreFailuresWithAReason() {
        assertEquals(ToolOutcome.Failed("division by zero"), toolbox.run(ToolCall("calc", mapOf("expression" to "1/0"))))
    }

    @Test
    fun fsReadReturnsALineRangeAndRefusesOutOfRangeLines() {
        File(root, "notes.txt").writeText("one\ntwo\nthree\n")
        val ranged = toolbox.run(ToolCall("fs_read", mapOf("path" to "notes.txt", "start_line" to "2", "end_line" to "3")))
        assertEquals(ToolOutcome.Ok("notes.txt, lines 2-3 of 3:\ntwo\nthree"), ranged)
        val past = toolbox.run(ToolCall("fs_read", mapOf("path" to "notes.txt", "start_line" to "9")))
        assertEquals(ToolOutcome.Ok("notes.txt has 3 lines; there are none in that range."), past)
    }

    @Test
    fun fsReadRefusesSecretsBinaryFilesAndPathsOutside() {
        File(root, ".env").writeText("TOKEN=abc")
        File(root, "image.bin").writeBytes(byteArrayOf(1, 0, 2))
        assertTrue(toolbox.run(ToolCall("fs_read", mapOf("path" to ".env"))).let { it is ToolOutcome.Failed && it.reason.contains("secret") })
        assertTrue(toolbox.run(ToolCall("fs_read", mapOf("path" to "image.bin"))).let { it is ToolOutcome.Failed && it.reason.contains("binary") })
        assertTrue(toolbox.run(ToolCall("fs_read", mapOf("path" to "../outside.txt"))).let { it is ToolOutcome.Failed && it.reason.contains("leaves the workspace") })
    }

    @Test
    fun fsListShowsFoldersWithASlashAndHidesSecrets() {
        File(root, "docs").mkdirs()
        File(root, "b.md").writeText("hello")
        File(root, "api.key").writeText("secret")
        val outcome = toolbox.run(ToolCall("fs_list", mapOf("path" to ".")))
        assertEquals(ToolOutcome.Ok("Folder .:\nb.md (5 bytes)\ndocs/"), outcome)
    }

    @Test
    fun fsSearchFindsLinesCaseInsensitivelyAndSkipsSecrets() {
        File(root, "notes").mkdirs()
        File(root, "notes/plan.txt").writeText("Buy milk\nmilk later\nnothing here\n")
        File(root, ".env").writeText("milk=secret")
        val outcome = toolbox.run(ToolCall("fs_search", mapOf("query" to "MILK", "max_results" to "5"))) as ToolOutcome.Ok
        assertEquals("2 matches for \"MILK\":\nnotes/plan.txt:1: Buy milk\nnotes/plan.txt:2: milk later", outcome.text)
        val limited = toolbox.run(ToolCall("fs_search", mapOf("query" to "milk", "max_results" to "1"))) as ToolOutcome.Ok
        assertEquals("1 match for \"milk\":\nnotes/plan.txt:1: Buy milk", limited.text)
    }

    @Test
    fun longOutputIsCutToTheLimit() {
        File(root, "long.txt").writeText("x".repeat(Bounded.MAX_OUTPUT_CHARS + 500))
        val text = (toolbox.run(ToolCall("fs_read", mapOf("path" to "long.txt"))) as ToolOutcome.Ok).text
        assertTrue(text.contains("more characters cut"))
        assertFalse(text.length > Bounded.MAX_OUTPUT_CHARS + 100)
    }
}
