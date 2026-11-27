"""The rules of a live conversation, with no window in them. A port of the browser's live/turns.ts, which ports the Android
LiveTurns.

The microphone listens for the whole conversation, not only between replies, unless the owner has turned it off. A
sentence the owner finishes is held for a moment, so the owner can cancel it, and then sent to the chat. The reply is said
sentence by sentence, and the owner can talk over it to stop it. What the computer says is taken out of what it hears, so
its own voice does not start a turn. Talk that arrives while the chat is still writing waits its turn, and then is held too.

The recogniser cannot tell the owner from other voices in the room, so a held sentence can be taken back: the owner
presses Cancel send, or says "cancel", "never mind" or "forget it" as the next sentence.

With edit before send on, a held sentence is not sent by the timer. The owner can change its text first, and sends it
with Send now.

A sentence that starts with a correction phrase ("correction", "that's wrong", "not quite") is not sent to the chat. It is
recorded against the last reply the owner heard: the words after the phrase are the correction, or, when there are none,
the next sentence is. A correction said while a reply is still being written waits until that reply has finished.

Call the methods for each input in order, and carry out the effects they return.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Callable

from mm_gui.cutter import SentenceCutter

RESTART_MS = 300
RETRY_MS = 800
MAX_FAILURES = 4
# Share of words that must be the computer's own, for a short utterance to count as its echo.
ECHO_SHARE = 0.9
# For a longer utterance, a lower share is enough: a recognition error is then expected.
ECHO_LONG_WORDS = 5
ECHO_LONG_SHARE = 0.7
# How many of the computer's recent words are kept to recognise its own voice.
SPOKEN_WORDS = 160
# How long a finished sentence waits, so the owner can take it back.
HOLD_MS = 1500
GAVE_UP = "Speech recognition stopped working on this computer."
NOT_SENT = "Your message was not sent. Choose a model in the Chat, then speak again."
NO_REPLY = "There is no reply to correct yet."
NOTED = "Noted."
SAVED = "Saved. Misty uses it for similar questions."

_NOT_A_WORD = re.compile(r"[\W_]+")
# A correction phrase at the start of a sentence, after any short lead-in such as "no" or "wait". Only the start counts,
# so "it is not quite what I meant" is still a message. The apostrophe may be straight or curly.
_CORRECTION_START = re.compile(
    r"^(?:(?:no|wait|actually|sorry|hang on|hold on|um|uh|well|hmm)\b[\s,.:;!?-]*)*"
    r"(?:correction|not quite|(?:that[’']?s|that is|you[’']?re|you are) (?:wrong|not right))\b[\s,.:;!?-]*",
    re.IGNORECASE,
)
# Words that take back a held sentence, at its start: "cancel", "never mind", "forget it", "ignore that".
_CANCEL_START = re.compile(r"^(?:cancel|never mind|forget (?:it|that)|ignore that)\b", re.IGNORECASE)


def words(text: str) -> list[str]:
    return [w for w in _NOT_A_WORD.split(text.lower()) if w]


def _join(first: str | None, second: str) -> str:
    return second if not first else f"{first} {second}"


@dataclass(frozen=True)
class Effect:
    """What the window does next. `kind` is one of: listen, cancelListening, hold, send, stopReply, say, stopSpeech, record.
    `delay_ms` is how long to wait before listening again (listen); `text` is the sentence (hold, send, say); `timed` says a
    hold ends by itself; `question`, `answer` and `correction` are for a record."""

    kind: str
    text: str = ""
    timed: bool = False
    delay_ms: int = 0
    question: str = ""
    answer: str = ""
    correction: str = ""


class LiveTurns:
    def __init__(self) -> None:
        self.begun = False
        self.active = False
        self.listening = False
        self.listen_delay = 0
        self.failures = 0
        # Whether the chat is writing a reply, as far as the window has reported it.
        self.chat_busy = False
        # A turn the owner started is not finished: it was sent, or it waits to be sent.
        self.replying = False
        # The chat has begun the turn. Until it has, a finished reply in the chat is the one from before.
        self.turn_started = False
        # The chat is still finishing a reply that was stopped. Its text is not said.
        self.stale = False
        # What the owner said while a reply was running, to hold and send once it has finished.
        self.waiting: str | None = None
        self.cutter = SentenceCutter()
        # Sentences handed to speech that have not been said yet.
        self.to_say = 0
        # The most recent words the computer has said, in order, so its own voice is not taken as the owner's.
        self.spoken: list[str] = []
        # The question of the turn the chat was last sent, and the reply so far for the turn in progress.
        self.turn_question = ""
        self.turn_reply = ""
        # The turn the owner heard last, as (question, reply). A correction refers to it.
        self.last_turn: tuple[str, str] | None = None
        # A correction said while a reply was still being written. It is recorded once that reply has finished.
        self.queued_correction: str | None = None

        # The owner said only the correction phrase. The next sentence is the correction.
        self.awaiting_correction = False
        # Whether the microphone is on. When it is off nothing is heard, but a reply that is running still plays.
        self.mic_on = True
        # A finished sentence that waits to be sent, so the owner can cancel it, or None.
        self.held: str | None = None
        # Whether the owner sends held sentences by hand. On, the hold timer does not send them.
        self.edit_before_send = False
        # What was saved last, for the screen, or None.
        self.notice: str | None = None
        # What the owner said, as it is heard, for the screen.
        self.heard_text = ""
        # What went wrong, for the screen, or None.
        self.error: str | None = None

    @property
    def phase(self) -> str:
        if not self.begun:
            return "starting"
        if not self.active:
            return "stopped"
        if self.to_say > 0:
            return "speaking"
        if self.replying:
            return "thinking"
        return "listening"

    # ---- the inputs -------------------------------------------------------------------------------------------------

    def start(self) -> list[Effect]:
        return self._step(self._start)

    def heard(self, text: str) -> list[Effect]:
        """The recogniser's words so far, while the owner is still speaking."""
        return self._step(lambda out: self._heard(text.strip(), out))

    def said(self, text: str) -> list[Effect]:
        """The recogniser's words for one utterance. Blank when nothing was heard."""
        return self._step(lambda out: self._on_said(text.strip(), out))

    def failed(self, transient: bool, message: str | None = None) -> list[Effect]:
        """The recogniser failed. A transient failure is retried a few times in a row; a permanent one ends the conversation."""
        return self._step(lambda out: self._failed(transient, message, out))

    def sent(self, started: bool) -> list[Effect]:
        """The chat was asked to answer. `started` is False when it did not start the reply."""
        return self._step(lambda out: self._on_sent(started, out))

    def reply(self, text: str | None, busy: bool, error: str | None = None) -> list[Effect]:
        """The chat's reply as it stands. `busy` says whether it is still being written."""
        return self._step(lambda out: self._on_reply(text, busy, error, out))

    def spoken_one(self) -> list[Effect]:
        """One sentence has been said, or could not be."""
        return self._step(lambda out: self._decrement_speech())

    def interrupt(self) -> list[Effect]:
        """The owner pressed Interrupt."""
        return self._step(self._interrupt)

    def set_mic(self, on: bool) -> list[Effect]:
        """The owner turned the microphone on or off. Off stops listening; a reply that is running keeps playing."""
        return self._step(lambda out: self._mic(on, out))

    def hold_ended(self) -> list[Effect]:
        """The hold on a held sentence has run out, so it is sent unless it was cancelled."""
        return self._step(self._hold_ended)

    def cancel_hold(self) -> list[Effect]:
        """The owner pressed Cancel send."""
        return self._step(self._cancel_hold)

    def set_edit_before_send(self, on: bool) -> list[Effect]:
        return self._step(lambda out: self._edit_before_send_changed(on, out))

    def edit_held(self, text: str) -> list[Effect]:
        """The owner changed the held sentence in the box."""
        return self._step(lambda out: self._edit_held(text))

    def send_held(self) -> list[Effect]:
        """The owner pressed Send now on the held sentence."""
        return self._step(self._send_held)

    def end(self) -> list[Effect]:
        """The owner ended the conversation."""
        return self._step(self._end)

    # ---- the steps --------------------------------------------------------------------------------------------------

    def _step(self, fn: Callable[[list[Effect]], object]) -> list[Effect]:
        out: list[Effect] = []
        fn(out)
        if self.active and self.mic_on and not self.listening:
            self.listening = True
            out.append(Effect("listen", delay_ms=self.listen_delay))
            self.listen_delay = 0
        return out

    def _start(self, out: list[Effect]) -> None:
        self.begun = True
        self.active = True
        self.error = None
        self.spoken = []

    def _heard(self, text: str, out: list[Effect]) -> None:
        if self.active and self.mic_on:
            self.failures = 0
            # The computer's own voice is not shown as the owner's words.
            self.heard_text = "" if self._echo_like(text) else text
            # Talking over the computer while it is speaking stops the reply. While the chat is only writing, the words wait.
            if self.to_say > 0 and self._interrupts(text):
                self._interrupt_reply(out)

    def _on_said(self, text: str, out: list[Effect]) -> None:
        self.listening = False
        if not self.active or not self.mic_on:
            return
        self.failures = 0
        if text == "":
            self.heard_text = ""
            self.listen_delay = RESTART_MS
            return
        # The computer hearing its own voice is not the owner talking, whether or not it is still speaking.
        if self._echo_like(text):
            self.heard_text = ""
            return
        self.heard_text = text
        self.error = None
        self._on_sentence(text, out)

    def _on_sentence(self, text: str, out: list[Effect]) -> None:
        # The owner takes back a sentence that is being held by saying so. Other sentences are not taken back.
        if self.held is not None and _CANCEL_START.search(text):
            self.held = None
            return
        found = self._correction_in(text)
        if found is None and not self.awaiting_correction:
            self._send_or_wait(text, out)
            return
        self.awaiting_correction = False
        correction = found if found is not None else text
        if correction == "":
            # Only the phrase was said. The next sentence is the correction.
            self.awaiting_correction = True
            return
        if self.to_say > 0:
            self._interrupt_reply(out)
        if self.replying:
            self.queued_correction = _join(self.queued_correction, correction)
        else:
            self._correct(correction, out)

    def _failed(self, transient: bool, message: str | None, out: list[Effect]) -> None:
        self.begun = True
        if not transient:
            self.error = message if message is not None else GAVE_UP
            self._end(out)
            return
        self.listening = False
        # A failure while the microphone is off is the recogniser stopping, not a fault to retry.
        if not self.active or not self.mic_on:
            return
        self.failures += 1
        if self.failures > MAX_FAILURES:
            self.error = GAVE_UP
            self._end(out)
            return
        self.listen_delay = RETRY_MS

    def _on_sent(self, started: bool, out: list[Effect]) -> None:
        if started:
            self.chat_busy = True
            self.turn_started = True
            # The owner stopped the turn before the chat began it: stop the reply that has just started.
            if not self.replying:
                self._stop_chat(out)
            return
        self.replying = False
        self.turn_started = False
        self.waiting = None
        self.error = NOT_SENT
        queued, self.queued_correction = self.queued_correction, None
        if queued is not None:
            self._correct(queued, out)

    def _on_reply(self, text: str | None, busy: bool, error: str | None, out: list[Effect]) -> None:
        self.chat_busy = busy
        if self.stale:
            # A reply the owner stopped. Its text is ignored until the chat has finished it.
            if not busy:
                self.stale = False
                self._resume(out)
            return
        if not self.replying:
            return
        if busy:
            self.turn_started = True
            if text is not None:
                self.turn_reply = text
                self._say(self.cutter.cut(text), out)
            return
        # The chat has not begun this turn yet: what it holds is the reply from before.
        if not self.turn_started:
            return
        finished = text if text else None
        if finished is not None:
            self._say(self.cutter.cut(finished, final=True), out)
        if error is not None:
            self.error = error
        self.last_turn = (self.turn_question, finished) if finished is not None and finished.strip() != "" else None
        queued, self.queued_correction = self.queued_correction, None
        self.replying = False
        self.turn_started = False
        self.turn_reply = ""
        self.cutter = SentenceCutter()
        if queued is not None:
            self._correct(queued, out)
        self._resume(out)

    def _decrement_speech(self) -> None:
        if self.to_say > 0:
            self.to_say -= 1

    def _interrupt(self, out: list[Effect]) -> None:
        if self.active:
            self.waiting = None
            self.held = None
            self.queued_correction = None
            self.awaiting_correction = False
            self._interrupt_reply(out)

    def _mic(self, on: bool, out: list[Effect]) -> None:
        self.mic_on = on
        if not on:
            self.heard_text = ""
            if self.listening:
                self.listening = False
                out.append(Effect("cancelListening"))

    def _hold_ended(self, out: list[Effect]) -> None:
        if not self.edit_before_send:
            self._send_held(out)

    def _cancel_hold(self, out: list[Effect]) -> None:
        self.held = None

    def _edit_held(self, text: str) -> None:
        if self.held is not None:
            self.held = text

    def _send_or_wait(self, text: str, out: list[Effect]) -> None:
        """Holds the owner's sentence before it is sent, or queues it while a reply is being written. Talking over a reply stops it."""
        if self.to_say > 0:
            self._interrupt_reply(out)
        if self.replying:
            self.waiting = _join(self.waiting, text)
            return
        self._hold(text, out)

    def _hold(self, text: str, out: list[Effect]) -> None:
        """Holds `text` before it is sent. More speech joins the held sentence and extends the hold."""
        now = _join(self.held, text)
        self.held = now
        out.append(Effect("hold", text=now, timed=not self.edit_before_send))

    def _send_held(self, out: list[Effect]) -> None:
        """Sends the held sentence as it now stands. A blank one is dropped."""
        text, self.held = self.held, None
        if text is None or text.strip() == "":
            return
        self._start_turn(text.strip(), out)

    def _edit_before_send_changed(self, on: bool, out: list[Effect]) -> None:
        """Turned off, a sentence still held is sent when its timer runs out, as before."""
        self.edit_before_send = on
        text = self.held
        if text is None or on:
            return
        if text.strip() == "":
            self.held = None
        else:
            out.append(Effect("hold", text=text, timed=True))

    def _start_turn(self, text: str, out: list[Effect]) -> None:
        self.replying = True
        self.turn_started = False
        self.turn_reply = ""
        self.cutter = SentenceCutter()
        self.notice = None
        if self.chat_busy or self.stale:
            self.waiting = _join(self.waiting, text)
        else:
            self.turn_question = text
            out.append(Effect("send", text=text))

    def _resume(self, out: list[Effect]) -> None:
        """Holds what was said while the chat was busy, once the chat is free."""
        nxt, self.waiting = self.waiting, None
        if nxt is not None:
            self._hold(nxt, out)

    def _correct(self, correction: str, out: list[Effect]) -> None:
        """Records `correction` against the last turn the owner heard. Only called when no reply is being written."""
        turn = self.last_turn
        if turn is None:
            self._say([NO_REPLY], out)
            return
        out.append(Effect("record", question=turn[0], answer=turn[1], correction=correction))
        self.notice = SAVED
        self._say([NOTED], out)

    def _say(self, sentences: list[str], out: list[Effect]) -> None:
        for sentence in sentences:
            self.to_say += 1
            self._remember(words(sentence))
            out.append(Effect("say", text=sentence))

    def _remember(self, said: list[str]) -> None:
        """Keeps the most recent words the computer has said."""
        self.spoken.extend(said)
        if len(self.spoken) > SPOKEN_WORDS:
            del self.spoken[: len(self.spoken) - SPOKEN_WORDS]

    def _interrupt_reply(self, out: list[Effect]) -> None:
        """Stops speech and the chat's reply. What the owner said after that is still heard and sent."""
        if self.to_say > 0:
            self.to_say = 0
            out.append(Effect("stopSpeech"))
        # What the owner heard of the reply, before talking over it, is the reply a correction refers to.
        if self.replying and self.turn_reply.strip() != "":
            self.last_turn = (self.turn_question, self.turn_reply)
        self._stop_chat(out)
        self.replying = False
        self.turn_started = False
        self.turn_reply = ""
        self.cutter = SentenceCutter()

    def _stop_chat(self, out: list[Effect]) -> None:
        if self.chat_busy and not self.stale:
            self.stale = True
            out.append(Effect("stopReply"))

    def _end(self, out: list[Effect]) -> None:
        if self.listening:
            self.listening = False
            out.append(Effect("cancelListening"))
        if self.to_say > 0:
            self.to_say = 0
            out.append(Effect("stopSpeech"))
        self._stop_chat(out)
        self.active = False
        self.replying = False
        self.turn_started = False
        self.waiting = None
        self.held = None
        self.queued_correction = None
        self.awaiting_correction = False
        self.spoken = []
        self.cutter = SentenceCutter()

    def _correction_in(self, text: str) -> str | None:
        """The correction a sentence starts with, or None when it does not start with one."""
        m = _CORRECTION_START.match(text)
        if m is None:
            return None
        return text[m.end() :].strip()

    def _echoed(self, all_words: list[str]) -> list[bool]:
        """Marks each word that is part of a pair of words the computer has just said, one after the other. A pair is the
        smallest unit that counts, so one common word such as "you" does not mark anything by itself."""
        marks = [False] * len(all_words)
        if len(self.spoken) < 2:
            return marks
        said = {f"{self.spoken[i]} {self.spoken[i + 1]}" for i in range(len(self.spoken) - 1)}
        for i in range(len(all_words) - 1):
            if f"{all_words[i]} {all_words[i + 1]}" in said:
                marks[i] = True
                marks[i + 1] = True
        return marks

    def _echo_like(self, text: str) -> bool:
        """True when the words are the computer's own voice: nearly all of them run on from what it has just said. A
        recognition error breaks only the pairs around it, so a long echo with one wrong word still counts."""
        all_words = words(text)
        if not all_words:
            return True
        share = sum(self._echoed(all_words)) / len(all_words)
        return share >= ECHO_SHARE or (len(all_words) >= ECHO_LONG_WORDS and share >= ECHO_LONG_SHARE)

    def _interrupts(self, text: str) -> bool:
        """True when the owner is talking over the computer: at least two words that are not its own voice."""
        all_words = words(text)
        own = self._echoed(all_words)
        return sum(1 for m in own if not m) >= 2 and not self._echo_like(text)
