# Audio comparison of each decode against the same tool's decode of clear (first file given).
# - NCC: waveform normalized cross-correlation at the best lag (+-2000 samples).
# - spec r: Pearson correlation of log-magnitude spectrograms (20 ms frames, 256-pt FFT) at that lag.
#   IMBE synthesis uses random phase for unvoiced bands, so waveform NCC between two runs of the SAME
#   bits can be < 1; the spectrogram correlation is insensitive to that.
import sys, wave, numpy as np
def load(p):
    w = wave.open(p); x = np.frombuffer(w.readframes(w.getnframes()), dtype="<i2").astype(np.float64)
    if w.getnchannels() == 2: x = x.reshape(-1, 2).mean(axis=1)
    return x, w.getframerate()
def spec(x):
    n = len(x)//160; f = x[:n*160].reshape(n, 160)*np.hanning(160)
    return np.log10(np.abs(np.fft.rfft(f, 256, axis=1))**2 + 1e2)
def rms_db(x): return 20*np.log10(np.sqrt(np.mean(x**2))/32768 + 1e-12)
ref, sr = load(sys.argv[1])
print(f"{'file':22s} {'samples':>7s} {'rms dBFS':>8s} {'lag':>5s} {'NCC':>7s} {'spec r':>7s} {'identical':>9s}")
for p in sys.argv[1:]:
    x, _ = load(p)
    if len(x) == 0 or not np.any(x):
        print(f"{p.split('/')[-1]:22s} {len(x):7d}   (silent / no audio)"); continue
    best = (-2, 0)
    for lag in range(-2000, 2001):
        a = ref[max(0, lag):]; b = x[max(0, -lag):]; n = min(len(a), len(b))
        a, b = a[:n], b[:n]; d = np.sqrt(np.dot(a, a)*np.dot(b, b))
        c = np.dot(a, b)/d if d else 0
        if c > best[0]: best = (c, lag)
    lag = best[1]; a = ref[max(0, lag):]; b = x[max(0, -lag):]; n = min(len(a), len(b))
    sr_ = np.corrcoef(spec(a[:n]).ravel(), spec(b[:n]).ravel())[0, 1]
    ident = len(x) == len(ref) and np.array_equal(x, ref)
    print(f"{p.split('/')[-1]:22s} {len(x):7d} {rms_db(x):8.2f} {lag:5d} {best[0]:7.4f} {sr_:7.4f} {str(ident):>9s}")
