package com.modelmistress.core.misty

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File
import java.nio.file.Files

class MistyFeedbackTest {
    private fun store(capacity: Int = FileMistyFeedback.DEFAULT_CAPACITY): FileMistyFeedback =
        FileMistyFeedback(File(Files.createTempDirectory("misty-feedback").toFile(), "feedback.jsonl"), capacity)

    private fun example(question: String, label: FeedbackLabel, at: Long = 1L, answer: String = "") =
        FeedbackExample(question, label, listOf("datetime"), listOf("datetime:ran"), answer, at)

    @Test
    fun savedExamplesComeBackInOrderWithEverything() {
        val feedback = store()
        val answer = "He said \"hi\",\nthen left (see [note])"
        feedback.add(example("what time is it", FeedbackLabel.GOOD, at = 5L, answer = answer))
        feedback.add(example("hello", FeedbackLabel.NO_TOOLS, at = 6L))

        val read = feedback.examples()
        assertEquals(2, read.size)
        assertEquals("what time is it", read[0].question)
        assertEquals(FeedbackLabel.GOOD, read[0].label)
        assertEquals(listOf("datetime"), read[0].tools)
        assertEquals(listOf("datetime:ran"), read[0].calls)
        assertEquals(answer, read[0].answer)
        assertEquals(5L, read[0].at)
        assertEquals(FeedbackLabel.NO_TOOLS, read[1].label)
    }

    @Test
    fun theNewestExamplesAreKeptWhenTheStoreIsFull() {
        val feedback = store(capacity = 2)
        feedback.add(example("one", FeedbackLabel.GOOD))
        feedback.add(example("two", FeedbackLabel.GOOD))
        feedback.add(example("three", FeedbackLabel.GOOD))

        assertEquals(listOf("two", "three"), feedback.examples().map { it.question })
    }

    @Test
    fun clearRemovesEverything() {
        val feedback = store()
        feedback.add(example("hello", FeedbackLabel.GOOD))
        feedback.clear()
        assertTrue(feedback.examples().isEmpty())
    }

    @Test
    fun aMissingFileHasNoExamples() {
        assertTrue(store().examples().isEmpty())
    }

    @Test
    fun similarQuestionsShareMostOfTheirWords() {
        assertTrue(MistyLearning.similarity("what time is it?", "what time is it") >= MistyLearning.SIMILAR)
        assertTrue(MistyLearning.similarity("hello", "hello there") < MistyLearning.SIMILAR)
        assertEquals(0.0, MistyLearning.similarity("hello", "what is 2 plus 2"), 0.0)
    }

    @Test
    fun theNewestSimilarVerdictWinsOverAnOlderOne() {
        val older = example("what time is it", FeedbackLabel.NO_TOOLS, at = 1L)
        val newer = example("what time is it", FeedbackLabel.NEEDS_TOOLS, at = 2L)
        assertEquals(FeedbackLabel.NEEDS_TOOLS, MistyLearning.taught("what time is it", listOf(older, newer))?.label)
    }

    @Test
    fun badVerdictsAndUnrelatedQuestionsAreNotTaught() {
        val bad = example("what time is it", FeedbackLabel.BAD)
        assertNull(MistyLearning.taught("what time is it", listOf(bad)))
        assertNull(MistyLearning.taught("what is 2 plus 2", listOf(example("what time is it", FeedbackLabel.NO_TOOLS))))
        assertFalse(MistyLearning.words("What's the TIME?").contains("what"))
    }
}
