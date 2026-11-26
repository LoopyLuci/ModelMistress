"""Tests for the native echo and noise cancellation (mm_gui.dsp). Every signal is synthetic and seeded, so the numbers repeat
on every run. The thresholds are the ones the module is held to: echo return loss enhancement of at least 12 dB on a linear
echo, the owner's voice kept while the reply plays, at least 6 dB of steady-noise reduction, and faster than real time."""

from __future__ import annotations

import time

import numpy as np
import pytest

from mm_gui import dsp

RATE = dsp.RATE
BLOCK = dsp.BLOCK


def coloured(rng: np.random.Generator, n: int, level: float = 1.0, a: float = 0.7) -> np.ndarray:
    """Coloured noise, scaled to an RMS of `level`: a stand-in for speech spectra."""
    white = rng.standard_normal(n)
    out = np.empty(n)
    acc = 0.0
    for i in range(n):
        acc = a * acc + white[i]
        out[i] = acc
    return level * out / np.sqrt(np.mean(out ** 2))


def speechlike(rng: np.random.Generator, n: int, level: float) -> np.ndarray:
    """Coloured noise with a syllable-like envelope (about four bursts a second)."""
    t = np.arange(n) / RATE
    envelope = np.clip(np.sin(2 * np.pi * 4 * t) + 0.6, 0, None)
    x = coloured(rng, n) * envelope
    return level * x / np.sqrt(np.mean(x ** 2))


def echo_path(rng: np.random.Generator, delay: int = 320, length: int = 1600, rt: float = 0.12) -> np.ndarray:
    """A room: a path that starts 20 ms late and decays over about 120 ms, with the energy of 0.7 RMS."""
    h = np.zeros(length)
    t = np.arange(length - delay) / RATE
    h[delay:] = rng.standard_normal(length - delay) * np.exp(-t * np.log(1000) / rt)
    h[delay] = 1.0
    return h / np.sqrt(np.sum(h ** 2)) * 0.7


def clean_echo(ref: np.ndarray, mic: np.ndarray) -> tuple[np.ndarray, dsp.EchoCanceller]:
    ec = dsp.EchoCanceller()
    out = np.zeros(len(mic))
    for i in range(0, len(mic), BLOCK):
        out[i:i + BLOCK] = ec.process(mic[i:i + BLOCK], ref[i:i + BLOCK])
    return out, ec


def erle_db(mic: np.ndarray, out: np.ndarray, start: int, end: int) -> float:
    return 10 * np.log10(np.mean(mic[start:end] ** 2) / np.mean(out[start:end] ** 2))


def snr_db(signal: np.ndarray, error: np.ndarray) -> float:
    return 10 * np.log10(np.mean(signal ** 2) / np.mean(error ** 2))


# -- echo cancellation --------------------------------------------------------------------------------------------------

def test_a_linear_echo_of_white_sound_is_reduced_by_12_db():
    rng = np.random.default_rng(1)
    n = RATE * 8
    ref = rng.standard_normal(n) * 0.1
    mic = np.convolve(ref, echo_path(rng))[:n]
    out, _ = clean_echo(ref, mic)
    assert erle_db(mic, out, n - 2 * RATE, n) >= 12.0


def test_a_linear_echo_of_speech_like_sound_is_reduced_by_12_db():
    rng = np.random.default_rng(3)
    n = RATE * 8
    ref = speechlike(rng, n, 0.1)
    mic = np.convolve(ref, echo_path(rng))[:n]
    out, _ = clean_echo(ref, mic)
    assert erle_db(mic, out, n - 2 * RATE, n) >= 12.0


def test_the_owner_talking_over_the_reply_is_kept():
    rng = np.random.default_rng(2)
    n = RATE * 8
    ref = speechlike(rng, n, 0.1)
    echo = np.convolve(ref, echo_path(rng))[:n]
    near = np.zeros(n)
    near[4 * RATE:] = speechlike(rng, n - 4 * RATE, 0.05)
    out, _ = clean_echo(ref, echo + near)
    seg = slice(5 * RATE, n)
    assert snr_db(near[seg], out[seg] - near[seg]) >= 6.0


