package com.modelmistress.core.misty

import com.modelmistress.core.chat.GenerationOptions
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

/** A reply may not say a file was saved or changed unless a file tool ran. */
class MistyClaimGuardTest {
    private fun turn(chat: ScriptedChat, notes: MutableList<String>) = MistyTurn(
        chat = chat,
        tools = Toolbox(Workspace(Files.createTempDirectory("misty-claim").toFile()), writable = true),
        policies = ToolPolicies(mapOf(ToolClass.WRITE to ToolPolicy.ASK)),
        reflex = null,
        feedback = null,
        persona = true,
    ) { notes += it }

    private fun answer(question: String, reply: String, notes: MutableList<String> = mutableListOf()) =
        turn(ScriptedChat(reply), notes).run(listOf(user(question)), GenerationOptions(), { true }, { true }, object : ToolListener {})

    @Test
    fun aSavedClaimWithNoToolRunIsReplaced() {
        val notes = mutableListOf<String>()
        val result = answer(
            "save the word hi",
            "The word \"hi\" has been successfully saved to notes/hi.txt.",
            notes,
        )

        assertEquals(0, result.toolCalls)
        assertEquals(MistyTurn.NO_FILE_REPLY, result.answer)
        assertTrue(result.replaced)
        assertTrue(notes.any { it.contains("no file tool ran") })
    }

    @Test
    fun aChangeClaimWithNoToolRunIsReplaced() {
        val result = answer("change hello to goodbye in notes/test.txt", "notes/test.txt has been updated to goodbye.")
        assertEquals(MistyTurn.NO_FILE_REPLY, result.answer)
    }

    @Test
    fun aRefusalIsKept() {
        val reply = "The operation to save \"stop\" to notes/stop.txt was not performed."
        assertEquals(reply, answer("save the word stop to notes/stop.txt", reply).answer)
    }

    @Test
    fun anOfferIsKept() {
        val reply = "I can save it to notes.txt if you name the words to put in it."
        assertFalse(MistyTurn.claimsFileChange(reply))
        assertEquals(reply, answer("could you save something for me", reply).answer)
    }

    @Test
    fun aClaimMadeWithAToolRunIsNotReplacedHere() {
        val root = Files.createTempDirectory("misty-claim-tool").toFile()
        File(root, "notes").mkdirs()
        File(root, "notes/test.txt").writeText("hello")
        val chat = ScriptedChat(
            """<tool>{"name":"fs_edit","arguments":{"path":"notes/test.txt","old_text":"hello","new_text":"goodbye"}}</tool>""",
            "notes/test.txt has been updated: hello replaced with goodbye.",
        )
        val result = MistyTurn(
            chat = chat,
            tools = Toolbox(Workspace(root), writable = true),
            policies = ToolPolicies(mapOf(ToolClass.WRITE to ToolPolicy.ASK)),
            reflex = null,
            feedback = null,
            persona = true,
        ) {}.run(
            listOf(user("change hello to goodbye in notes/test.txt")),
            GenerationOptions(),
            { true },
            { true },
            object : ToolListener {},
        )

        assertEquals(1, result.toolCalls)
        assertFalse(result.answer == MistyTurn.NO_FILE_REPLY)
        assertEquals("goodbye", File(root, "notes/test.txt").readText())
    }

    @Test
    fun theClaimPatternNeedsAPastTenseVerbAndAFileName() {
        assertTrue(MistyTurn.claimsFileChange("I wrote the note to diary.md"))
        assertTrue(MistyTurn.claimsFileChange("Saved as notes/hi.txt"))
        assertFalse(MistyTurn.claimsFileChange("Hello. How can I help?"))
        assertFalse(MistyTurn.claimsFileChange("You can save files in the notes folder."))
    }
}
