package com.modelmistress.core.voice

import org.junit.Assert.assertEquals
import org.junit.Test

class SentenceCutterTest {
    @Test
    fun aSentenceIsSpokenOnceItIsFinished() {
        val cutter = SentenceCutter()
        assertEquals(emptyList<String>(), cutter.cut("Hello there"))
        assertEquals(emptyList<String>(), cutter.cut("Hello there."))
        assertEquals(listOf("Hello there."), cutter.cut("Hello there. "))
        assertEquals(emptyList<String>(), cutter.cut("Hello there. How are you?"))
        assertEquals(listOf("How are you?"), cutter.cut("Hello there. How are you? Fine"))
        assertEquals(listOf("Fine"), cutter.cut("Hello there. How are you? Fine", final = true))
    }

    @Test
    fun thinkingIsNeverSpoken() {
        val cutter = SentenceCutter()
        assertEquals(emptyList<String>(), cutter.cut("<think>Let me work this out. More"))
        assertEquals(listOf("The answer is 4."), cutter.cut("<think>Let me work this out.</think>The answer is 4. "))
    }

    @Test
    fun markdownMarksAreDropped() {
        val cutter = SentenceCutter()
        assertEquals(listOf("Bold text here."), cutter.cut("**Bold** text here. ", final = false))
    }

    @Test
    fun decimalNumbersAreNotSplit() {
        val cutter = SentenceCutter()
        assertEquals(listOf("Pi is 3.14 today."), cutter.cut("Pi is 3.14 today.", final = true))
    }

    @Test
    fun aReplyThatStartsAgainIsCutFromTheStart() {
        val cutter = SentenceCutter()
        assertEquals(listOf("First one."), cutter.cut("First one. Second"))
        // A tool step cleared the text, so the new text is read from its beginning.
        assertEquals(emptyList<String>(), cutter.cut("New"))
        assertEquals(listOf("New text."), cutter.cut("New text.", final = true))
    }

    @Test
    fun aLineEndCountsAsASentenceEnd() {
        val cutter = SentenceCutter()
        assertEquals(listOf("First line"), cutter.cut("First line\nSecond"))
    }
}
