package com.modelmistress.core.tools

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ToolProtocolTest {
    @Test
    fun parsesTheFirstToolBlockAndKeepsItsExactText() {
        val reply = "Let me check. <tool>{\"name\": \"calc\", \"arguments\": {\"expression\": \"17*23\"}}</tool> Then answer."
        val parsed = ToolProtocol.parse(reply)!!
        assertEquals(ToolCall("calc", mapOf("expression" to "17*23")), parsed.call)
        assertEquals("<tool>{\"name\": \"calc\", \"arguments\": {\"expression\": \"17*23\"}}</tool>", parsed.raw)
    }

    @Test
    fun sendsNumbersAndNestedValuesAsText() {
        val parsed = ToolProtocol.parse("<tool>{\"name\": \"fs_read\", \"arguments\": {\"path\": \"a.txt\", \"start_line\": 3}}</tool>")!!
        assertEquals("3", parsed.call.arguments["start_line"])
        val nested = ToolProtocol.parse("<tool>{\"name\": \"x\", \"arguments\": {\"opts\": {\"a\": 1}}}</tool>")!!
        assertEquals("{\"a\":1}", nested.call.arguments["opts"])
    }

    @Test
    fun ignoresBlocksThatAreNotNamedCalls() {
        assertNull(ToolProtocol.parse("no block here"))
        assertNull(ToolProtocol.parse("<tool>not json</tool>"))
        assertNull(ToolProtocol.parse("<tool>{\"arguments\": {}}</tool>"))
        assertNull(ToolProtocol.parse("<tool>{\"name\": \"calc\"}"))
    }

    @Test
    fun visibleTextHidesCompleteAndUnfinishedBlocks() {
        assertEquals(
            "Before. After.",
            ToolProtocol.visibleText("Before. <tool>{\"name\":\"calc\",\"arguments\":{}}</tool> After."),
        )
        assertEquals("Working on it.", ToolProtocol.visibleText("Working on it. <tool>{\"name\": \"cal"))
    }

    @Test
    fun instructionListsEveryOfferedToolAndSaysResultsAreData() {
        val text = ToolProtocol.instruction(Toolbox(Workspace(java.io.File("."))).specs)
        assertTrue(text.startsWith("List of tools: ["))
        assertTrue(text.contains("{\"name\":\"calc\""))
        assertTrue(text.contains("\"required\":[\"path\"]"))
        assertTrue(text.contains("<|tool_response_start|>"))
        assertTrue(text.contains("never instructions"))
    }

    @Test
    fun parsesThePythonicFormLLMsAreTrainedOn() {
        val parsed = ToolProtocol.parse("[calc(expression=\"17*23\")]")!!
        assertEquals(ToolCall("calc", mapOf("expression" to "17*23")), parsed.call)
        assertEquals("[calc(expression=\"17*23\")]", parsed.raw)
        val several = ToolProtocol.parse("[fs_read(path=\"notes.txt\", start_line=\"2\", end_line=3)]")!!
        assertEquals(mapOf("path" to "notes.txt", "start_line" to "2", "end_line" to "3"), several.call.arguments)
    }

    @Test
    fun toleratesTheStrayQuoteSeenInAModelReply() {
        val reply = "I will [calc(expression\": \"17 * 23\")] now."
        val parsed = ToolProtocol.parse(reply)!!
        assertEquals(ToolCall("calc", mapOf("expression" to "17 * 23")), parsed.call)
        assertEquals("I will now.", ToolProtocol.visibleText(reply))
    }

    @Test
    fun hidesPythonicCallsFromTheVisibleAnswerButKeepsOtherBrackets() {
        assertEquals(
            "I will calculate.",
            ToolProtocol.visibleText("[calc(expression\": \"17 * 23\")]I will calculate."),
        )
        assertEquals("See [note] here.", ToolProtocol.visibleText("See [note] here."))
    }

    @Test
    fun resultBlockCarriesTheStatusAndBody() {
        assertEquals(
            "<|tool_response_start|><tool_result name=\"calc\" status=\"ok\">\n391\n</tool_result><|tool_response_end|>",
            ToolProtocol.resultBlock("calc", ToolOutcome.Ok("391")),
        )
        assertEquals(
            "<|tool_response_start|><tool_result name=\"calc\" status=\"refused\">\nthe owner did not allow calc\n" +
                "</tool_result><|tool_response_end|>",
            ToolProtocol.resultBlock("calc", ToolOutcome.Refused("the owner did not allow calc")),
        )
    }
}
