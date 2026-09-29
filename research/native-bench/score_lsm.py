#!/usr/bin/env python3
"""Score p25tool output against gen_lsm.ts ground truth.

  score_lsm.py truth.json cc.jsonl [voice_<hz>.jsonl ...]

CC: each transmitted TSBK counts as received if an identical block was decoded
within 0.2 s; decoded blocks that were never sent are "false".
Voice: each decoded LDU is aligned to the transmitted LDU it shares the most
identical codewords with (≥ 3, else "unaligned"). Per codeword:
  wrong       u0..u6 differ from what was sent (u7 is unprotected; not counted)
  audible     wrong AND not flagged (E0 < 3 and ET < 10): the vocoder plays it
  needless    right but flagged: the vocoder repeats a good frame
"""
import json
import sys


def main():
    truth = json.load(open(sys.argv[1]))
    cc = [json.loads(l) for l in open(sys.argv[2]) if l.startswith("{")]
    sent = sorted(truth["ccTruth"], key=lambda x: x["t"])
    by_hex = {}
    for i, s in enumerate(sent):
        by_hex.setdefault(s["hex"], []).append(i)
    used, false = set(), 0
    for d in cc:
        hit = next((i for i in by_hex.get(d["hex"], []) if i not in used and abs(sent[i]["t"] - d["t"]) <= 0.2), None)
        if hit is None:
            false += 1 if d["hex"] not in by_hex else 0
        else:
            used.add(hit)
    # Only TSBKs sent after the receiver could have locked (skip the first 0.3 s).
    total = sum(1 for s in sent if 0.3 <= s["t"] <= truth["dur"] - 0.2)
    got = sum(1 for i in used if 0.3 <= sent[i]["t"] <= truth["dur"] - 0.2)
    print(f"CC     TSBKs {got}/{total} ({100 * got / total:.1f}%)  false {false}")

    for path in sys.argv[3:]:
        frames = [json.loads(l) for l in open(path) if l.startswith("{")]
        hz = int(path.rsplit("_", 1)[1].split(".")[0])
        call = next(c for c in truth["calls"] if c["hz"] == hz)
        tu = [tuple(u[:7]) for u in call["u"]]
        n_ldu = len(tu) // 9
        seen, unaligned = set(), 0
        wrong = audible = needless = cw = 0
        for f in frames:
            if f["duid"] not in (5, 10):
                continue
            du = [tuple(c["u"][:7]) for c in f["imbe"]]
            best, bi = -1, -1
            for i in range(n_ldu):
                m = sum(1 for k in range(9) if du[k] == tu[9 * i + k])
                if m > best:
                    best, bi = m, i
            if best < 3:
                unaligned += 1
                continue
            seen.add(bi)
            for k, c in enumerate(f["imbe"]):
                cw += 1
                ok = du[k] == tu[9 * bi + k]
                flagged = c["e0"] >= 3 or c["errs"] >= 10
                wrong += not ok
                audible += (not ok) and not flagged
                needless += ok and flagged
        lost = n_ldu - len(seen)
        print(f"{hz/1e6:.4f} LDUs {len(seen)}/{n_ldu} lost {lost} unaligned {unaligned} | codewords {cw}: wrong {wrong} "
              f"({100 * wrong / max(1, cw):.2f}%) audible {audible} needless-repeats {needless}")


if __name__ == "__main__":
    main()
