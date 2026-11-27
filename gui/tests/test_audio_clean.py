"""The sound Live carries: a WAV clip as samples, the cleaning of the microphone's sound, the microphone core's cleaning
step, and the speaker handing the clip it plays to the echo canceller. The cleaning is bytes in and bytes out, so it needs
no device; the speaker is only started, and the test ends before any sound would be heard."""

from __future__ import annotations

import os
import time

import numpy as np
import pytest
from PySide6.QtMultimedia import QMediaDevices
from PySide6.QtWidgets import QApplication

from mm_gui import dsp
from mm_gui.audio import RATE, MicCore, SoundCleaner, Speaker, wav_bytes, wav_floats


def pcm_of(samples) -> bytes:
    return (np.clip(np.asarray(samples, dtype=np.float64), -1.0, 1.0) * 32767).astype("<i2").tobytes()


def floats_of(pcm: bytes) -> np.ndarray:
    return np.frombuffer(pcm, dtype="<i2").astype(np.float64) / 32768.0


@pytest.fixture(scope="module")
def app():
    return QApplication.instance() or QApplication([])


def test_a_wav_clip_comes_back_as_samples_at_the_microphone_rate():
    tone = 0.5 * np.sin(2 * np.pi * 440 * np.arange(24000) / 24000)
    samples = wav_floats(wav_bytes(pcm_of(tone), rate=24000))
    assert len(samples) == RATE
    expected = 0.5 * np.sin(2 * np.pi * 440 * np.arange(RATE) / RATE)
    np.testing.assert_allclose(samples[500:-500], expected[500:-500], atol=0.01)


def test_something_that_is_not_a_16_bit_wav_gives_no_samples():
    assert wav_floats(b"not a wav file").size == 0


def test_the_cleaner_lowers_background_sound_when_there_is_no_echo_reference():
    rng = np.random.default_rng(20)
    noise = rng.standard_normal(RATE * 4) * 0.01
    cleaned = floats_of(SoundCleaner()(pcm_of(noise)))
    assert len(cleaned) > 0
    reduction = 10 * np.log10(np.mean(noise[2 * RATE:len(cleaned)] ** 2) / np.mean(cleaned[2 * RATE:] ** 2))
    assert reduction >= 6.0


def test_the_cleaner_takes_the_echo_out_once_it_has_the_clip_the_speaker_plays():
    rng = np.random.default_rng(21)
    n = RATE * 8
    clip = np.convolve(rng.standard_normal(n), np.ones(8) / 8, mode="same") * 0.1  # coloured, like speech
    room = np.zeros(1600)
    room[320] = 0.5
    room[321:] = rng.standard_normal(1600 - 321) * np.exp(-np.arange(1600 - 321) / 400) * 0.05
    mic = np.convolve(clip, room)[:n]
    alignment = dsp.Alignment()
    alignment.start(clip)
    cleaner = SoundCleaner(alignment)
    out = []
    for i in range(0, n, 1600):
        out.append(floats_of(cleaner(pcm_of(mic[i:i + 1600]))))
    out = np.concatenate(out)
    tail = slice(n - 2 * RATE, len(out))
    erle = 10 * np.log10(np.mean(mic[n - 2 * RATE:len(out)] ** 2) / np.mean(out[tail] ** 2))
    assert erle >= 12.0


def test_the_microphone_core_cleans_each_piece_of_sound_before_it_finds_sentences():
    cleaned_sizes: list[int] = []

    def clean(pcm: bytes) -> bytes:
        cleaned_sizes.append(len(pcm))
        return pcm

    core = MicCore(lambda pcm, ms: None, clean=clean)
    core.set_enabled(True)
    core.feed(b"\x00\x00" * 1600)
    core.feed(b"\x00\x00" * 800)
    assert cleaned_sizes == [3200, 1600]


def test_the_speaker_hands_the_clip_it_plays_to_the_alignment(app):
    speaker = Speaker()
    assert not speaker.alignment.playing
    speaker.play(wav_bytes(pcm_of(np.zeros(RATE))), lambda error: None)
    assert speaker.alignment.playing
    speaker.stop()
    assert not speaker.alignment.playing


def test_the_speaker_plays_its_clip_from_a_file_and_deletes_the_file_when_the_clip_ends(app):
    if not QMediaDevices.audioOutputs():
        pytest.skip("this computer has no sound output")
    speaker = Speaker()
    done: list = []
    speaker.play(wav_bytes(pcm_of(np.zeros(RATE // 4))), done.append)  # a quarter second of silence
    path = speaker._path
    assert path is not None and os.path.exists(path)
    end = time.monotonic() + 10
    while not done and time.monotonic() < end:
        app.processEvents()
        time.sleep(0.01)
    assert done == [None], f"the clip did not play: {done}"
    assert not os.path.exists(path)
