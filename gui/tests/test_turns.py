"""The rules of a live conversation, with no window: the same conversations as the browser's turns.test.ts (and the
Android LiveTurnsTest), with the same effects expected."""

from __future__ import annotations

import pytest

from mm_gui.turns import GAVE_UP, NOT_SENT, NO_REPLY, NOTED, SAVED, Effect, LiveTurns


def listen(delay: int = 0) -> Effect:
    return Effect("listen", delay_ms=delay)


def hold(text: str, timed: bool = True) -> Effect:
    return Effect("hold", text=text, timed=timed)


def send(text: str) -> Effect:
    return Effect("send", text=text)


def says(sentence: str) -> Effect:
    return Effect("say", text=sentence)


def record(question: str, answer: str, correction: str) -> Effect:
    return Effect("record", question=question, answer=answer, correction=correction)


STOP_SPEECH = Effect("stopSpeech")
STOP_REPLY = Effect("stopReply")
CANCEL_LISTENING = Effect("cancelListening")


@pytest.fixture
def turns() -> LiveTurns:
    return LiveTurns()


@pytest.fixture
def live(turns: LiveTurns) -> LiveTurns:
    turns.start()
    return turns


def send_said(t: LiveTurns, text: str) -> None:
    """The owner's sentence, held and then sent, for tests that are not about the hold."""
    t.said(text)
    t.hold_ended()


def test_starts_listening_and_shows_listening(turns):
    assert turns.phase == "starting"
    assert turns.start() == [listen(0)]
    assert turns.phase == "listening"


def test_holds_a_finished_sentence_then_sends_it_when_the_hold_ends(live):
    assert live.said("what time is it") == [hold("what time is it"), listen(0)]
    assert live.held == "what time is it"
    assert live.hold_ended() == [send("what time is it")]
    assert live.held is None
    assert live.phase == "thinking"


def test_does_not_send_a_sentence_that_was_cancelled(live):
    live.said("the tv says so")
    assert live.cancel_hold() == []
    assert live.held is None
    assert live.hold_ended() == []
    assert live.phase == "listening"


def test_joins_more_speech_to_a_sentence_that_is_held(live):
    live.said("what time")
    assert live.said("is it") == [hold("what time is it"), listen(0)]
    assert live.hold_ended() == [send("what time is it")]


def test_takes_back_the_held_sentence_when_the_owner_says_cancel(live):
    live.said("the news is on")
    assert live.said("cancel that") == [listen(0)]
    assert live.held is None
    assert live.hold_ended() == []


def test_drops_a_held_sentence_when_the_owner_interrupts(live):
    live.said("hold this")
    assert live.interrupt() == []
    assert live.held is None
    assert live.hold_ended() == []


def test_says_a_reply_sentence_by_sentence_and_keeps_listening(live):
    send_said(live, "what time is it")
    assert live.sent(True) == []
    assert live.reply("It is noon. Anything else?", True) == [says("It is noon.")]
    assert live.phase == "speaking"
    assert live.reply("It is noon. Anything else?", False) == [says("Anything else?")]
    live.spoken_one()
    assert live.phase == "speaking"
    live.spoken_one()
    assert live.phase == "listening"


def test_starts_listening_again_quickly_after_silence(live):
    assert live.said("") == [listen(300)]
    assert live.phase == "listening"


def test_retries_transient_failures_then_gives_up(live):
    assert live.failed(True) == [listen(800)]
    for _ in range(3):
        live.failed(True)
    last = live.failed(True)
    assert live.phase == "stopped"
    assert live.error == GAVE_UP
    assert not any(e.kind == "listen" for e in last)


def test_ends_the_conversation_with_the_reason_for_a_permanent_failure(live):
    effects = live.failed(False, "The microphone is not allowed.")
    assert live.phase == "stopped"
    assert live.error == "The microphone is not allowed."
    assert effects == [CANCEL_LISTENING]


def test_stops_the_reply_when_the_owner_talks_over_it_and_sends_what_is_said(live):
    send_said(live, "tell me a story")
    live.sent(True)
    live.reply("Once upon a time. The end.", True)
    assert live.phase == "speaking"

    assert live.heard("wait stop that") == [STOP_SPEECH, STOP_REPLY]
    assert live.phase == "listening"

    # The chat's old reply keeps arriving until it has stopped. None of it is spoken.
    assert live.reply("Once upon a time. The end.", True) == []
    assert live.reply("Once upon a time. The end.", False) == []

    assert live.said("wait stop that") == [hold("wait stop that"), listen(0)]
    assert live.hold_ended() == [send("wait stop that")]


