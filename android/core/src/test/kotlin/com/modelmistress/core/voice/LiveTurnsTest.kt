package com.modelmistress.core.voice

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class LiveTurnsTest {
    private val turns = LiveTurns()

    private fun begin(): List<LiveEffect> = turns.handle(LiveInput.Start)

    /** The owner's sentence, held and then sent, for tests that are not about the hold. */
    private fun sendSaid(text: String) {
        turns.handle(LiveInput.Said(text))
        turns.handle(LiveInput.HoldEnded)
    }

    @Test
    fun startListensAndShowsListening() {
        assertEquals(LivePhase.STARTING, turns.phase)
        assertEquals(listOf<LiveEffect>(LiveEffect.Listen(0)), begin())
        assertEquals(LivePhase.LISTENING, turns.phase)
    }

    @Test
    fun aFinishedSentenceIsHeldThenSentWhenTheHoldEnds() {
        begin()
        assertEquals(
            listOf(LiveEffect.Hold("what time is it"), LiveEffect.Listen(0)),
            turns.handle(LiveInput.Said("what time is it")),
        )
        assertEquals("what time is it", turns.held)
        assertEquals(listOf<LiveEffect>(LiveEffect.Send("what time is it")), turns.handle(LiveInput.HoldEnded))
        assertEquals(null, turns.held)
        assertEquals(LivePhase.THINKING, turns.phase)
    }

    @Test
    fun aCancelledSentenceIsNotSent() {
        begin()
        turns.handle(LiveInput.Said("the tv says so"))
        assertEquals(emptyList<LiveEffect>(), turns.handle(LiveInput.CancelHold))
        assertEquals(null, turns.held)
        assertEquals(emptyList<LiveEffect>(), turns.handle(LiveInput.HoldEnded))
        assertEquals(LivePhase.LISTENING, turns.phase)
    }

    @Test
    fun moreSpeechWhileASentenceIsHeldJoinsIt() {
        begin()
        turns.handle(LiveInput.Said("what time"))
        assertEquals(
            listOf(LiveEffect.Hold("what time is it"), LiveEffect.Listen(0)),
            turns.handle(LiveInput.Said("is it")),
        )
        assertEquals(listOf<LiveEffect>(LiveEffect.Send("what time is it")), turns.handle(LiveInput.HoldEnded))
    }

    @Test
    fun savingCancelTakesBackTheHeldSentence() {
        begin()
        turns.handle(LiveInput.Said("the news is on"))
        assertEquals(listOf<LiveEffect>(LiveEffect.Listen(0)), turns.handle(LiveInput.Said("cancel that")))
        assertEquals(null, turns.held)
        assertEquals(emptyList<LiveEffect>(), turns.handle(LiveInput.HoldEnded))
    }

    @Test
    fun interruptDropsAHeldSentence() {
        begin()
        turns.handle(LiveInput.Said("hold this"))
        assertEquals(emptyList<LiveEffect>(), turns.handle(LiveInput.Interrupt))
        assertEquals(null, turns.held)
        assertEquals(emptyList<LiveEffect>(), turns.handle(LiveInput.HoldEnded))
    }

    @Test
    fun aReplyIsSaidSentenceBySentenceAndThePhoneKeepsListening() {
        begin()
        sendSaid("what time is it")
        assertEquals(emptyList<LiveEffect>(), turns.handle(LiveInput.Sent(true)))

        assertEquals(
            listOf(LiveEffect.Say("It is noon.")),
            turns.handle(LiveInput.Reply("It is noon. Anything else?", busy = true, error = null)),
        )
        assertEquals(LivePhase.SPEAKING, turns.phase)

        assertEquals(
            listOf(LiveEffect.Say("Anything else?")),
            turns.handle(LiveInput.Reply("It is noon. Anything else?", busy = false, error = null)),
        )
        turns.handle(LiveInput.Spoken)
        assertEquals(LivePhase.SPEAKING, turns.phase)
        turns.handle(LiveInput.Spoken)
        assertEquals(LivePhase.LISTENING, turns.phase)
    }

    @Test
    fun silenceStartsListeningAgainQuickly() {
        begin()
        assertEquals(listOf(LiveEffect.Listen(300)), turns.handle(LiveInput.Said("")))
        assertEquals(LivePhase.LISTENING, turns.phase)
    }

    @Test
    fun transientFailuresAreRetriedThenGiveUp() {
        begin()
        assertEquals(listOf(LiveEffect.Listen(800)), turns.handle(LiveInput.Failed(transient = true)))
        repeat(3) { turns.handle(LiveInput.Failed(transient = true)) }
        val last = turns.handle(LiveInput.Failed(transient = true))
        assertEquals(LivePhase.STOPPED, turns.phase)
        assertEquals("Speech recognition stopped working on this phone.", turns.error)
        assertTrue(last.none { it is LiveEffect.Listen })
    }

    @Test
    fun aPermanentFailureEndsTheConversationWithItsReason() {
        begin()
        val effects = turns.handle(LiveInput.Failed(transient = false, message = "The microphone is not allowed."))
        assertEquals(LivePhase.STOPPED, turns.phase)
        assertEquals("The microphone is not allowed.", turns.error)
        assertEquals(listOf<LiveEffect>(LiveEffect.CancelListening), effects)
    }

    @Test
    fun talkingOverTheReplyStopsItAndSendsWhatIsSaid() {
        begin()
        sendSaid("tell me a story")
        turns.handle(LiveInput.Sent(true))
        turns.handle(LiveInput.Reply("Once upon a time. The end.", busy = true, error = null))
        assertEquals(LivePhase.SPEAKING, turns.phase)

        assertEquals(
            listOf(LiveEffect.StopSpeech, LiveEffect.StopReply),
            turns.handle(LiveInput.Heard("wait stop that")),
        )
        assertEquals(LivePhase.LISTENING, turns.phase)

        // The chat's old reply keeps arriving until it has stopped. None of it is spoken.
        assertEquals(emptyList<LiveEffect>(), turns.handle(LiveInput.Reply("Once upon a time. The end.", busy = true, error = null)))
        assertEquals(emptyList<LiveEffect>(), turns.handle(LiveInput.Reply("Once upon a time. The end.", busy = false, error = null)))

        assertEquals(
            listOf(LiveEffect.Hold("wait stop that"), LiveEffect.Listen(0)),
            turns.handle(LiveInput.Said("wait stop that")),
        )
        assertEquals(listOf<LiveEffect>(LiveEffect.Send("wait stop that")), turns.handle(LiveInput.HoldEnded))
    }

    @Test
    fun whatThePhoneSaysItselfIsNotTakenAsTheOwnersTalk() {
        begin()
        sendSaid("hello")
        turns.handle(LiveInput.Sent(true))
        turns.handle(LiveInput.Reply("Hello there. How can I help you today?", busy = true, error = null))
        assertEquals(LivePhase.SPEAKING, turns.phase)

        // The recogniser hears the sentence being said. Nothing is interrupted and nothing is sent.
        assertEquals(listOf(LiveEffect.Listen(0)), turns.handle(LiveInput.Said("hello there")))
        assertEquals(emptyList<LiveEffect>(), turns.handle(LiveInput.Heard("hello there")))
        assertEquals(LivePhase.SPEAKING, turns.phase)
    }

    @Test
    fun aSentenceSaidWhileTheChatIsWritingWaitsItsTurn() {
        begin()
        sendSaid("first question")
        turns.handle(LiveInput.Sent(true))
        turns.handle(LiveInput.Reply("", busy = true, error = null))

        assertEquals(listOf(LiveEffect.Listen(0)), turns.handle(LiveInput.Said("and a second one")))
        assertEquals(LivePhase.THINKING, turns.phase)

        // Once the reply has finished, the waiting sentence is held like any other, and then sent.
        assertEquals(
            listOf(LiveEffect.Say("Answer one."), LiveEffect.Hold("and a second one")),
            turns.handle(LiveInput.Reply("Answer one.", busy = false, error = null)),
        )
        assertEquals(listOf<LiveEffect>(LiveEffect.Send("and a second one")), turns.handle(LiveInput.HoldEnded))
    }

    @Test
    fun interruptStopsTheReplyAndDropsWhatWasQueued() {
        begin()
        sendSaid("tell me")
        turns.handle(LiveInput.Sent(true))
        turns.handle(LiveInput.Reply("", busy = true, error = null))
        turns.handle(LiveInput.Said("and this too"))

        assertEquals(listOf<LiveEffect>(LiveEffect.StopReply), turns.handle(LiveInput.Interrupt))
        assertEquals(LivePhase.LISTENING, turns.phase)
        assertEquals(emptyList<LiveEffect>(), turns.handle(LiveInput.Reply("", busy = false, error = null)))

        assertEquals(
            listOf(LiveEffect.Hold("new question"), LiveEffect.Listen(0)),
            turns.handle(LiveInput.Said("new question")),
        )
        assertEquals(listOf<LiveEffect>(LiveEffect.Send("new question")), turns.handle(LiveInput.HoldEnded))
    }

    @Test
    fun aMessageTheChatRefusesIsReportedAndListeningGoesOn() {
        begin()
        sendSaid("hello")
        assertEquals(emptyList<LiveEffect>(), turns.handle(LiveInput.Sent(false)))
        assertEquals(LivePhase.LISTENING, turns.phase)
        assertEquals("Your message was not sent. Choose a model in the Chat, then speak again.", turns.error)
    }

    @Test
    fun anOldReplyIsNotSpokenAgain() {
        begin()
        assertEquals(emptyList<LiveEffect>(), turns.handle(LiveInput.Reply("Old answer.", busy = false, error = null)))
    }

    @Test
    fun aReplyThatFailsStillReturnsTheConversationToListening() {
        begin()
        sendSaid("hi")
        turns.handle(LiveInput.Sent(true))
        turns.handle(LiveInput.Reply("", busy = true, error = null))
        turns.handle(LiveInput.Reply(null, busy = false, error = "The model stopped."))
        assertEquals(LivePhase.LISTENING, turns.phase)
        assertEquals("The model stopped.", turns.error)
    }

    @Test
    fun endStopsEverything() {
        begin()
        sendSaid("hi")
        turns.handle(LiveInput.Sent(true))
        turns.handle(LiveInput.Reply("Hello. There", busy = true, error = null))
        assertEquals(
            listOf(LiveEffect.CancelListening, LiveEffect.StopSpeech, LiveEffect.StopReply),
            turns.handle(LiveInput.End),
        )
        assertEquals(LivePhase.STOPPED, turns.phase)
    }

    @Test
    fun micOffStopsListeningAndMicOnStartsItAgain() {
        begin()
        assertEquals(listOf<LiveEffect>(LiveEffect.CancelListening), turns.handle(LiveInput.Mic(false)))
        assertFalse(turns.micOn)
        // While the microphone is off, nothing is taken and the phone does not restart by itself.
        assertEquals(emptyList<LiveEffect>(), turns.handle(LiveInput.Said("hello")))
        assertEquals(emptyList<LiveEffect>(), turns.handle(LiveInput.Failed(transient = true)))
        assertEquals(listOf<LiveEffect>(LiveEffect.Listen(0)), turns.handle(LiveInput.Mic(true)))
        assertTrue(turns.micOn)
    }

    @Test
    fun aReplyKeepsPlayingWhileTheMicrophoneIsOff() {
        begin()
        sendSaid("tell me")
        turns.handle(LiveInput.Sent(true))
        assertEquals(listOf<LiveEffect>(LiveEffect.CancelListening), turns.handle(LiveInput.Mic(false)))
        assertEquals(
            listOf<LiveEffect>(LiveEffect.Say("Here it is.")),
            turns.handle(LiveInput.Reply("Here it is. More.", busy = true, error = null)),
        )
        assertEquals(
            listOf<LiveEffect>(LiveEffect.Say("More.")),
            turns.handle(LiveInput.Reply("Here it is. More.", busy = false, error = null)),
        )
        turns.handle(LiveInput.Spoken)
        turns.handle(LiveInput.Spoken)
        assertEquals(LivePhase.LISTENING, turns.phase)
        assertEquals(listOf<LiveEffect>(LiveEffect.Listen(0)), turns.handle(LiveInput.Mic(true)))
    }

    @Test
    fun aCorrectionInTheSameSentenceIsRecordedAgainstTheLastReply() {
        begin()
        sendSaid("what is the capital of spain")
        turns.handle(LiveInput.Sent(true))
        turns.handle(LiveInput.Reply("Lyon is the capital.", busy = false, error = null))
        turns.handle(LiveInput.Spoken)
        assertEquals(
            listOf(
                LiveEffect.Record("what is the capital of spain", "Lyon is the capital.", "it is Madrid"),
                LiveEffect.Say("Noted."),
                LiveEffect.Listen(0),
            ),
            turns.handle(LiveInput.Said("that's wrong, it is Madrid")),
        )
        assertEquals("Saved. Misty uses it for similar questions.", turns.notice)
    }

    @Test
    fun aCorrectionOnItsOwnTakesTheNextSentence() {
        begin()
        sendSaid("what is the capital of spain")
        turns.handle(LiveInput.Sent(true))
        turns.handle(LiveInput.Reply("Lyon is the capital.", busy = false, error = null))
        turns.handle(LiveInput.Spoken)
        assertEquals(listOf<LiveEffect>(LiveEffect.Listen(0)), turns.handle(LiveInput.Said("that's wrong")))
        assertTrue(turns.awaitingCorrection)
        assertEquals(
            listOf(
                LiveEffect.Record("what is the capital of spain", "Lyon is the capital.", "it is Madrid"),
                LiveEffect.Say("Noted."),
                LiveEffect.Listen(0),
            ),
            turns.handle(LiveInput.Said("it is Madrid")),
        )
        assertFalse(turns.awaitingCorrection)
    }

    @Test
    fun aCorrectionWithNoReplyYetIsSaidAndNotRecorded() {
        begin()
        assertEquals(
            listOf(LiveEffect.Say("There is no reply to correct yet."), LiveEffect.Listen(0)),
            turns.handle(LiveInput.Said("correction it is a cat")),
        )
    }

    @Test
    fun aCorrectionSaidWhileTheReplyIsWrittenIsRecordedWhenItFinishes() {
        begin()
        sendSaid("tell me")
        turns.handle(LiveInput.Sent(true))
        turns.handle(LiveInput.Reply("", busy = true, error = null))
        assertEquals(listOf<LiveEffect>(LiveEffect.Listen(0)), turns.handle(LiveInput.Said("correction it is a cat")))
        assertEquals(
            listOf(
                LiveEffect.Say("It is a dog."),
                LiveEffect.Record("tell me", "It is a dog.", "it is a cat"),
                LiveEffect.Say("Noted."),
            ),
            turns.handle(LiveInput.Reply("It is a dog.", busy = false, error = null)),
        )
    }

    @Test
    fun talkingOverAReplyWithACorrectionRecordsWhatWasHeard() {
        begin()
        sendSaid("tell me a fable")
        turns.handle(LiveInput.Sent(true))
        turns.handle(LiveInput.Reply("Once upon a time. The end.", busy = true, error = null))
        assertEquals(
            listOf(LiveEffect.StopSpeech, LiveEffect.StopReply),
            turns.handle(LiveInput.Heard("that's wrong it's a fable")),
        )
        // A correction is recorded, not held and sent to the chat.
        assertEquals(
            listOf(
                LiveEffect.Record("tell me a fable", "Once upon a time. The end.", "it's a fable"),
                LiveEffect.Say("Noted."),
                LiveEffect.Listen(0),
            ),
            turns.handle(LiveInput.Said("that's wrong it's a fable")),
        )
    }

    @Test
    fun aPhraseInTheMiddleOfASentenceIsStillAMessage() {
        begin()
        assertEquals(
            listOf(LiveEffect.Hold("it is not quite what I wanted"), LiveEffect.Listen(0)),
            turns.handle(LiveInput.Said("it is not quite what I wanted")),
        )
        assertEquals(
            listOf<LiveEffect>(LiveEffect.Send("it is not quite what I wanted")),
            turns.handle(LiveInput.HoldEnded),
        )
    }

    @Test
    fun interruptDropsAWaitingCorrection() {
        begin()
        turns.handle(LiveInput.Said("that's wrong"))
        assertTrue(turns.awaitingCorrection)
        turns.handle(LiveInput.Interrupt)
        assertFalse(turns.awaitingCorrection)
    }

    @Test
    fun theEchoOfAReplyHeardAfterItEndsIsNotTakenAsTheOwnersTalk() {
        begin()
        sendSaid("how are you")
        turns.handle(LiveInput.Sent(true))
        assertEquals(
            listOf<LiveEffect>(LiveEffect.Say("Doing well, thanks for asking.")),
            turns.handle(LiveInput.Reply("Doing well, thanks for asking. How about you?", busy = true, error = null)),
        )
        assertEquals(
            listOf<LiveEffect>(LiveEffect.Say("How about you?")),
            turns.handle(LiveInput.Reply("Doing well, thanks for asking. How about you?", busy = false, error = null)),
        )
        turns.handle(LiveInput.Spoken)
        turns.handle(LiveInput.Spoken)
        assertEquals(LivePhase.LISTENING, turns.phase)

        // The recogniser's result for the phone's own sentence arrives after it has finished speaking.
        assertEquals(
            listOf<LiveEffect>(LiveEffect.Listen(0)),
            turns.handle(LiveInput.Said("doing well thanks for asking how about you")),
        )
        assertEquals("", turns.heard)
        assertEquals(null, turns.held)
        assertEquals(LivePhase.LISTENING, turns.phase)
    }

    @Test
    fun aRecognitionErrorInALongEchoIsStillTheEcho() {
        begin()
        sendSaid("are you there")
        turns.handle(LiveInput.Sent(true))
        turns.handle(LiveInput.Reply("Let me think about that for a moment.", busy = false, error = null))
        assertEquals(
            listOf<LiveEffect>(LiveEffect.Listen(0)),
            turns.handle(LiveInput.Said("let me thinks about that for a moment")),
        )
        assertEquals("", turns.heard)
        assertEquals(null, turns.held)
    }

    @Test
    fun realSpeechThatSharesSomeWordsWithTheReplyIsStillSent() {
        begin()
        sendSaid("what do you think")
        turns.handle(LiveInput.Sent(true))
        turns.handle(LiveInput.Reply("That is a good idea.", busy = false, error = null))
        turns.handle(LiveInput.Spoken)
        assertEquals(
            listOf(LiveEffect.Hold("is it a good idea to go out"), LiveEffect.Listen(0)),
            turns.handle(LiveInput.Said("is it a good idea to go out")),
        )
        assertEquals("is it a good idea to go out", turns.heard)
    }

    @Test
    fun aPartialOfThePhonesOwnVoiceIsNotShownAsTheOwnersWords() {
        begin()
        sendSaid("hi")
        turns.handle(LiveInput.Sent(true))
        turns.handle(LiveInput.Reply("Hello there. How can I help?", busy = true, error = null))
        turns.handle(LiveInput.Heard("hello there"))
        assertEquals("", turns.heard)
        assertEquals(LivePhase.SPEAKING, turns.phase)
    }

    @Test
    fun inEditModeAHeldSentenceWaitsForTheOwnerToSendIt() {
        begin()
        turns.handle(LiveInput.EditBeforeSend(true))
        assertTrue(turns.editBeforeSend)
        assertEquals(
            listOf(LiveEffect.Hold("what time is it", timed = false), LiveEffect.Listen(0)),
            turns.handle(LiveInput.Said("what time is it")),
        )
        assertEquals(emptyList<LiveEffect>(), turns.handle(LiveInput.HoldEnded))
        assertEquals("what time is it", turns.held)
        assertEquals(listOf<LiveEffect>(LiveEffect.Send("what time is it")), turns.handle(LiveInput.SendHeld))
        assertEquals(null, turns.held)
    }

    @Test
    fun theOwnerCanChangeTheHeldSentenceBeforeSendingIt() {
        begin()
        turns.handle(LiveInput.EditBeforeSend(true))
        turns.handle(LiveInput.Said("what time is it"))
        assertEquals(emptyList<LiveEffect>(), turns.handle(LiveInput.EditHeld("what is the time")))
        assertEquals("what is the time", turns.held)
        assertEquals(listOf<LiveEffect>(LiveEffect.Send("what is the time")), turns.handle(LiveInput.SendHeld))
    }

    @Test
    fun speechJoinsTheHeldSentenceInEditMode() {
        begin()
        turns.handle(LiveInput.EditBeforeSend(true))
        turns.handle(LiveInput.Said("what time"))
        assertEquals(
            listOf(LiveEffect.Hold("what time is it", timed = false), LiveEffect.Listen(0)),
            turns.handle(LiveInput.Said("is it")),
        )
        assertEquals("what time is it", turns.held)
    }

    @Test
    fun turningEditModeOffStartsTheHoldTimerAgain() {
        begin()
        turns.handle(LiveInput.EditBeforeSend(true))
        turns.handle(LiveInput.Said("what time is it"))
        assertEquals(
            listOf<LiveEffect>(LiveEffect.Hold("what time is it", timed = true)),
            turns.handle(LiveInput.EditBeforeSend(false)),
        )
        assertEquals(listOf<LiveEffect>(LiveEffect.Send("what time is it")), turns.handle(LiveInput.HoldEnded))
    }

    @Test
    fun aHeldSentenceClearedInEditModeIsNotSent() {
        begin()
        turns.handle(LiveInput.EditBeforeSend(true))
        turns.handle(LiveInput.Said("what time is it"))
        turns.handle(LiveInput.EditHeld(""))
        assertEquals(emptyList<LiveEffect>(), turns.handle(LiveInput.SendHeld))
        assertEquals(null, turns.held)
    }

    @Test
    fun cancelSendStillDropsTheHeldSentenceInEditMode() {
        begin()
        turns.handle(LiveInput.EditBeforeSend(true))
        turns.handle(LiveInput.Said("the news is on"))
        turns.handle(LiveInput.CancelHold)
        assertEquals(null, turns.held)
        assertEquals(emptyList<LiveEffect>(), turns.handle(LiveInput.SendHeld))
    }
}
