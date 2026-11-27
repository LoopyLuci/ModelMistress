"""Echo and background-noise cancellation for the microphone of Live, in numpy, with no device in it.

Two stages, in order:

1. **Echo cancellation.** The reply the computer says comes back into the microphone through the room. A partitioned
   block frequency-domain adaptive filter (PBFDAF) learns the path from the reference (the clip the speaker plays, lined
   up with the microphone by a measured delay, `Alignment`) to the microphone and subtracts its estimate. It stops
   adapting while the owner talks over the reply (double talk), so the owner's voice is not learned away.
2. **Noise suppression.** A spectral subtraction per short frame, from a noise floor tracked over time, takes out steady
   background sound (a fan, traffic) that is quieter than speech. The gain is smoothed over frames and kept above a floor.

Both work on float samples in [-1, 1] at 16 kHz (RATE). The filter is linear and adaptive: it is strong on a room and a
small speaker, and weaker on a loud or distorting one. A browser's own echo canceller (WebRTC's, in Chromium) is
stronger there, which is why the web Live asks the browser for it too. A headset removes the echo path entirely."""

from __future__ import annotations

import math
from collections import deque

import numpy as np

RATE = 16000
BLOCK = 256  # 16 ms: the adaptation and the noise frames advance this far at a time
PARTITIONS = 24  # 24 x 16 ms: the filter covers an echo path up to 384 ms long
_SILENT = 1e-7  # mean square of a reference block below which the speaker is taken as silent
_SILENT_PEAK = 1e-3  # peak of a reference over the filter's span below which there is no echo to learn from
_STEP_REG = 1e-3  # added to the per-bin energy before the step divides by it (a bin of a 0.001-RMS reference has about 5e-4)
_DIVERGED_GAIN = 2.0  # the filter is started again when its error is this many times the microphone's energy


