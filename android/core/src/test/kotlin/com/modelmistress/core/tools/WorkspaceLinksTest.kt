package com.modelmistress.core.tools

import java.io.ByteArrayInputStream
import java.io.File
import java.io.IOException
import java.nio.file.Files
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class WorkspaceLinksTest {
    private val home: File = Files.createTempDirectory("mm-sandbox").toFile()
    private val shared: File = Files.createTempDirectory("mm-linked").toFile().also { File(it, "notes.txt").writeText("hello linked\n") }
    private val workspace = Workspace(home, mapOf("notes" to shared))

    @Test
    fun linkedFoldersResolveUnderTheLinksFolder() {
        assertEquals(File(shared, "notes.txt").canonicalFile, workspace.resolve("links/notes/notes.txt"))
        assertEquals("links/notes/notes.txt", workspace.relativePath(File(shared, "notes.txt").canonicalFile))
        assertEquals("links/notes", workspace.relativePath(shared.canonicalFile))
    }

    @Test
    fun unknownLinksAndEscapesAreRefused() {
        expectRefusal("links/missing/a.txt")
        expectRefusal("links/notes/../../etc")
    }

    @Test
    fun linksOfNothingLeaveTheFolderAsItWas() {
        val plain = Workspace(home)
        assertFalse(plain.isLinksFolder("links"))
        assertEquals(File(home.canonicalFile, "links"), plain.resolve("links"))
    }

    @Test
    fun writesReachTheLinkedFolder() {
        val toolbox = Toolbox(workspace, writable = true)
        val outcome = toolbox.run(ToolCall("fs_write", mapOf("path" to "links/notes/new.txt", "content" to "made here")))
        assertTrue(outcome is ToolOutcome.Ok)
        assertEquals("made here", File(shared, "new.txt").readText())
    }

    @Test
    fun listingShowsTheLinksAndSearchReachesThem() {
        val toolbox = Toolbox(workspace)
        val top = (toolbox.run(ToolCall("fs_list", mapOf("path" to "."))) as ToolOutcome.Ok).text
        assertTrue(top.contains("links/ (linked folders)"))
        val linked = (toolbox.run(ToolCall("fs_list", mapOf("path" to "links"))) as ToolOutcome.Ok).text
        assertTrue(linked.contains("notes/"))
        val found = (toolbox.run(ToolCall("fs_search", mapOf("query" to "linked"))) as ToolOutcome.Ok).text
        assertTrue(found, found.contains("links/notes/notes.txt:1:"))
    }

    @Test
    fun linkNamesMustBePlain() {
        try {
            Workspace(home, mapOf("a/b" to shared))
            fail("a link name with a slash must be refused")
        } catch (expected: IllegalArgumentException) {
            // expected
        }
    }

    @Test
    fun importKeepsTheFileAndNumbersAClash() {
        val first = workspace.importFile("photo.jpg", ByteArrayInputStream(byteArrayOf(1, 2, 3)), 1_000)
        val second = workspace.importFile("photo.jpg", ByteArrayInputStream(byteArrayOf(4)), 1_000)
        assertEquals("photo.jpg", first)
        assertEquals("photo (2).jpg", second)
        assertEquals(listOf<Byte>(1, 2, 3), File(home, first).readBytes().toList())
        assertFalse(File(home, "photo.jpg.part").exists())
    }

    @Test
    fun importStopsAtTheSizeLimitAndLeavesNothingBehind() {
        try {
            workspace.importFile("big.bin", ByteArrayInputStream(ByteArray(100)), 10)
            fail("a file over the limit must be refused")
        } catch (expected: IOException) {
            assertTrue(expected.message.orEmpty(), expected.message.orEmpty().contains("too large"))
        }
        assertFalse(File(home, "big.bin").exists())
        assertFalse(File(home, "big.bin.part").exists())
    }

    @Test
    fun importRefusesSecretNames() {
        try {
            workspace.importFile(".env", ByteArrayInputStream(byteArrayOf(1)), 100)
            fail("a secret file must not be added")
        } catch (expected: IllegalArgumentException) {
            assertTrue(expected.message.orEmpty().contains("secret"))
        }
    }

    private fun expectRefusal(path: String) {
        try {
            workspace.resolve(path)
            fail("$path should be refused")
        } catch (expected: IllegalArgumentException) {
            // expected
        }
    }
}
