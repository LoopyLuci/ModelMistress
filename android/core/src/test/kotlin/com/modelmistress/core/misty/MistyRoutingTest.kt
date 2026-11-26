package com.modelmistress.core.misty

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class MistyRoutingTest {
    private val everything = setOf(
        "datetime", "calc", "fs_list", "fs_read", "fs_search", "fs_write", "fs_edit", "device_status", "models_list",
    )
    private val readOnly = setOf("datetime", "calc", "fs_list", "fs_read", "fs_search", "device_status", "models_list")

    private fun route(
        question: String,
        available: Set<String> = everything,
        examples: List<FeedbackExample> = emptyList(),
        reflexReady: Boolean = true,
    ) = MistyRouter.route(question, available, examples, reflexReady)

    @Test
    fun aGreetingIsOfferedNoTool() {
        val route = route("hello")
        assertTrue(route.tools.isEmpty())
        assertFalse(route.reflex)
        assertTrue(route.reasons.isEmpty())
    }

    @Test
    fun smallTalkIsOfferedNoToolEvenWithAllToolsOn() {
        for (text in listOf("hi there", "thanks!", "good morning", "how are you today?", "tell me a joke")) {
            assertTrue(text, route(text).tools.isEmpty())
        }
    }

    @Test
    fun aClockQuestionIsOfferedTheClockAndNothingElse() {
        assertEquals(listOf("datetime"), route("what time is it").tools)
        assertEquals(listOf("datetime"), route("what day is it today").tools)
    }

    @Test
    fun arithmeticIsOfferedTheCalculator() {
        assertTrue(MistyFeatures.isArithmetic("what is 17 * 23"))
        assertEquals(listOf("calc"), route("what is 17 * 23").tools)
        assertEquals(listOf("calc"), route("calculate 2 plus 2").tools)
    }

    @Test
    fun aFileQuestionIsOfferedTheReadTools() {
        assertEquals(listOf("fs_list", "fs_read", "fs_search"), route("list the files in my notes").tools)
        assertEquals(listOf("fs_list", "fs_read", "fs_search"), route("read the file notes.txt").tools)
    }

    @Test
    fun aCreateRequestIsOfferedTheWriteToolOnly() {
        val tools = route("save hello to notes.txt").tools
        assertTrue(tools.contains("fs_write"))
        assertFalse(tools.contains("fs_edit"))
        assertTrue(route("make a new file called todo.txt").tools.contains("fs_write"))
    }

    @Test
    fun aChangeRequestIsOfferedTheEditToolOnly() {
        val tools = route("change hello to goodbye in notes/test.txt").tools
        assertTrue(tools.contains("fs_edit"))
        assertFalse(tools.contains("fs_write"))
        assertTrue(route("change the line in notes.txt").tools.contains("fs_edit"))
    }

    @Test
    fun aWriteWordWithoutAFileIsNotAFileTask() {
        assertTrue(route("add two more ideas to the list").tools.isEmpty())
    }

    @Test
    fun writeToolsAreOfferedOnlyWhenTheyAreAvailable() {
        val tools = route("save hello to notes.txt", available = readOnly).tools
        assertFalse(tools.contains("fs_write"))
        assertFalse(tools.contains("fs_edit"))
        assertTrue(tools.contains("fs_read"))
    }

    @Test
    fun deviceAndModelQuestionsGetTheirOwnTools() {
        assertEquals(listOf("device_status"), route("how much free memory do I have").tools)
        assertEquals(listOf("models_list"), route("which models are installed").tools)
    }

    @Test
    fun aReadyReflexRunsForAToolQuestion() {
        val route = route("list the files in my notes")
        assertTrue(route.reflex)
        assertTrue(route.reasons.contains("Needle tries the call first"))
    }

    @Test
    fun aMissingReflexIsSaidOutLoudAndToolsStillOffered() {
        val route = route("list the files in my notes", reflexReady = false)
        assertFalse(route.reflex)
        assertTrue(route.tools.isNotEmpty())
        assertTrue(route.reasons.any { it.contains("not installed") })
    }

    @Test
    fun aReasoningQuestionKeepsToolsButNotNeedle() {
        val route = route("read my notes and compare the trade-off, then explain the root cause in depth")
        assertTrue(route.tools.isNotEmpty())
        assertFalse(route.reflex)
    }

    @Test
    fun reasoningWordsRaiseReasoning() {
        assertTrue(MistyFeatures.of("explain the trade-off and compare the two designs").reasoning >= 0.35)
    }

    @Test
    fun aLongQuestionAddsToReasoning() {
        val long = "word ".repeat(400)
        assertEquals(0.2, MistyFeatures.of(long).reasoning, 0.0001)
    }

    @Test
    fun theOwnersNoToolsVerdictOverridesTheWording() {
        val taught = example("what time is it", FeedbackLabel.NO_TOOLS, emptyList())
        val route = route("what time is it", examples = listOf(taught))
        assertTrue(route.tools.isEmpty())
        assertTrue(route.reasons.single().contains("need no tool"))
    }

    @Test
    fun theOwnersNeedsToolsVerdictOffersThoseTools() {
        val taught = example("hello there friend", FeedbackLabel.NEEDS_TOOLS, listOf("device_status"))
        assertEquals(listOf("device_status"), route("hello there friend", examples = listOf(taught)).tools)
    }

    @Test
    fun aVerdictOnADifferentQuestionDoesNotApply() {
        val taught = example("what time is it", FeedbackLabel.NO_TOOLS, emptyList())
        assertEquals(listOf("calc"), route("what is 17 * 23", examples = listOf(taught)).tools)
    }

    @Test
    fun aBadVerdictDoesNotChangeRouting() {
        val taught = example("what time is it", FeedbackLabel.BAD, emptyList())
        assertEquals(listOf("datetime"), route("what time is it", examples = listOf(taught)).tools)
    }

    private fun example(question: String, label: FeedbackLabel, tools: List<String>) =
        FeedbackExample(question, label, tools, emptyList(), "", 1L)
}