class EchoCanceller:
    """Removes the reference from the microphone. `process(mic, ref)` takes one block of each (BLOCK samples) and returns the
    microphone with the estimated echo taken out."""

    def __init__(self, block: int = BLOCK, partitions: int = PARTITIONS, mu: float = 0.25):
        self.block = block
        self.parts = partitions
        self.mu = mu  # step size: a larger one converges faster but lets more of the owner's voice into the filter
        self.frozen_blocks = 0  # blocks where double talk froze the filter, for diagnosis and tests
        self.resets = 0  # times the filter was started again because it had made the microphone louder
        bins = block + 1
        self.W = np.zeros((partitions, bins), dtype=np.complex128)  # the filter, one partition per block of delay
        self.X = np.zeros((partitions, bins), dtype=np.complex128)  # spectra of the reference, newest first
        self.prev_ref = np.zeros(block)
        self.power = np.zeros(bins)  # smoothed reference energy inside the filter's window, per frequency: the step divides by it
        self.s_mic = 0.0  # smoothed energies, for the echo return loss enhancement (ERLE)
        self.s_err = 0.0
        self.ref_peaks: deque[float] = deque(maxlen=partitions)  # the reference's peak in each block the filter spans
        self.dt_gain = 1.5  # double talk when the microphone peak exceeds this times the reference peak (echo peaks are below 1)

    def process(self, mic: np.ndarray, ref: np.ndarray) -> np.ndarray:
        b = self.block
        x = np.concatenate([self.prev_ref, ref])  # overlap-save: the last block and this one
        self.prev_ref = ref
        spectrum = np.fft.rfft(x)
        self.X = np.roll(self.X, 1, axis=0)
        self.X[0] = spectrum
        # The reference's energy inside the filter's window, per frequency, summed over the partitions.
        inst = np.sum(np.abs(self.X) ** 2, axis=0)
        self.power = 0.8 * self.power + 0.2 * inst

        echo = np.fft.irfft(np.sum(self.X * self.W, axis=0), 2 * b)[b:]
        err = mic - echo

        e_mic = float(np.mean(mic * mic))
        self.s_mic = 0.95 * self.s_mic + 0.05 * e_mic
        self.s_err = 0.95 * self.s_err + 0.05 * float(np.mean(err * err))
        # A filter that makes the microphone clearly louder than it was has learned a wrong path: start it again from silence.
        if self.s_mic > _SILENT and self.s_err > _DIVERGED_GAIN * self.s_mic:
            self.W[:] = 0.0
            self.resets += 1
        # Double talk (Geigel): the microphone's peak is louder than the echo could be, given the loudest reference the
        # filter spans. The test is the signals' own peaks, never the filter's estimate: a filter that under-estimates the
        # echo must not be able to freeze itself. A louder block is the owner talking, so the filter does not learn it.
        self.ref_peaks.append(float(np.max(np.abs(ref))))
        double_talk = max(self.ref_peaks) > _SILENT_PEAK and float(np.max(np.abs(mic))) > self.dt_gain * max(self.ref_peaks)
        if double_talk:
            self.frozen_blocks += 1
        if float(np.mean(ref * ref)) > _SILENT and not double_talk:
            grad = np.conj(self.X) * np.fft.rfft(np.concatenate([np.zeros(b), err]))
            taps = np.fft.irfft(grad, 2 * b, axis=1)[:, :b]  # keep the first block of each partition's gradient
            # The step is divided, per frequency bin, by the larger of the smoothed and the current reference energy, plus an
            # absolute regulariser. The smoothed energy lags a speech onset, so dividing by it alone overshoots; the
            # regulariser stops a quiet reference from making the step unbounded.
            denom = np.maximum(self.power, inst) + _STEP_REG
            step = self.mu / denom
            self.W += step * np.fft.rfft(np.concatenate([taps, np.zeros((self.parts, b))], axis=1), axis=1)
            # Constrain each partition back to its B taps. Dividing per frequency makes the update leak past them, and
            # without this step the weights grow until the filter diverges.
            taps_now = np.fft.irfft(self.W, 2 * b, axis=1)
            taps_now[:, b:] = 0.0
            self.W = np.fft.rfft(taps_now, axis=1)
        return np.clip(err, -1.0, 1.0)

    @property
    def erle_db(self) -> float:
        """How much the echo is reduced, in dB, over the recent blocks (0 before the filter has learned anything)."""
        if self.s_err <= 0 or self.s_mic <= 0:
            return 0.0
        return 10.0 * math.log10(self.s_mic / self.s_err)


class NoiseSuppressor:
    """Lowers steady background sound. Frames of 2 x `hop` samples overlap by half, with square-root Hann windows, so the
    frames add back to the input. Each frame's spectrum has a tracked noise floor taken away (spectral subtraction)."""

    def __init__(self, hop: int = BLOCK, floor: float = 0.2, oversubtract: float = 1.5, smoothing: float = 0.6,
                 loud: float = 2.0, rate: float = 0.05):
        self.hop = hop
        self.floor = floor  # the least gain a bin is cut to, so speech is never silenced outright
        self.oversubtract = oversubtract
        self.smoothing = smoothing
        self.loud = loud  # a frame this many times louder than the floor is speech, not background
        self.rate = rate  # how fast the floor follows the background frames (0.05: about 0.3 s)
        self.win = np.sqrt(np.hanning(2 * hop + 1)[:-1])
        self.prev = np.zeros(hop)
        self.tail = np.zeros(hop)
        self.noise: np.ndarray | None = None
        self.gain: np.ndarray | None = None

    def process(self, x: np.ndarray) -> np.ndarray:
        frame = np.concatenate([self.prev, x])
        self.prev = x
        spectrum = np.fft.rfft(frame * self.win)
        power = np.abs(spectrum) ** 2
        if self.noise is None or self.gain is None:
            self.noise = np.maximum(power, 1e-12)
            self.gain = np.ones_like(power)
        # The floor is the average spectrum of the background frames: frames whose total power stays under `loud` times the
        # floor's. A louder frame is speech and leaves the floor alone, so speech does not raise what it is cleaned against.
        # A floor that started on speech falls once the background returns.
        if float(np.sum(power)) < self.loud * float(np.sum(self.noise)):
            self.noise = (1.0 - self.rate) * self.noise + self.rate * power
        target = np.maximum(1.0 - self.oversubtract * self.noise / (power + 1e-12), self.floor)
        self.gain = self.smoothing * self.gain + (1.0 - self.smoothing) * target
        y = np.fft.irfft(spectrum * self.gain, 2 * self.hop) * self.win
        out = y[: self.hop] + self.tail
        self.tail = y[self.hop:]
        return out


