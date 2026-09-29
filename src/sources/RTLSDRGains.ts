// RTL-SDR tuner gains. The dongle only supports a fixed set of discrete gain
// steps; the real set is read from the device once open, but this standard
// R820T2 table is used for the UI before a device is connected.
// Ported from Sources/TVPower/Model/RTLSDRGains.swift.

export const RTLSDRGains = {
  /** Standard R820T2 gains in dB (the common librtlsdr table). */
  standard: [
    0.0, 0.9, 1.4, 2.7, 3.7, 7.7, 8.7, 12.5, 14.4, 15.7, 16.6, 19.7, 20.7, 22.9, 25.4, 28.0, 29.7, 32.8, 33.8, 36.4,
    37.2, 38.6, 40.2, 42.1, 43.4, 43.9, 44.5, 48.0, 49.6,
  ] as number[],

  /** Nearest supported gain (dB) to a requested value, within `steps`. */
  nearest(db: number, steps: number[]): number {
    const table = steps.length === 0 ? RTLSDRGains.standard : steps;
    let best = table[0];
    for (const g of table) if (Math.abs(g - db) < Math.abs(best - db)) best = g;
    return best;
  },
};
