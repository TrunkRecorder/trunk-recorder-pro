"""blip25-mbe on a .frames.jsonl of IMBE frames → raw 16-bit 8 kHz.

    python blip25_decode.py <frames.jsonl> <out.s16> [--enhance]

The 88 information bits (u0..u7, 22 hex digits) are blip25's no-FEC IMBE
frame (Rate.IMBE_4400X4400). --enhance turns on its optional post-filter.
"""

import json
import sys

import blip25_mbe as b
import numpy as np


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if len(args) != 2:
        sys.exit(__doc__)
    v = b.Vocoder(b.Rate.IMBE_4400X4400)
    v.set_enhancement(b.EnhancementMode.CLASSICAL if "--enhance" in sys.argv else b.EnhancementMode.NONE)
    out = []
    with open(args[0]) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            r = json.loads(line)
            if r.get("codec") != "imbe":
                continue
            out.append(np.asarray(v.decode_bits(bytes.fromhex(r["bits"])), dtype=np.int16))
    (np.concatenate(out) if out else np.zeros(0, np.int16)).tofile(args[1])


if __name__ == "__main__":
    main()
