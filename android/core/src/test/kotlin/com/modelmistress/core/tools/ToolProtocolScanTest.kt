package com.modelmistress.core.tools

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ToolProtocolScanTest {
    @Test
    fun aValueMayHoldCommasBracketsAndParentheses() {
        val parsed = ToolProtocol.parse("[fs_write(path=\"a.py\", content=\"f(x), [y] )] end\")]")!!
        assertEquals("f(x), [y] )] end", parsed.call.arguments["content"])
        assertEquals("a.py", parsed.call.arguments["path"])
    }

    @Test
    fun escapedQuotesAndNewLinesAreWrittenBack() {
        val parsed = ToolProtocol.parse("[fs_write(path=\"a.txt\", content=\"line one\\nsay \\\"two\\\"\")]")!!
        assertEquals("line one\nsay \"two\"", parsed.call.arguments["content"])
    }

    @Test
    fun aRealNewLineInsideAValueIsKept() {
        val reply = "[fs_write(path=\"a.txt\", content=\"one\ntwo\n\nfour\")]"
        assertEquals("one\ntwo\n\nfour", ToolProtocol.parse(reply)!!.call.arguments["content"])
    }

    @Test
    fun aQuoteInsideTheTextDoesNotEndTheValue() {
        val parsed = ToolProtocol.parse("[fs_write(path=\"a.txt\", content=\"He said \"hi\" to me\")]")!!
        assertEquals("He said \"hi\" to me", parsed.call.arguments["content"])
    }

    @Test
    fun aBackslashThatIsNotAnEscapeStaysAsItIs() {
        val parsed = ToolProtocol.parse("[fs_read(path=\"C:\\reports\\a.txt\")]")!!
        assertEquals("C:\\reports\\a.txt", parsed.call.arguments["path"])
    }

    @Test
    fun theRawTextOfACallIsTheWholeCallWithItsBrackets() {
        val reply = "Saving. [fs_edit(path=\"a.txt\", old_text=\"two\", new_text=\"2\")] Done."
        val parsed = ToolProtocol.parse(reply)!!
        assertEquals("[fs_edit(path=\"a.txt\", old_text=\"two\", new_text=\"2\")]", parsed.raw)
        assertEquals("Saving. Done.", ToolProtocol.visibleText(reply))
    }

    @Test
    fun anIncompleteCallIsNotParsedAndDoesNotShow() {
        val partial = "Saving. [fs_write(path=\"a.txt\", content=\"hel"
        assertNull(ToolProtocol.parse(partial))
        assertEquals("Saving.", ToolProtocol.visibleText(partial))
    }

    @Test
    fun aBracketedWordWithoutACallStaysVisible() {
        assertEquals("Use [notes] (later).", ToolProtocol.visibleText("Use [notes] (later)."))
    }

    @Test
    fun theInstructionSaysToUseToolsOnlyWhenNeededAndNotToRetryARefusal() {
        val text = ToolProtocol.instruction(Toolbox(Workspace(java.io.File(".")), writable = true).specs)
        assertTrue(text.contains("Answer greetings and general questions yourself"))
        assertTrue(text.contains("do not try it again"))
        assertTrue(text.contains("\"name\":\"fs_edit\""))
    }
}
