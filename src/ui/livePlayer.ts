// Gapless playback of decoded audio as it arrives. The decoders deliver audio
// in bursts (a window at a time), so chunks are scheduled back to back on the
// AudioContext clock behind a small lead, and a gap longer than that restarts
// the schedule instead of letting it drift behind real time.

const LEAD_S = 0.4;

export class LivePlayer {
  private ctx: AudioContext | null = null;
  private next = 0;
  private gain: GainNode | null = null;

  resume(): void {
    if (!this.ctx) {
      this.ctx = new AudioContext();
      this.gain = this.ctx.createGain();
      this.gain.gain.value = 2; // the vocoder's output is quiet
      this.gain.connect(this.ctx.destination);
    }
    void this.ctx.resume();
  }

  stop(): void {
    void this.ctx?.close();
    this.ctx = null;
    this.gain = null;
    this.next = 0;
  }

  enqueue(samples: Float32Array, rate: number): void {
    const ctx = this.ctx;
    if (!ctx || !this.gain || !samples.length) return;
    const buf = ctx.createBuffer(1, samples.length, rate);
    buf.getChannelData(0).set(samples);
    const src = ctx.createBufferSource();
    src.buffer = buf;
    src.connect(this.gain);
    const now = ctx.currentTime;
    if (this.next < now + 0.02 || this.next > now + 3) this.next = now + LEAD_S;
    src.start(this.next);
    this.next += buf.duration;
  }
}
