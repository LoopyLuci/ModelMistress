"""Sound and pictures for the window: the microphone (sentences the owner finishes), the dictation recorder, the voice that
plays the hub's speech, and the camera's latest picture.

The sentence finding and the WAV files are plain Python and are tested alone. The devices are Qt; they are reached only
when the window uses them, and a computer with no microphone or camera says so instead of failing."""

from __future__ import annotations

import array
import io
import math
import os
import sys
import tempfile
import wave
from typing import Callable

import numpy as np
from PySide6.QtCore import QBuffer, QByteArray, QIODevice, QObject, Qt, QTimer, QUrl, Signal
from PySide6.QtGui import QImage
from PySide6.QtMultimedia import (
    QAudioFormat,
    QAudioSource,
    QCamera,
    QMediaCaptureSession,
    QMediaDevices,
    QMediaPlayer,
    QAudioOutput,
    QVideoSink,
)

from mm_gui import dsp
from mm_gui.segmenter import Segmenter, SegmenterOptions

RATE = 16000  # the sound is 16-bit mono at this rate: what the hub's speech to text takes
FRAME_MS = 100  # the level is read every tenth of a second
PICTURE_SIDE = 768  # a picture for the chat is scaled so that neither side is longer than this


class DeviceError(Exception):
    """A device is missing or cannot be used; the message says so in words for the owner."""


def wav_bytes(pcm: bytes, rate: int = RATE, channels: int = 1) -> bytes:
    """16-bit PCM as a WAV file (what the hub's speech to text reads)."""
    out = io.BytesIO()
    with wave.open(out, "wb") as w:
        w.setnchannels(channels)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(pcm)
    return out.getvalue()


def wav_floats(wav: bytes) -> np.ndarray:
    """A WAV clip as float samples in [-1, 1] at RATE (its first channel). Empty when it is not 16-bit PCM WAV."""
    try:
        with wave.open(io.BytesIO(wav), "rb") as w:
            if w.getsampwidth() != 2 or w.getframerate() <= 0:
                return np.zeros(0)
            channels, rate = w.getnchannels(), w.getframerate()
            raw = w.readframes(w.getnframes())
    except (wave.Error, EOFError):
        return np.zeros(0)
    usable = len(raw) - len(raw) % (2 * channels)
    samples = np.frombuffer(raw[:usable], dtype="<i2").astype(np.float64) / 32768.0
    return dsp.resample(samples[::channels], rate, RATE)


def level(pcm: bytes) -> float:
    """The RMS of 16-bit little-endian samples: 0 for silence, 1 for full scale."""
    samples = array.array("h")
    samples.frombytes(pcm[: len(pcm) - (len(pcm) % 2)])
    if sys.byteorder == "big":
        samples.byteswap()
    if not samples:
        return 0.0
    return math.sqrt(sum(s * s for s in samples) / len(samples)) / 32768.0


class SoundCleaner:
    """Takes the echo and the background out of the microphone's sound: 16-bit mono at RATE in, the same kind out. With an
    alignment, the clip the speaker plays is the echo's reference; without one, only the background is lowered. The output
    comes a block (16 ms) late and in whole blocks, so its length can differ a little from the input's."""

    def __init__(self, alignment: dsp.Alignment | None = None):
        self._alignment = alignment
        self._cleaner = dsp.Cleaner()

    def __call__(self, pcm: bytes) -> bytes:
        mic = np.frombuffer(pcm[: len(pcm) - len(pcm) % 2], dtype="<i2").astype(np.float64) / 32768.0
        ref = self._alignment.reference_for(mic) if self._alignment is not None else None
        out = self._cleaner.push(mic, ref)
        return (np.clip(out, -1.0, 1.0) * 32767.0).astype("<i2").tobytes()


