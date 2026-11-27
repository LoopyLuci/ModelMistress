"""Cuts a reply that is still being written into sentences, so each one can be said as soon as it is complete. A port of the
browser's live/cutter.ts (and the Android SentenceCutter): text inside <think> tags and markdown marks are never said."""

from __future__ import annotations

import re

_THINK_BLOCK = re.compile(r"<think>.*?</think>", re.DOTALL)
_MARKDOWN_MARKS = re.compile(r"[*#`~_|]")
_SPACES = re.compile(r"\s+")
_HAS_WORD = re.compile(r"[^\W_]")  # a letter or a digit


def without_thinking(reply: str) -> str:
    """The reply without its thinking: complete think blocks are removed, and an unfinished one hides the rest."""
    closed = _THINK_BLOCK.sub("", reply)
    open_at = closed.find("<think>")
    return closed[:open_at] if open_at >= 0 else closed


def clean_sentence(piece: str) -> str | None:
    """The text as it is said: markdown marks dropped, spaces collapsed. None when no letter or digit is left."""
    said = _SPACES.sub(" ", _MARKDOWN_MARKS.sub("", piece)).strip()
    return said if _HAS_WORD.search(said) else None


class SentenceCutter:
    def __init__(self) -> None:
        # How much of the visible reply has been cut so far.
        self.spoken_up_to = 0

    def cut(self, reply: str, final: bool = False) -> list[str]:
        """Call with the reply so far each time it grows. Each sentence comes back once. Call it once more with `final`
        set when the reply is finished, which also returns the unfinished last piece."""
        visible = without_thinking(reply)
        # A reply that shrank (a tool step cleared the text) starts again from its beginning.
        if len(visible) < self.spoken_up_to:
            self.spoken_up_to = 0
        out: list[str] = []
        for index in range(self.spoken_up_to, len(visible)):
            c = visible[index]
            ends = False
            if c == "\n":
                ends = True
            elif c in ".!?":
                nxt = visible[index + 1] if index + 1 < len(visible) else None
                ends = final if nxt is None else nxt.isspace()
            if ends:
                said = clean_sentence(visible[self.spoken_up_to : index + 1])
                if said:
                    out.append(said)
                self.spoken_up_to = index + 1
        if final and self.spoken_up_to < len(visible):
            said = clean_sentence(visible[self.spoken_up_to :])
            if said:
                out.append(said)
            self.spoken_up_to = len(visible)
        return out
