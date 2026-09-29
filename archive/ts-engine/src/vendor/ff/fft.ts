// In-place radix-2 iterative Cooley–Tukey FFT over split real/imaginary arrays.
// Pure TS — no dependency. Fast enough at 2048 points / a few hundred FFTs per
// sweep (the retune, not the FFT, dominates). Swap for a WASM FFT only if
// profiling later shows this is the bottleneck.

export class FFT {
  readonly n: number;
  private readonly cos: Float64Array;
  private readonly sin: Float64Array;
  private readonly rev: Uint32Array;

  constructor(n: number) {
    if ((n & (n - 1)) !== 0) throw new Error("FFT size must be a power of two");
    this.n = n;
    // Twiddle tables (forward transform sign convention).
    this.cos = new Float64Array(n / 2);
    this.sin = new Float64Array(n / 2);
    for (let i = 0; i < n / 2; i++) {
      const a = (-2 * Math.PI * i) / n;
      this.cos[i] = Math.cos(a);
      this.sin[i] = Math.sin(a);
    }
    // Bit-reversal permutation table.
    this.rev = new Uint32Array(n);
    let log2n = 0;
    while (1 << log2n < n) log2n++;
    for (let i = 0; i < n; i++) {
      let x = i;
      let r = 0;
      for (let b = 0; b < log2n; b++) {
        r = (r << 1) | (x & 1);
        x >>= 1;
      }
      this.rev[i] = r;
    }
  }

  /** Forward FFT in place on the given real/imaginary arrays (length n). */
  forward(re: Float64Array, im: Float64Array): void {
    const n = this.n;
    const rev = this.rev;
    // Bit-reversal reorder.
    for (let i = 0; i < n; i++) {
      const j = rev[i];
      if (j > i) {
        let t = re[i];
        re[i] = re[j];
        re[j] = t;
        t = im[i];
        im[i] = im[j];
        im[j] = t;
      }
    }
    // Butterflies.
    const cos = this.cos;
    const sin = this.sin;
    for (let size = 2; size <= n; size <<= 1) {
      const half = size >> 1;
      const step = n / size;
      for (let i = 0; i < n; i += size) {
        for (let j = i, k = 0; j < i + half; j++, k += step) {
          const c = cos[k];
          const s = sin[k];
          const tre = re[j + half] * c - im[j + half] * s;
          const tim = re[j + half] * s + im[j + half] * c;
          re[j + half] = re[j] - tre;
          im[j + half] = im[j] - tim;
          re[j] += tre;
          im[j] += tim;
        }
      }
    }
  }
}
