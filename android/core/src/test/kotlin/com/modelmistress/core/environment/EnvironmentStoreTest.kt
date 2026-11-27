package com.modelmistress.core.environment

import com.modelmistress.core.tools.ToolCall
import com.modelmistress.core.tools.ToolOutcome
import com.modelmistress.core.tools.Toolbox
import java.io.File
import java.nio.file.Files
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class EnvironmentStoreTest {
    private val base: File = Files.createTempDirectory("mm-environments").toFile()
    private val store = EnvironmentStore(base) { 1_000L }

    @Test
    fun theDefaultSandboxIsAlwaysThereAndKeepsTheOldFolder() {
        val default = store.get(EnvironmentStore.DEFAULT_ID)
        assertNotNull(default)
        assertEquals("workspace", default!!.folder)
        assertEquals(1, store.list().count { it.id == EnvironmentStore.DEFAULT_ID })
    }

    @Test
    fun createdSandboxesGetTheirOwnFolderAndSurviveANewStore() {
        val made = store.create("Trip plans")
        assertEquals("Trip plans", made.name)
        assertTrue(made.folder.startsWith("environments/"))
        val reopened = EnvironmentStore(base)
        assertEquals("Trip plans", reopened.get(made.id)?.name)
    }

    @Test
    fun renameChangesTheNameOnly() {
        val made = store.create("Old")
        assertTrue(store.rename(made.id, "New name"))
        assertEquals("New name", store.get(made.id)?.name)
        assertEquals(made.folder, store.get(made.id)?.folder)
        assertFalse(store.rename("missing", "x"))
    }

    @Test
    fun deletingASandboxRemovesItsFilesAndItsLinks() {
        val a = store.create("A")
        val b = store.create("B")
        assertTrue(store.link(a.id, b.id))
        File(base, b.folder).mkdirs()
        File(File(base, b.folder), "keep.txt").writeText("gone soon")

        assertTrue(store.delete(b.id))
        assertNull(store.get(b.id))
        assertFalse(File(base, b.folder).exists())
        assertFalse(store.get(a.id)!!.links.contains(b.id))
    }

    @Test
    fun linksWorkBothWays() {
        val a = store.create("Chat one")
        val b = store.create("Chat two")
        assertTrue(store.link(a.id, b.id))
        assertEquals(listOf(b.id), store.get(a.id)?.links)
        assertEquals(listOf(a.id), store.get(b.id)?.links)
        assertTrue(store.unlink(b.id, a.id))
        assertTrue(store.get(a.id)!!.links.isEmpty())
        assertTrue(store.get(b.id)!!.links.isEmpty())
    }

    @Test
    fun aSandboxCannotLinkToItselfOrToAMissingOne() {
        val a = store.create("Solo")
        assertFalse(store.link(a.id, a.id))
        assertFalse(store.link(a.id, "nope"))
    }

    @Test
    fun theLinkedSandboxIsReachableFromTheToolsAndNothingElseIs() {
        val a = store.create("Home")
        val b = store.create("Shared notes")
        store.link(a.id, b.id)
        File(base, b.folder).mkdirs()
        File(File(base, b.folder), "plan.txt").writeText("ship on friday\n")

        val workspace = store.workspace(a.id)!!
        assertEquals(listOf("Shared notes"), workspace.linkNames())
        val toolbox = Toolbox(workspace, writable = true)
        val read = toolbox.run(ToolCall("fs_read", mapOf("path" to "links/Shared notes/plan.txt")))
        assertTrue(read.toString(), (read as ToolOutcome.Ok).text.contains("ship on friday"))
    }

    @Test
    fun twoLinkedSandboxesWithTheSameNameGetDistinctLinkNames() {
        val a = store.create("Me")
        val one = store.create("Notes")
        val two = store.create("Notes")
        store.link(a.id, one.id)
        store.link(a.id, two.id)
        assertEquals(listOf("Notes", "Notes 2"), store.workspace(a.id)!!.linkNames())
    }

    @Test
    fun aDamagedIndexFallsBackToTheDefaultSandbox() {
        store.create("Lost")
        File(base, "environments.json").writeText("{ not json")
        val reopened = EnvironmentStore(base)
        assertEquals(listOf(EnvironmentStore.DEFAULT_ID), reopened.list().map { it.id })
    }
}