class Cleaner:
    """The microphone's cleaning, for a stream: `push(mic, ref)` takes microphone samples and the reference played over the
    same span (None for no echo cancellation), and returns the cleaned samples that are ready. Blocks wait until whole, and
    the noise suppressor's overlap adds one block of delay (16 ms), so the output lags the input by that much; its length
    can differ from the input's on any one call."""

    def __init__(self, echo: bool = True, noise: bool = True):
        self.echo = EchoCanceller() if echo else None
        self.noise = NoiseSuppressor() if noise else None
        self._mic = np.zeros(0)
        self._ref = np.zeros(0)

    def push(self, mic: np.ndarray, ref: np.ndarray | None = None) -> np.ndarray:
        mic = np.asarray(mic, dtype=np.float64)
        ref = np.zeros_like(mic) if ref is None else np.asarray(ref, dtype=np.float64)
        if ref.shape != mic.shape:
            raise ValueError("the reference must cover the same samples as the microphone")
        self._mic = np.concatenate([self._mic, mic])
        self._ref = np.concatenate([self._ref, ref])
        out: list[np.ndarray] = []
        while len(self._mic) >= BLOCK:
            m, self._mic = self._mic[:BLOCK], self._mic[BLOCK:]
            r, self._ref = self._ref[:BLOCK], self._ref[BLOCK:]
            x = self.echo.process(m, r) if self.echo is not None else m
            if self.noise is not None:
                x = self.noise.process(x)
            out.append(x)
        return np.concatenate(out) if out else np.zeros(0)

    @property
    def erle_db(self) -> float:
        return self.echo.erle_db if self.echo is not None else 0.0


class Reference:
    """What the speaker is playing, at 16 kHz, so the echo canceller can line it up with the microphone. `window(end, n)` gives
    the `n` samples of the clip that end at `end` (a position in samples from the start of the clip), zeros where the clip
    does not cover them."""

    def __init__(self):
        self._clip = np.zeros(0)

    def load(self, clip: np.ndarray) -> None:
        self._clip = np.asarray(clip, dtype=np.float64)

    def clear(self) -> None:
        self._clip = np.zeros(0)

    def window(self, end: int, n: int) -> np.ndarray:
        out = np.zeros(n)
        start = end - n
        lo, hi = max(start, 0), min(end, len(self._clip))
        if hi > lo:
            out[lo - start : hi - start] = self._clip[lo:hi]
        return out


LAG_MAX = int(0.6 * RATE)  # the speaker-to-microphone delay searched for: up to 0.6 s
LAG_WINDOW = RATE  # the delay is measured on the microphone's last second
LAG_EVERY = RATE // 2  # and measured again every half second
LAG_MIN_MATCH = 0.2  # the best match must correlate at least this well; otherwise the last delay stands
LAG_KEEP = 0.2  # a measured delay is kept while it still matches at least this fraction of the best match
LAG_EARLY = 32  # the reference runs this many samples (2 ms) early: the filter learns leading zeros, and loses no arrivals