def test_a_pure_echo_does_not_freeze_the_filter():
    """Regression: a gate that compared the microphone with the filter's own estimate froze the filter on 95% of blocks."""
    rng = np.random.default_rng(4)
    n = RATE * 8
    ref = rng.standard_normal(n) * 0.1
    mic = np.convolve(ref, echo_path(rng))[:n]
    _, ec = clean_echo(ref, mic)
    assert ec.frozen_blocks <= 0.05 * (n // BLOCK)


def test_a_silent_reference_leaves_the_microphone_and_the_filter_alone():
    rng = np.random.default_rng(5)
    mic = rng.standard_normal(BLOCK * 10) * 0.1
    out, ec = clean_echo(np.zeros(len(mic)), mic)
    np.testing.assert_allclose(out, mic)
    assert not ec.W.any()


# -- noise suppression --------------------------------------------------------------------------------------------------

def run_noise(x: np.ndarray, **kwargs) -> np.ndarray:
    ns = dsp.NoiseSuppressor(**kwargs)
    out = np.zeros(len(x))
    for i in range(0, len(x), BLOCK):
        out[i:i + BLOCK] = ns.process(x[i:i + BLOCK])
    return out


@pytest.mark.parametrize("kind", ["white", "mains hum"])
def test_steady_background_is_lowered_by_at_least_6_db(kind):
    rng = np.random.default_rng(6)
    n = RATE * 6
    x = rng.standard_normal(n) * 0.01
    if kind == "mains hum":
        x = x + 0.8 * 0.01 * np.sin(2 * np.pi * 60 * np.arange(n) / RATE)
    out = run_noise(x)
    reduction = 10 * np.log10(np.mean(x[2 * RATE:] ** 2) / np.mean(out[2 * RATE:] ** 2))
    assert reduction >= 6.0


def test_speech_in_background_keeps_most_of_its_speech():
    rng = np.random.default_rng(7)
    n = RATE * 6
    speech = speechlike(rng, n, 0.1)
    noise = rng.standard_normal(n) * 0.01
    out = run_noise(speech + noise)
    # The output is the input one block later, so compare it with the clean speech one block earlier.
    clean = speech[2 * RATE:n - BLOCK]
    assert snr_db(clean, out[2 * RATE + BLOCK:n] - clean) >= 12.0


def test_at_unity_gain_the_suppressor_gives_the_input_back_one_block_later():
    rng = np.random.default_rng(8)
    n = BLOCK * 62
    x = rng.standard_normal(n) * 0.1
    out = run_noise(x, floor=1.0, oversubtract=0.0)
    np.testing.assert_allclose(out[BLOCK:n], x[:n - BLOCK], atol=1e-9)


# -- the cleaner, the reference and resampling --------------------------------------------------------------------------

def test_the_cleaner_returns_whole_blocks_and_keeps_the_rest():
    c = dsp.Cleaner()
    assert len(c.push(np.zeros(1000), np.zeros(1000))) == 768  # three whole blocks; 232 samples wait
    assert len(c.push(np.zeros(1000), np.zeros(1000))) == 1024  # 232 + 1000 = four more blocks


def test_the_cleaner_rejects_a_reference_that_does_not_cover_the_microphone():
    with pytest.raises(ValueError):
        dsp.Cleaner().push(np.zeros(512), np.zeros(256))


def test_the_cleaner_runs_at_least_four_times_faster_than_real_time():
    rng = np.random.default_rng(9)
    seconds = 10
    n = RATE * seconds
    ref = speechlike(rng, n, 0.1)
    mic = np.convolve(ref, echo_path(rng))[:n] + speechlike(rng, n, 0.1)
    c = dsp.Cleaner()
    start = time.perf_counter()
    for i in range(0, n, 1600):
        c.push(mic[i:i + 1600], ref[i:i + 1600])
    assert time.perf_counter() - start < seconds / 4


def test_the_reference_lines_the_clip_up_with_the_microphone_position():
    r = dsp.Reference()
    r.load(np.arange(1.0, 6.0))  # [1, 2, 3, 4, 5]
    np.testing.assert_array_equal(r.window(3, 3), [1, 2, 3])
    np.testing.assert_array_equal(r.window(7, 4), [4, 5, 0, 0])  # past the end of the clip
    np.testing.assert_array_equal(r.window(2, 4), [0, 0, 1, 2])  # before the start of the clip
    r.clear()
    np.testing.assert_array_equal(r.window(3, 3), [0, 0, 0])


# -- the speaker-to-microphone delay ------------------------------------------------------------------------------------

def test_the_delay_from_the_speaker_to_the_microphone_is_measured():
    rng = np.random.default_rng(10)
    clip = speechlike(rng, RATE * 4, 0.1)
    delay = int(0.2 * RATE)  # 200 ms: the player's buffers and the room
    mic = np.concatenate([np.zeros(delay), 0.5 * clip])[: len(clip)]
    al = dsp.Alignment()
    al.start(clip)
    for i in range(0, len(mic), 1600):  # the microphone arrives in 100 ms pieces
        al.reference_for(mic[i:i + 1600])
    # The reference is taken early on purpose (see Alignment): a few samples either side of the true delay is correct.
    assert al.lag is not None
    assert abs(al.lag - (delay - dsp.LAG_EARLY)) <= 4


def test_the_reference_is_silence_until_the_delay_is_known_and_when_nothing_plays():
    al = dsp.Alignment()
    assert not al.reference_for(np.ones(100)).any()  # nothing has started
    al.start(np.ones(1000))
    assert not al.reference_for(np.ones(100)).any()  # started, but no delay is known yet
    assert al.lag is None and al.playing
    al.stop()
    assert not al.playing and not al.reference_for(np.ones(100)).any()


def test_the_reference_is_the_clip_at_the_measured_delay():
    rng = np.random.default_rng(12)
    clip = speechlike(rng, RATE * 4, 0.1)
    delay = int(0.2 * RATE)
    mic = np.concatenate([np.zeros(delay), 0.5 * clip])[: len(clip)]
    al = dsp.Alignment()
    al.start(clip)
    for i in range(0, len(mic), 1600):
        al.reference_for(mic[i:i + 1600])
    fed = len(mic)
    ref = al.reference_for(np.zeros(1600))
    expected = clip[fed - al.lag: fed - al.lag + 1600]
    np.testing.assert_allclose(ref, expected)


def test_a_microphone_that_does_not_hear_the_clip_gives_no_delay():
    rng = np.random.default_rng(13)
    clip = speechlike(rng, RATE * 4, 0.1)
    al = dsp.Alignment()
    al.start(clip)
    for _ in range(0, RATE * 4, 1600):
        al.reference_for(rng.standard_normal(1600) * 0.1)  # sound from somewhere else entirely
    assert al.lag is None


def test_the_echo_of_a_clip_is_cancelled_once_its_delay_is_measured():
    rng = np.random.default_rng(14)
    n = RATE * 8
    clip = speechlike(rng, n, 0.1)
    room = echo_path(rng)
    room[320] += 2.0  # a speaker near the microphone: its first arrival is strong, as on a laptop (a bare random tail
    room = room / np.linalg.norm(room) * 0.7  # is harder: about 9 dB in some seeds)
    mic = np.convolve(clip, room)[:n]  # the room: the clip as the microphone hears it
    al = dsp.Alignment()
    al.start(clip)
    cleaner = dsp.Cleaner(noise=False)
    out = []
    for i in range(0, n, 1600):
        out.append(cleaner.push(mic[i:i + 1600], al.reference_for(mic[i:i + 1600])))
    out = np.concatenate(out)
    assert al.lag is not None
    assert erle_db(mic, out, n - 2 * RATE, n) >= 12.0


def test_resampling_keeps_a_tone_and_changes_the_length():
    t = np.arange(8000) / 8000
    tone = np.sin(2 * np.pi * 100 * t)
    up = dsp.resample(tone, 8000, RATE)
    assert len(up) == RATE
    expected = np.sin(2 * np.pi * 100 * np.arange(RATE) / RATE)
    np.testing.assert_allclose(up[1000:-1000], expected[1000:-1000], atol=0.02)
    np.testing.assert_array_equal(dsp.resample(tone, 8000, 8000), tone)