class MicCore:
    """Finds the owner's sentences in the sound, with no device in it. Feed it the sound as it comes (16-bit mono at RATE);
    it calls `on_utterance(pcm, duration_ms)` for each sentence the owner finished. Off, nothing is heard. `clean`, when
    given, takes each piece of sound first (the echo and background cancellation)."""

    def __init__(
        self,
        on_utterance: Callable[[bytes, int], None],
        options: SegmenterOptions | None = None,
        clean: Callable[[bytes], bytes] | None = None,
    ):
        self._on_utterance = on_utterance
        self._clean = clean
        self._seg = Segmenter(options)
        self._frame_bytes = RATE * FRAME_MS // 1000 * 2
        self._pending = bytearray()
        self._clip: bytearray | None = None
        self._time = 0.0  # milliseconds of sound heard, counted from the frames (not the clock)
        self.enabled = False

    def set_enabled(self, on: bool) -> None:
        if on == self.enabled:
            return
        self.enabled = on
        if not on:
            # An utterance in progress is dropped, and nothing is heard until the microphone is on again.
            self._seg.reset()
            self._clip = None
            self._pending.clear()

    def feed(self, pcm: bytes) -> None:
        if self._clean is not None:
            pcm = self._clean(pcm)
        self._pending += pcm
        while len(self._pending) >= self._frame_bytes:
            frame = bytes(self._pending[: self._frame_bytes])
            del self._pending[: self._frame_bytes]
            self._time += FRAME_MS
            self._frame(frame)

    def _frame(self, frame: bytes) -> None:
        if not self.enabled:
            return
        if self._clip is not None:
            self._clip += frame
        event = self._seg.push(level(frame), self._time)
        if event is None:
            return
        if event.kind == "start":
            self._clip = bytearray(frame)
            return
        clip, self._clip = self._clip, None
        if event.keep and clip:
            self._on_utterance(bytes(clip), event.duration_ms)


class AudioInput(QObject):
    """The default microphone, read as it records: `on_chunk(pcm)` gets the sound about every tenth of a second."""

    def __init__(self, on_chunk: Callable[[bytes], None], parent: QObject | None = None):
        super().__init__(parent)
        self._on_chunk = on_chunk
        self._source: QAudioSource | None = None
        self._io = None
        self._timer = QTimer(self)
        self._timer.setInterval(FRAME_MS)
        self._timer.timeout.connect(self._pump)

    def start(self) -> None:
        fmt = QAudioFormat()
        fmt.setSampleRate(RATE)
        fmt.setChannelCount(1)
        fmt.setSampleFormat(QAudioFormat.SampleFormat.Int16)
        device = QMediaDevices.defaultAudioInput()
        if device.isNull():
            raise DeviceError("There is no microphone on this computer.")
        if not device.isFormatSupported(fmt):
            raise DeviceError("The microphone does not record the sound Live needs (16 kHz, mono).")
        self.stop()
        self._source = QAudioSource(device, fmt, self)
        self._io = self._source.start()
        if self._io is None:
            self.stop()
            raise DeviceError("The microphone could not be opened.")
        self._timer.start()

    def _pump(self) -> None:
        if self._io is None:
            return
        data = bytes(self._io.readAll())
        if data:
            self._on_chunk(data)

    def stop(self) -> None:
        self._timer.stop()
        if self._source is not None:
            self._source.stop()
            self._source.deleteLater()
        self._source = None
        self._io = None


class Microphone(QObject):
    """The microphone for Live: its sound is cleaned (the echo of the speaker's `alignment`, and the background), then goes
    through MicCore, which says when the owner has finished a sentence."""

    def __init__(
        self,
        on_utterance: Callable[[bytes, int], None],
        alignment: dsp.Alignment | None = None,
        parent: QObject | None = None,
    ):
        super().__init__(parent)
        self._core = MicCore(on_utterance, clean=SoundCleaner(alignment))
        self._input = AudioInput(self._core.feed, self)

    def open(self) -> None:
        """Starts reading the microphone. DeviceError when there is none, or it cannot be used."""
        self._input.start()

    def set_enabled(self, on: bool) -> None:
        self._core.set_enabled(on)

    def close(self) -> None:
        self._core.set_enabled(False)
        self._input.stop()


class Dictation(QObject):
    """Records what the owner says, all of it, until stopped. Stopping gives the recording as WAV bytes."""

    def __init__(self, parent: QObject | None = None):
        super().__init__(parent)
        self._chunks: list[bytes] = []
        self._input = AudioInput(self._chunks.append, self)

    def start(self) -> None:
        self._chunks = []
        self._input.start()

    def stop(self) -> bytes:
        self._input.stop()
        pcm, self._chunks = b"".join(self._chunks), []
        return wav_bytes(pcm)


