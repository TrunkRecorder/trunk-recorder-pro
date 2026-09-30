#!/usr/bin/env python3
"""Compare two TSBK JSONL streams (p25tool cc vs ts_cc.ts): which CRC-valid
blocks each decoded, matched by content within a time tolerance.

  compare_tsbk.py cpp.jsonl ts.jsonl [--tol 0.5]
"""
import json
import sys
from collections import Counter

OPS = {0x00: "GRP_V_CH_GRANT", 0x02: "GRP_V_CH_GRANT_UPDT", 0x03: "GRANT_UPDT_EXP", 0x28: "AFFIL", 0x2c: "REG",
       0x30: "TDMA_SYNC", 0x33: "IDEN_UP_TDMA", 0x34: "IDEN_UP_VU", 0x39: "SCCB", 0x3a: "RFSS_STS", 0x3b: "NET_STS",
       0x3c: "ADJ_STS", 0x3d: "IDEN_UP"}


def load(path):
    return [json.loads(l) for l in open(path) if l.startswith("{")]


def match(a, b, tol):
    """Greedy: each block in a matched to an unused identical block in b within tol seconds."""
    pool = {}
    for i, x in enumerate(b):
        pool.setdefault(x["hex"], []).append(i)
    used = set()
    hits = 0
    for x in a:
        for i in pool.get(x["hex"], []):
            if i not in used and abs(b[i]["t"] - x["t"]) <= tol:
                used.add(i)
                hits += 1
                break
    return hits, used


def main():
    tol = float(sys.argv[sys.argv.index("--tol") + 1]) if "--tol" in sys.argv else 0.5
    a, b = load(sys.argv[1]), load(sys.argv[2])
    both, used = match(a, b, tol)
    print(f"C++ {len(a)} TSBKs, TS {len(b)} TSBKs, both {both}, C++ only {len(a) - both}, TS only {len(b) - both}")
    ca = Counter(OPS.get(x["op"], f"0x{x['op']:02x}") for x in a)
    cb = Counter(OPS.get(x["op"], f"0x{x['op']:02x}") for x in b)
    for k in sorted(set(ca) | set(cb), key=lambda k: -(ca[k] + cb[k])):
        print(f"  {k:22s} C++ {ca[k]:4d}   TS {cb[k]:4d}")


if __name__ == "__main__":
    main()
