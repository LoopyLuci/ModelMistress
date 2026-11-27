package com.modelmistress.core.tools

import java.io.File
import java.nio.file.Files
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class WorkspaceTest {
    private val root: File = Files.createTempDirectory("mm-workspace").toFile()
    private val workspace = Workspace(root)

    @Test
    fun resolvesPathsInsideTheWorkspace() {
        File(root, "notes").mkdirs()
        assertEquals(File(workspace.root, "notes").canonicalFile, workspace.resolve("notes"))
        assertEquals(workspace.root, workspace.resolve(""))
        assertEquals(workspace.root, workspace.resolve("."))
    }

    @Test
    fun refusesPathsThatLeaveTheWorkspace() {
        expectRefusal("../outside.txt")
        expectRefusal("notes/../../outside.txt")
        expectRefusal("/etc/passwd")
        expectRefusal("\\windows\\win.ini")
        expectRefusal("C:/Windows/win.ini")
    }

    @Test
    fun marksSecretNamesExtensionsAndFolders() {
        assertTrue(Workspace.isSecret(".env"))
        assertTrue(Workspace.isSecret(".env.local"))
        assertTrue(Workspace.isSecret("keys/id_rsa"))
        assertTrue(Workspace.isSecret("certs/server.pem"))
        assertTrue(Workspace.isSecret("backup/store.kdbx"))
        assertTrue(Workspace.isSecret("project/.ssh/config"))
        assertTrue(Workspace.isSecret("secrets.json"))
        assertFalse(Workspace.isSecret("notes/todo.txt"))
        assertFalse(Workspace.isSecret("notes/ssh-ideas.md"))
    }

    @Test
    fun relativePathUsesForwardSlashes() {
        val file = File(root, "a/b.txt").apply { parentFile.mkdirs(); writeText("x") }
        assertEquals("a/b.txt", workspace.relativePath(file))
        assertEquals(".", workspace.relativePath(workspace.root))
    }

    @Test
    fun safeNamesKeepPlainCharactersOnly() {
        assertEquals("notes.txt", Workspace.safeName("notes.txt"))
        assertEquals("file.md", Workspace.safeName("my/file.md"))
        assertEquals("my_file.md", Workspace.safeName("my*file.md"))
        assertEquals("passwd", Workspace.safeName("../../passwd"))
        assertEquals("file.txt", Workspace.safeName("...."))
    }

    @Test
    fun storesTextAndRefusesOverwritesSecretsAndBinaryFiles() {
        val stored = workspace.store("plan.txt", "hello".toByteArray())
        assertEquals("hello", stored.readText())
        assertEquals(listOf("plan.txt"), workspace.files().map { it.name })
        expectStoreRefusal("plan.txt", "already in the workspace")
        expectStoreRefusal(".env", "secret")
        expectStoreRefusal("keys.pem", "secret")
        expectStoreRefusal("bin.dat", "binary", byteArrayOf(1, 0, 2))
        expectStoreRefusal("../escape.txt", "cannot be used")
        assertTrue(workspace.remove("plan.txt"))
        assertFalse(workspace.remove("plan.txt"))
    }

    private fun expectStoreRefusal(name: String, reason: String, bytes: ByteArray = "x".toByteArray()) {
        try {
            workspace.store(name, bytes)
            fail("$name should not be stored")
        } catch (error: IllegalArgumentException) {
            assertTrue(error.message.orEmpty(), error.message.orEmpty().contains(reason))
        }
    }

    private fun expectRefusal(path: String) {
        try {
            workspace.resolve(path)
            fail("$path should be refused")
        } catch (error: IllegalArgumentException) {
            assertTrue(error.message.orEmpty().isNotEmpty())
        }
    }
}
