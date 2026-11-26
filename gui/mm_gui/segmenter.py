"""Finds where the owner's sentences start and end in the sound from the microphone, from the level of each short frame of
it (voice activity detection). Pure: the audio glue feeds it the level of a frame every tenth of a second, and it says
when an utterance begins and when it has ended. A port of the browser's live/segmenter.ts."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class SegmenterOptions:
    # The level (0 to 1, the RMS of a frame) at or above which a frame is speech.
    threshold: float = 0.02
    # How long the quiet must last to end an utterance.
    silence_ms: int = 700
    # An utterance shorter than this is noise (a click, a cough) and is dropped.
    min_ms: int = 300
    # An utterance is ended at this length, however long the owner talks.
    max_ms: int = 20000


@dataclass(frozen=True)
class SegmentEvent:
    """`start`, or `end` with how long the utterance was (from its first speech frame to its last) and whether to keep it."""

    kind: str
    duration_ms: int = 0
    keep: bool = False


class Segmenter:
    def __init__(self, options: SegmenterOptions | None = None) -> None:
        self.o = options or SegmenterOptions()
        self.speaking = False
        self.started_at = 0.0
        self.last_voice = 0.0

    def push(self, level: float, now: float) -> SegmentEvent | None:
        """Feed the level of one frame, with the time it was taken (milliseconds). Returns an event when one begins or ends."""
        if not self.speaking:
            if level < self.o.threshold:
                return None
            self.speaking = True
            self.started_at = now
            self.last_voice = now
            return SegmentEvent("start")
        if level >= self.o.threshold:
            self.last_voice = now
        quiet = now - self.last_voice
        length = now - self.started_at
        if quiet < self.o.silence_ms and length < self.o.max_ms:
            return None
        return self._finish()

    def reset(self) -> SegmentEvent | None:
        """Ends the utterance in progress, if there is one (the microphone was turned off, or the conversation ended)."""
        return self._finish() if self.speaking else None

    def _finish(self) -> SegmentEvent:
        self.speaking = False
        duration = self.last_voice - self.started_at
        return SegmentEvent("end", duration_ms=int(duration), keep=duration >= self.o.min_ms)