class Speaker(QObject):
    """Plays a WAV clip through the default output. `play` calls `on_done(error)` once the clip has ended (error is None)
    or could not be played; `stop` ends it without calling back. The clip is written to a temporary file and played from
    there: Qt's player cannot read a clip from memory on Windows ("Could not open file"), and the file is deleted once the
    player has let go of it."""

    def __init__(self, parent: QObject | None = None):
        super().__init__(parent)
        self._player = QMediaPlayer(self)
        self._output = QAudioOutput(self)
        self._player.setAudioOutput(self._output)
        self._path: str | None = None  # the clip being played, in a temporary file
        self._stale: list[str] = []  # clip files the player still held when they were due to go; deleted on a later call
        self._on_done: Callable[[str | None], None] | None = None
        # What is playing, lined up with the microphone: the echo canceller's reference. The delay to the microphone is
        # measured from the sound, so the player's own latency does not need to be known.
        self.alignment = dsp.Alignment()
        self._player.mediaStatusChanged.connect(self._status)
        self._player.errorOccurred.connect(self._error)

    def play(self, wav: bytes, on_done: Callable[[str | None], None]) -> None:
        self.stop()
        self.alignment.start(wav_floats(wav))
        fd, path = tempfile.mkstemp(prefix="mm-speech-", suffix=".wav")
        with os.fdopen(fd, "wb") as f:
            f.write(wav)
        self._path = path
        self._on_done = on_done
        self._player.setSource(QUrl.fromLocalFile(path))
        self._player.play()

    def stop(self) -> None:
        self.alignment.stop()
        self._on_done = None
        self._player.stop()
        self._discard()

    def _discard(self) -> None:
        """Deletes the clip's file once the player has let go of it. The player is pointed at nothing first, since Windows
        will not delete a file that is open; a file still refused is deleted on a later call."""
        self._player.setSource(QUrl())
        if self._path is not None:
            self._stale.append(self._path)
            self._path = None
        remaining = []
        for path in self._stale:
            try:
                os.remove(path)
            except FileNotFoundError:
                pass
            except OSError:
                remaining.append(path)
        self._stale = remaining

    def _status(self, status) -> None:
        if status == QMediaPlayer.MediaStatus.EndOfMedia:
            self._finish(None)

    def _error(self, _code, message: str = "") -> None:
        self._finish(message or "The speech could not be played.")

    def _finish(self, error: str | None) -> None:
        self.alignment.stop()
        self._discard()  # before the callback, which may start the next clip
        callback, self._on_done = self._on_done, None
        if callback is not None:
            callback(error)


def png_bytes(image: QImage) -> bytes:
    """A picture as PNG bytes, scaled down so that neither side is longer than PICTURE_SIDE."""
    if max(image.width(), image.height()) > PICTURE_SIDE:
        image = image.scaled(PICTURE_SIDE, PICTURE_SIDE, Qt.AspectRatioMode.KeepAspectRatio)
    data = QByteArray()
    buffer = QBuffer(data)
    buffer.open(QIODevice.OpenModeFlag.WriteOnly)
    image.save(buffer, "PNG")
    buffer.close()
    return bytes(data)


class Camera(QObject):
    """The default camera. `latest_png()` gives the newest picture, and `frame` is emitted with each new one."""

    frame = Signal(object)  # the QImage of each new picture

    def __init__(self, parent: QObject | None = None):
        super().__init__(parent)
        self._camera: QCamera | None = None
        self._session = QMediaCaptureSession(self)
        self._sink = QVideoSink(self)
        self._session.setVideoSink(self._sink)
        self._sink.videoFrameChanged.connect(self._new_frame)
        self._image: QImage | None = None

    def start(self) -> None:
        device = QMediaDevices.defaultVideoInput()
        if device.isNull():
            raise DeviceError("There is no camera on this computer.")
        self.stop()
        self._camera = QCamera(device, self)
        self._session.setCamera(self._camera)
        self._camera.start()

    def stop(self) -> None:
        if self._camera is not None:
            self._camera.stop()
            self._camera.deleteLater()
        self._camera = None
        self._image = None

    def latest_png(self) -> bytes | None:
        return png_bytes(self._image) if self._image is not None else None

    def _new_frame(self, frame) -> None:
        image = frame.toImage()
        if not image.isNull():
            self._image = image
            self.frame.emit(image)
