"""The hub's speech for the window: dictation (speech to text) and read-aloud (text to speech). Each call returns a pair
(kind, value) instead of raising, so that a worker thread hands back a result the window can show: OK with the value,
UNAVAILABLE when this build of the hub has no speech (404 or 501), or ERROR with the reason."""

from __future__ import annotations

from mm_gui.hub import Hub, HubError

OK = "ok"
UNAVAILABLE = "unavailable"
ERROR = "error"
UNAVAILABLE_TEXT = "Speech to text is not in this build of the hub."
NO_SPEECH_TEXT = "Speech is not in this build of the hub."


def _failure(e: HubError) -> tuple[str, str]:
    return (UNAVAILABLE if e.status in (404, 501) else ERROR), str(e)


def transcribe_result(hub: Hub, wav: bytes) -> tuple[str, str]:
    """The words in a WAV clip: (OK, text), or (UNAVAILABLE | ERROR, reason)."""
    try:
        return OK, hub.transcribe(wav)
    except HubError as e:
        return _failure(e)


def speak_result(hub: Hub, text: str) -> tuple[str, bytes | str]:
    """`text` as WAV bytes: (OK, wav), or (UNAVAILABLE | ERROR, reason)."""
    try:
        return OK, hub.speak(text)
    except HubError as e:
        return _failure(e)