def test_does_not_take_what_the_computer_says_itself_as_the_owner_talking(live):
    send_said(live, "hello")
    live.sent(True)
    live.reply("Hello there. How can I help you today?", True)
    assert live.phase == "speaking"
    # The recogniser hears the sentence being said. Nothing is interrupted and nothing is sent.
    assert live.said("hello there") == [listen(0)]
    assert live.heard("hello there") == []
    assert live.phase == "speaking"


def test_holds_a_sentence_said_while_the_chat_is_writing_until_its_turn_comes(live):
    send_said(live, "first question")
    live.sent(True)
    live.reply("", True)
    assert live.said("and a second one") == [listen(0)]
    assert live.phase == "thinking"
    assert live.reply("Answer one.", False) == [says("Answer one."), hold("and a second one")]
    assert live.hold_ended() == [send("and a second one")]


def test_stops_the_reply_on_interrupt_and_drops_what_was_queued(live):
    send_said(live, "tell me")
    live.sent(True)
    live.reply("", True)
    live.said("and this too")
    assert live.interrupt() == [STOP_REPLY]
    assert live.phase == "listening"
    assert live.reply("", False) == []
    assert live.said("new question") == [hold("new question"), listen(0)]
    assert live.hold_ended() == [send("new question")]


def test_reports_a_message_the_chat_refused_and_goes_on_listening(live):
    send_said(live, "hello")
    assert live.sent(False) == []
    assert live.phase == "listening"
    assert live.error == NOT_SENT


def test_does_not_say_an_old_reply_again(live):
    assert live.reply("Old answer.", False) == []


def test_returns_to_listening_when_a_reply_fails(live):
    send_said(live, "hi")
    live.sent(True)
    live.reply("", True)
    live.reply(None, False, "The model stopped.")
    assert live.phase == "listening"
    assert live.error == "The model stopped."


def test_stops_everything_at_the_end(live):
    send_said(live, "hi")
    live.sent(True)
    live.reply("Hello. There", True)
    assert live.end() == [CANCEL_LISTENING, STOP_SPEECH, STOP_REPLY]
    assert live.phase == "stopped"


def test_stops_listening_with_the_microphone_off_and_starts_again_when_it_is_on(live):
    assert live.set_mic(False) == [CANCEL_LISTENING]
    assert live.mic_on is False
    # While the microphone is off, nothing is taken and the computer does not restart by itself.
    assert live.said("hello") == []
    assert live.failed(True) == []
    assert live.set_mic(True) == [listen(0)]
    assert live.mic_on is True


def test_keeps_a_reply_playing_while_the_microphone_is_off(live):
    send_said(live, "tell me")
    live.sent(True)
    assert live.set_mic(False) == [CANCEL_LISTENING]
    assert live.reply("Here it is. More.", True) == [says("Here it is.")]
    assert live.reply("Here it is. More.", False) == [says("More.")]
    live.spoken_one()
    live.spoken_one()
    assert live.phase == "listening"
    assert live.set_mic(True) == [listen(0)]


def test_records_a_correction_in_the_same_sentence_against_the_last_reply(live):
    send_said(live, "what is the capital of spain")
    live.sent(True)
    live.reply("Lyon is the capital.", False)
    live.spoken_one()
    assert live.said("that's wrong, it is Madrid") == [
        record("what is the capital of spain", "Lyon is the capital.", "it is Madrid"),
        says(NOTED),
        listen(0),
    ]
    assert live.notice == SAVED


def test_takes_the_next_sentence_as_the_correction_when_the_phrase_comes_on_its_own(live):
    send_said(live, "what is the capital of spain")
    live.sent(True)
    live.reply("Lyon is the capital.", False)
    live.spoken_one()
    assert live.said("that's wrong") == [listen(0)]
    assert live.awaiting_correction is True
    assert live.said("it is Madrid") == [
        record("what is the capital of spain", "Lyon is the capital.", "it is Madrid"),
        says(NOTED),
        listen(0),
    ]
    assert live.awaiting_correction is False


def test_says_there_is_no_reply_to_correct_yet_and_records_nothing(live):
    assert live.said("correction it is a cat") == [says(NO_REPLY), listen(0)]


def test_records_a_correction_said_while_the_reply_is_written_when_the_reply_finishes(live):
    send_said(live, "tell me")
    live.sent(True)
    live.reply("", True)
    assert live.said("correction it is a cat") == [listen(0)]
    assert live.reply("It is a dog.", False) == [
        says("It is a dog."),
        record("tell me", "It is a dog.", "it is a cat"),
        says(NOTED),
    ]


