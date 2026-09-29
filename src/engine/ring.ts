// Single-producer / single-consumer ring of f32 over a SharedArrayBuffer — the
// in-browser analogue of CyberEther's `CircularBuffer`: fixed capacity, zero
// allocation after construction, an explicit overflow policy (drop the NEW
// data and count it, so the reader's clock stays honest about the gap), and
// counters for health and throughput.
//
// Layout: Int32 header [writeIdx, readIdx, dropped, doorbell] + f32 data.
// Indices run modulo 2·capacity (the "mirror bit" scheme): position is
// `idx % capacity`, fill is `(w − r) mod 2·capacity`, so full and empty are never
// ambiguous and the indices never overflow however long the ring runs. Works on a plain ArrayBuffer too (tests,
// node, the single-thread fallback).

const HDR = 4; // Int32 slots
const W = 0;
const R = 1;
const DROPPED = 2;
const BELL = 3;

export interface RingStats {
  capacity: number;
  size: number;
  dropped: number;
}

export class SampleRing {
  readonly buffer: SharedArrayBuffer | ArrayBuffer;
  readonly capacity: number;
  private readonly hdr: Int32Array;
  private readonly data: Float32Array;

  /** Create a ring, or attach to one another thread created. */
  constructor(capacityOrBuffer: number | SharedArrayBuffer | ArrayBuffer) {
    if (typeof capacityOrBuffer === "number") {
      const bytes = HDR * 4 + capacityOrBuffer * 4;
      this.buffer = typeof SharedArrayBuffer !== "undefined" ? new SharedArrayBuffer(bytes) : new ArrayBuffer(bytes);
    } else {
      this.buffer = capacityOrBuffer;
    }
    this.hdr = new Int32Array(this.buffer, 0, HDR);
    this.data = new Float32Array(this.buffer, HDR * 4);
    this.capacity = this.data.length;
  }

  get size(): number {
    return this.fill(Atomics.load(this.hdr, W), Atomics.load(this.hdr, R));
  }

  private fill(w: number, r: number): number {
    const m = 2 * this.capacity;
    return (((w - r) % m) + m) % m;
  }

  /** Producer. Writes all of `src` or, if it doesn't fit, none of it. */
  write(src: Float32Array): boolean {
    const w = Atomics.load(this.hdr, W);
    const r = Atomics.load(this.hdr, R);
    if (this.fill(w, r) + src.length > this.capacity) {
      Atomics.add(this.hdr, DROPPED, src.length);
      return false;
    }
    const at = w % this.capacity;
    const first = Math.min(src.length, this.capacity - at);
    this.data.set(src.subarray(0, first), at);
    if (first < src.length) this.data.set(src.subarray(first), 0);
    Atomics.store(this.hdr, W, (w + src.length) % (2 * this.capacity));
    Atomics.add(this.hdr, BELL, 1);
    Atomics.notify(this.hdr, BELL);
    return true;
  }

  /** Consumer. Copies up to `dst.length` elements into `dst`; returns the count. */
  read(dst: Float32Array): number {
    const w = Atomics.load(this.hdr, W);
    const r = Atomics.load(this.hdr, R);
    const n = Math.min(dst.length, this.fill(w, r));
    if (n <= 0) return 0;
    const at = r % this.capacity;
    const first = Math.min(n, this.capacity - at);
    dst.set(this.data.subarray(at, at + first), 0);
    if (first < n) dst.set(this.data.subarray(0, n - first), first);
    Atomics.store(this.hdr, R, (r + n) % (2 * this.capacity));
    return n;
  }

  /** Current doorbell value, for `Atomics.waitAsync(ring.bell, 3, value)`. */
  get bell(): Int32Array {
    return this.hdr;
  }

  stats(): RingStats {
    return {
      capacity: this.capacity,
      size: this.size,
      dropped: Atomics.load(this.hdr, DROPPED),
    };
  }
}
