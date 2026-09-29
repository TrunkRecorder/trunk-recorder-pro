#!/usr/bin/env python3
"""Compare recordings of the same call from different decoders.

Vocoders draw random phases, so waveforms differ even for identical frames;
what should agree is the speech envelope. For each file: voiced seconds (20 ms
frames within 30 dB of the loudest), and against the first (reference) file the
best-lag correlation of the 20 ms log-energy envelopes (±5 s search).

  compare_audio.py label=ref.wav label=other.wav ...
"""
import sys
import wave

import numpy as np


def load(path):
    with wave.open(path) as w:
        rate, n, width = w.getframerate(), w.getnframes(), w.getsampwidth()
        x = np.frombuffer(w.readframes(n), dtype={2: np.int16, 4: np.int32}[width]).astype(np.float64)
        if w.getnchannels() > 1:
            x = x.reshape(-1, w.getnchannels()).mean(axis=1)
    if rate != 8000:  # TR may write 16 kHz: decimate by block mean
        k = rate // 8000
        x = x[: len(x) // k * k].reshape(-1, k).mean(axis=1)
    return x / 32768.0


def envelope(x):
    f = 160
    e = (x[: len(x) // f * f].reshape(-1, f) ** 2).mean(axis=1)
    return 10 * np.log10(e + 1e-10)


def best_corr(a, b, max_lag=250):
    best = (-1.0, 0)
    for lag in range(-max_lag, max_lag + 1):
        if lag >= 0:
            x, y = a[lag:], b
        else:
            x, y = a, b[-lag:]
        n = min(len(x), len(y))
        if n < 50:
            continue
        x, y = x[:n], y[:n]
        if x.std() == 0 or y.std() == 0:
            continue
        c = float(np.corrcoef(x, y)[0, 1])
        if c > best[0]:
            best = (c, lag)
    return best


def main():
    items = [a.split("=", 1) for a in sys.argv[1:]]
    envs = {}
    for label, path in items:
        x = load(path)
        e = envelope(x)
        voiced = float((e > e.max() - 30).sum() * 0.02)
        envs[label] = e
        print(f"{label:14s} {len(x) / 8000:6.1f} s  voiced {voiced:5.1f} s  rms {np.sqrt((x ** 2).mean()):.4f}")
    ref = items[0][0]
    for label, _ in items[1:]:
        c, lag = best_corr(envs[ref], envs[label])
        print(f"  envelope corr {ref} vs {label}: {c:.3f} (lag {lag * 0.02:+.2f} s)")


if __name__ == "__main__":
    main()
