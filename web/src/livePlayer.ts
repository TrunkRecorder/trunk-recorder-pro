// Gapless playback of decoded audio as it arrives. The decoders deliver audio
// in bursts (a window at a time), so chunks are scheduled back to back on the
// AudioContext clock behind a small lead, and a gap longer than that restarts
// the schedule instead of letting it drift behind real time.
//
// Safari needs more care than Chrome: it may refuse or poorly resample 8 kHz
// buffers (each 20 ms chunk resampled on its own clicks at the seams), and its
// context can drop out of "running" (tab switches, output device changes) and
// only resume on a user gesture. So audio is resampled here, continuously
// across chunks, to the context's own rate; chunks are dropped rather than
// piled up while the context isn't running; and any click or key press on the
// page resumes it.

const LEAD_S = 0.4;

export class LivePlayer {
  private ctx: AudioContext | null = null;
  private next = 0;
  private gain: GainNode | null = null;
  private resampler: Resampler | null = null;
  private inRate = 0;

  constructor() {
    const wake = () => {
      if (this.ctx && this.ctx.state !== "running") void this.ctx.resume().catch(() => {});
    };
    if (typeof document !== "undefined") {
      document.addEventListener("pointerdown", wake, true);
      document.addEventListener("keydown", wake, true);
      document.addEventListener("visibilitychange", wake);
    }
  }

  resume(): void {
    if (!this.ctx) {
      this.ctx = new AudioContext();
      this.gain = this.ctx.createGain();
      this.gain.gain.value = 2; // the vocoder's output is quiet
      this.gain.connect(this.ctx.destination);
    }
    void this.ctx.resume().catch(() => {});
  }

  stop(): void {
    void this.ctx?.close().catch(() => {});
    this.ctx = null;
    this.gain = null;
    this.resampler = null;
    this.next = 0;
  }

  enqueue(samples: Float32Array, rate: number): void {
    const ctx = this.ctx;
    if (!ctx || !this.gain || !samples.length) return;
    if (ctx.state !== "running") {
      // The clock is frozen; scheduling now would stack chunks on top of each
      // other and play them all at once when it wakes.
      void ctx.resume().catch(() => {});
      return;
    }
    const now = ctx.currentTime;
    const restart = this.next < now + 0.02 || this.next > now + 3;
    if (restart || !this.resampler || this.inRate !== rate) {
      this.resampler = new Resampler(rate, ctx.sampleRate);
      this.inRate = rate;
    }
    const out = this.resampler.process(samples);
    if (!out.length) return;
    const buf = ctx.createBuffer(1, out.length, ctx.sampleRate);
    buf.getChannelData(0).set(out);
    const src = ctx.createBufferSource();
    src.buffer = buf;
    src.connect(this.gain);
    if (restart) this.next = now + LEAD_S;
    src.start(this.next);
    this.next += buf.duration;
  }
}

/** Streaming windowed-sinc resampler; state carries across chunks so the seams are seamless. */
class Resampler {
  private static readonly W = 8; // taps each side
  private readonly step: number; // input samples per output sample
  private readonly cutoff: number; // relative to the input Nyquist
  private hist: Float32Array;
  private pos: number;

  constructor(inRate: number, outRate: number) {
    this.step = inRate / outRate;
    this.cutoff = Math.min(1, outRate / inRate) * 0.95;
    this.hist = new Float32Array(Resampler.W);
    this.pos = Resampler.W;
  }

  process(x: Float32Array): Float32Array {
    const W = Resampler.W;
    const buf = new Float32Array(this.hist.length + x.length);
    buf.set(this.hist);
    buf.set(x, this.hist.length);
    const n = Math.max(0, Math.ceil((buf.length - W - this.pos) / this.step));
    const out = new Float32Array(n);
    let t = this.pos;
    for (let i = 0; i < n; i++, t += this.step) {
      const base = Math.floor(t);
      let acc = 0;
      for (let k = base - W + 1; k <= base + W; k++) {
        if (k < 0 || k >= buf.length) continue;
        acc += buf[k] * this.kernel(t - k);
      }
      out[i] = acc;
    }
    const drop = Math.max(0, Math.floor(t) - W);
    this.hist = buf.slice(drop);
    this.pos = t - drop;
    return out;
  }

  private kernel(d: number): number {
    const W = Resampler.W;
    if (Math.abs(d) >= W) return 0;
    const c = this.cutoff;
    const x = Math.PI * d * c;
    const sinc = d === 0 ? c : (c * Math.sin(x)) / x;
    const w = 0.42 + 0.5 * Math.cos((Math.PI * d) / W) + 0.08 * Math.cos((2 * Math.PI * d) / W); // Blackman
    return sinc * w;
  }
}