class Alignment:
    """The clip the speaker is playing, lined up with what the microphone hears.

    The delay from the speaker to the microphone is not known: the player's buffers, the audio driver and the room all add
    to it. So it is measured. Every half second the microphone's last second is correlated with the clip at every delay
    from 0 to 0.6 s, and the best delay is kept when it matches well. Until a delay is known, and when nothing plays, the
    reference is silence, so the echo canceller passes the microphone through unchanged."""

    def __init__(self) -> None:
        self._clip = Reference()
        self._active = False
        self._hist = np.zeros(0)  # the microphone's recent samples, for the measurement
        self._seen = 0  # microphone samples since the clip started
        self._since = 0  # samples since the last measurement
        self._peak: int | None = None  # the delay of the best match, in samples
        self.lag: int | None = None  # the delay the reference is taken at, in samples, once measured

    @property
    def playing(self) -> bool:
        return self._active

    def start(self, clip) -> None:
        """A clip starts now: the microphone's samples are counted from here."""
        self._clip.load(np.asarray(clip, dtype=np.float64))
        self._active = True
        self._hist = np.zeros(0)
        self._seen = 0
        self._since = 0
        self._peak = None
        self.lag = None

    def stop(self) -> None:
        self._clip.clear()
        self._active = False
        self._peak = None
        self.lag = None

    def reference_for(self, mic) -> np.ndarray:
        """The clip's samples that the microphone is hearing for these microphone samples (zeros when not known)."""
        mic = np.asarray(mic, dtype=np.float64)
        n = len(mic)
        if not self._active:
            return np.zeros(n)
        self._hist = np.concatenate([self._hist, mic])[-LAG_WINDOW:]
        self._seen += n
        self._since += n
        if self._since >= LAG_EVERY and self._seen >= LAG_WINDOW:
            self._since = 0
            self._measure()
        if self.lag is None:
            return np.zeros(n)
        return self._clip.window(self._seen - self.lag, n)

    def _measure(self) -> None:
        w = len(self._hist)
        span = w + LAG_MAX
        seg = self._clip.window(self._seen, span)  # seg[i] is the clip at position (seen - span + i)
        size = 1 << (span - 1).bit_length()  # a power of two that holds the whole span: the correlation does not wrap
        # cross[k] = sum over j of hist[j] * seg[k + j], where k = LAG_MAX - lag
        cross = np.fft.irfft(np.fft.rfft(seg, size) * np.conj(np.fft.rfft(self._hist, size)), size)[: LAG_MAX + 1]
        seg_energy = np.concatenate([[0.0], np.cumsum(seg * seg)])
        k = np.arange(LAG_MAX + 1)
        energy = seg_energy[k + w] - seg_energy[k]
        score = cross / np.sqrt(energy * float(self._hist @ self._hist) + 1e-12)
        best = int(np.argmax(score))
        if score[best] < LAG_MIN_MATCH:
            return
        # The earliest delay that matches at least half as well as the best one: in a room with a random tail the best match
        # can sit a few milliseconds after the first arrival, and a reference that is late loses those first arrivals.
        early = int(np.nonzero(score >= 0.5 * score[best])[0].max())
        # Once a delay is found it stays while it still matches reasonably: a change of even one sample makes the filter
        # relearn its path. A new one replaces it only when the old one has become a poor match.
        if self._peak is None or score[LAG_MAX - self._peak] < LAG_KEEP * score[best]:
            self._peak = LAG_MAX - early
        # The reference is taken a little early (LAG_EARLY). A filter can learn leading zeros for free, but a reference
        # that is late loses the first arrivals of the room, which is what limits the echo reduction in a small room.
        self.lag = max(0, self._peak - LAG_EARLY)


def resample(samples: np.ndarray, rate: int, to_rate: int) -> np.ndarray:
    """Straight-line resampling, which is enough for speech going to the echo canceller and the speaker."""
    if rate == to_rate or len(samples) == 0:
        return np.asarray(samples, dtype=np.float64)
    count = max(1, round(len(samples) * to_rate / rate))
    positions = np.arange(count) * (rate / to_rate)  # each output sample at its true time in the input
    return np.interp(positions, np.arange(len(samples)), samples)
