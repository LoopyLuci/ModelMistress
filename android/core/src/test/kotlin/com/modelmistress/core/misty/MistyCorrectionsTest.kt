package com.modelmistress.core.misty

import com.modelmistress.core.json.Json
import java.io.File
import java.nio.file.Files
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class MistyCorrectionsTest {
    private fun example(
        question: String,
        label: FeedbackLabel,
        at: Long = 1,
        answer: String = "An answer.",
        tools: List<String> = emptyList(),
        correction: String? = null,
        camera: String? = null,
    ) = FeedbackExample(question, label, tools, emptyList(), answer, at, correction, camera)

    @Test
    fun correctionsApplyOnlyToSimilarQuestions() {
        val examples = listOf(
            example("what is the capital of spain", FeedbackLabel.CORRECTED, correction = "It is Madrid."),
            example("what is the weather tomorrow", FeedbackLabel.CORRECTED, correction = "It will rain."),
        )
        assertEquals(listOf("It is Madrid."), MistyLearning.corrections("capital of spain please", examples).map { it.correction })
    }

    @Test
    fun aCorrectionWithNoTextIsNotUsed() {
        val examples = listOf(example("what is the capital of spain", FeedbackLabel.CORRECTED, correction = " "))
        assertTrue(MistyLearning.corrections("capital of spain", examples).isEmpty())
    }

    @Test
    fun theNewestOfEqualCorrectionsComesFirstAndOnlyTheLimitIsKept() {
        val examples = listOf(
            example("capital of spain", FeedbackLabel.CORRECTED, at = 1, correction = "old"),
            example("capital of spain", FeedbackLabel.CORRECTED, at = 3, correction = "newest"),
            example("capital of spain", FeedbackLabel.CORRECTED, at = 2, correction = "middle"),
        )
        assertEquals(listOf("newest", "middle"), MistyLearning.corrections("capital of spain", examples).map { it.correction })
    }

    @Test
    fun aCorrectionNeverChangesTheRoute() {
        val examples = listOf(example("is it raining in london", FeedbackLabel.CORRECTED, correction = "No, it is dry."))
        assertNull(MistyLearning.taught("is it raining in london", examples))
    }

    @Test
    fun theRoutingVerdictsStillApply() {
        val good = example("what time is it", FeedbackLabel.GOOD, tools = listOf("clock"))
        assertEquals(good, MistyLearning.taught("what time is it", listOf(good)))
    }

    @Test
    fun correctionsSurviveAReloadAndOlderLinesStillLoad() {
        val file = File(Files.createTempDirectory("misty-feedback").toFile(), "feedback.jsonl")
        // A line written before corrections existed has no correction or camera field.
        file.writeText("""{"question":"what is this","label":"good","tools":[],"calls":[],"answer":"A cup.","at":5}""" + "\n")
        val store = FileMistyFeedback(file)
        val old = store.examples().single()
        assertNull(old.correction)
        assertNull(old.camera)

        val corrected = example(
            "what is this",
            FeedbackLabel.CORRECTED,
            at = 9,
            answer = "A bowl.",
            correction = "It is a cup.",
            camera = "What the camera shows now:\ncup, table",
        )
        store.add(corrected)
        assertEquals(listOf(old, corrected), store.examples())
    }

    @Test
    fun theTrainingFileHoldsTheAcceptedAnswersAndTheCorrectionsOnly() {
        val examples = listOf(
            example("what is the capital of spain", FeedbackLabel.GOOD, answer = "Madrid."),
            example("is it raining", FeedbackLabel.BAD, answer = "Yes."),
            example("hello", FeedbackLabel.NO_TOOLS, answer = "Hi."),
            example("what is this", FeedbackLabel.CORRECTED, answer = "A bowl.", correction = "It is a cup.", camera = "cup, table"),
        )
        val lines = MistyTraining.lines(examples)
        assertEquals(2, lines.size)

        val good = Json.parse(lines[0]) as Map<*, *>
        assertEquals("Madrid.", (good["messages"] as List<*>)[1].contentOf())

        val corrected = Json.parse(lines[1]) as Map<*, *>
        val messages = corrected["messages"] as List<*>
        assertEquals("what is this\n\ncup, table", messages[0].contentOf())
        assertEquals("It is a cup.", messages[1].contentOf())
        val metadata = corrected["metadata"] as Map<*, *>
        assertEquals("corrected", metadata["label"])
        assertEquals("A bowl.", metadata["rejected"])
    }

    @Test
    fun aCorrectionWithNoTextIsNotExported() {
        val examples = listOf(example("what is this", FeedbackLabel.CORRECTED, correction = ""))
        assertTrue(MistyTraining.lines(examples).isEmpty())
    }

    private fun Any?.contentOf(): Any? = (this as Map<*, *>)["content"]
}
