package com.modelmistress.core.misty

import com.modelmistress.core.tools.ToolClass
import com.modelmistress.core.tools.ToolParameter
import com.modelmistress.core.tools.ToolSpec
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class MistyReflexProposalTest {
    @Test
    fun readsAConfidentProposal() {
        val proposal = MistyReflexProposal.parse(
            """{"function_calls":[{"name":"calc","arguments":{"expression":"17*23"}}],"confidence":0.8}""",
        )
        assertEquals(MistyReflexProposal("calc", mapOf("expression" to "17*23"), 0.8), proposal)
    }

    @Test
    fun aMissingConfidenceCountsAsZero() {
        val proposal = MistyReflexProposal.parse("""{"function_calls":[{"name":"datetime","arguments":{}}]}""")
        assertEquals(0.0, proposal?.confidence ?: -1.0, 0.0)
    }

    @Test
    fun anAnswerWithoutACallIsNotAProposal() {
        assertNull(MistyReflexProposal.parse("""{"confidence":0.9}"""))
        assertNull(MistyReflexProposal.parse("not json at all"))
        assertNull(MistyReflexProposal.parse("""{"function_calls":[{"arguments":{}}],"confidence":0.9}"""))
    }

    @Test
    fun theToolSchemasNameTheirRequiredArguments() {
        val json = toolsJsonFor(
            listOf(
                ToolSpec(
                    name = "fs_read",
                    toolClass = ToolClass.READ,
                    description = "Read a file.",
                    parameters = listOf(
                        ToolParameter("path", "File inside the workspace.", true),
                        ToolParameter("start_line", "First line.", false),
                    ),
                ),
            ),
        )
        assertTrue(json.contains("\"name\":\"fs_read\""))
        assertTrue(json.contains("\"required\":[\"path\"]"))
    }
}
