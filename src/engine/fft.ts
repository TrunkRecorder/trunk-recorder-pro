// In-place radix-2 complex FFT over split f64 arrays. Plans (twiddles +
// bit-reversal) are cached per size. `inverse` flips the twiddle sign and does
// NOT scale by 1/n — callers fold the scale into their own constants.

interface Plan {
  cos: Float64Array;
  sin: Float64Array;
  rev: Uint32Array;
}

const plans = new Map<number, Plan>();

function plan(n: number): Plan {
  let p = plans.get(n);
  if (p) return p;
  if (n < 2 || (n & (n - 1)) !== 0) throw new Error(`FFT size ${n} is not a power of two`);
  const cos = new Float64Array(n / 2);
  const sin = new Float64Array(n / 2);
  for (let i = 0; i < n / 2; i++) {
    cos[i] = Math.cos((-2 * Math.PI * i) / n);
    sin[i] = Math.sin((-2 * Math.PI * i) / n);
  }
  const bits = Math.log2(n);
  const rev = new Uint32Array(n);
  for (let i = 0; i < n; i++) {
    let r = 0;
    for (let b = 0, x = i; b < bits; b++, x >>= 1) r = (r << 1) | (x & 1);
    rev[i] = r;
  }
  p = { cos, sin, rev };
  plans.set(n, p);
  return p;
}

export function fft(re: Float64Array, im: Float64Array, n: number, inverse = false): void {
  const { cos, sin, rev } = plan(n);
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
  const sg = inverse ? -1 : 1;
  for (let size = 2; size <= n; size <<= 1) {
    const half = size >> 1;
    const step = n / size;
    for (let i = 0; i < n; i += size) {
      for (let j = i, k = 0; j < i + half; j++, k += step) {
        const c = cos[k];
        const s = sg * sin[k];
        const a = j + half;
        const tr = re[a] * c - im[a] * s;
        const ti = re[a] * s + im[a] * c;
        re[a] = re[j] - tr;
        im[a] = im[j] - ti;
        re[j] += tr;
        im[j] += ti;
      }
    }
  }
}
