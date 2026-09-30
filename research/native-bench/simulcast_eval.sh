#!/bin/zsh
# Simulcast (LSM) receiver evaluation on DCFD captures: control-channel TSBKs
# and voice framing/FEC for a set of p25tool option sets.
#
#   simulcast_eval.sh <cap.cu8> "<p25tool options>" ["<options>" ...]
#
# Per option set: CC TSDUs found and good/bad TSBKs; per voice channel LDUs
# decoded, LDUs missing inside transmissions (a gap of n·864 symbols between
# LDUs = n−1 lost), and "repeat-worthy" codewords (E0 ≥ 3 or ET ≥ 10, which
# the vocoder repeats or mutes).
cap=$1; shift
here=${0:A:h}
tool=(${=P25TOOL:-$here/cpp/p25tool})  # e.g. P25TOOL="../../target/release/trunk-lite tool"
tmp=$(mktemp -d)
common=(--fs 2400000 --center 858300000)
for opts in "$@"; do
  o=(${=opts})
  $tool cc $cap $common --cc 857987500 $o > /dev/null 2> $tmp/cc.txt
  line="$(printf '%-34s' "$opts") $(python3 - $tmp/cc.txt <<'EOF'
import json, sys
d = json.loads(open(sys.argv[1]).read().splitlines()[-1])
g, b = d["good"], d["bad"]
print(f"CC {d['frames'].get('TSDU', 0):4d} TSDU {g:4d}/{b:3d} ({100 * g / max(1, g + b):.1f}%)")
EOF
)"
  for f in 858587500 859037500 857187500; do
    $tool voice $cap $common --freq $f $o > $tmp/v.jsonl 2> $tmp/v.txt
    line="$line | ${f:0:3}.${f:3:2} $(python3 - $tmp/v.jsonl $tmp/v.txt <<'EOF'
import json, sys
fr = [json.loads(l) for l in open(sys.argv[1]) if l.startswith("{")]
d = json.loads(open(sys.argv[2]).read().splitlines()[-1])
ldu = [f for f in fr if f["duid"] in (5, 10)]
miss = 0
for a, b in zip(fr, fr[1:]):
    if a["duid"] in (5, 10) and b["duid"] in (5, 10):
        n = round((b["t"] - a["t"]) * 4800 / 864)
        if 2 <= n <= 6:
            miss += n - 1
print(f"{len(ldu):3d}L {miss:2d}m {d['repeatWorthy']:3d}r" if ldu else "  -         ")
EOF
)"
  done
  echo $line
done
rm -rf $tmp