def test_records_what_was_heard_when_the_owner_talks_over_a_reply_with_a_correction(live):
    send_said(live, "tell me a fable")
    live.sent(True)
    live.reply("Once upon a time. The end.", True)
    assert live.heard("that's wrong it's a fable") == [STOP_SPEECH, STOP_REPLY]
    # A correction is recorded, not held and sent to the chat.
    assert live.said("that's wrong it's a fable") == [
        record("tell me a fable", "Once upon a time. The end.", "it's a fable"),
        says(NOTED),
        listen(0),
    ]


def test_keeps_a_phrase_in_the_middle_of_a_sentence_as_a_message(live):
    assert live.said("it is not quite what I wanted") == [hold("it is not quite what I wanted"), listen(0)]
    assert live.hold_ended() == [send("it is not quite what I wanted")]


def test_drops_a_waiting_correction_on_interrupt(live):
    live.said("that's wrong")
    assert live.awaiting_correction is True
    live.interrupt()
    assert live.awaiting_correction is False


def test_does_not_take_the_echo_of_a_reply_heard_after_it_ends_as_the_owner_talk(live):
    send_said(live, "how are you")
    live.sent(True)
    assert live.reply("Doing well, thanks for asking. How about you?", True) == [says("Doing well, thanks for asking.")]
    assert live.reply("Doing well, thanks for asking. How about you?", False) == [says("How about you?")]
    live.spoken_one()
    live.spoken_one()
    assert live.phase == "listening"
    # The recogniser's result for the computer's own sentence arrives after it has finished speaking.
    assert live.said("doing well thanks for asking how about you") == [listen(0)]
    assert live.heard_text == ""
    assert live.held is None
    assert live.phase == "listening"


def test_still_takes_a_long_echo_with_one_recognition_error_as_the_echo(live):
    send_said(live, "are you there")
    live.sent(True)
    live.reply("Let me think about that for a moment.", False)
    assert live.said("let me thinks about that for a moment") == [listen(0)]
    assert live.heard_text == ""
    assert live.held is None


def test_still_sends_real_speech_that_shares_some_words_with_the_reply(live):
    send_said(live, "what do you think")
    live.sent(True)
    live.reply("That is a good idea.", False)
    live.spoken_one()
    assert live.said("is it a good idea to go out") == [hold("is it a good idea to go out"), listen(0)]
    assert live.heard_text == "is it a good idea to go out"


def test_does_not_show_a_part_of_the_computer_own_voice_as_the_owners_words(live):
    send_said(live, "hi")
    live.sent(True)
    live.reply("Hello there. How can I help?", True)
    live.heard("hello there")
    assert live.heard_text == ""
    assert live.phase == "speaking"


class TestEditBeforeSend:
    def test_keeps_a_held_sentence_until_the_owner_sends_it(self, live):
        live.set_edit_before_send(True)
        assert live.edit_before_send is True
        assert live.said("what time is it") == [hold("what time is it", False), listen(0)]
        assert live.hold_ended() == []
        assert live.held == "what time is it"
        assert live.send_held() == [send("what time is it")]
        assert live.held is None

    def test_lets_the_owner_change_the_held_sentence_before_sending_it(self, live):
        live.set_edit_before_send(True)
        live.said("what time is it")
        assert live.edit_held("what is the time") == []
        assert live.held == "what is the time"
        assert live.send_held() == [send("what is the time")]

    def test_joins_speech_to_the_held_sentence_in_edit_mode(self, live):
        live.set_edit_before_send(True)
        live.said("what time")
        assert live.said("is it") == [hold("what time is it", False), listen(0)]
        assert live.held == "what time is it"

    def test_starts_the_hold_timer_again_when_edit_mode_is_turned_off(self, live):
        live.set_edit_before_send(True)
        live.said("what time is it")
        assert live.set_edit_before_send(False) == [hold("what time is it", True)]
        assert live.hold_ended() == [send("what time is it")]

    def test_does_not_send_a_held_sentence_that_was_cleared(self, live):
        live.set_edit_before_send(True)
        live.said("what time is it")
        live.edit_held("")
        assert live.send_held() == []
        assert live.held is None

    def test_drops_the_held_sentence_on_cancel_send_even_in_edit_mode(self, live):
        live.set_edit_before_send(True)
        live.said("the news is on")
        live.cancel_hold()
        assert live.held is None
        assert live.send_held() == []
