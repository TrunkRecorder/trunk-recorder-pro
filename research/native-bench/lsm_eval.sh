#!/bin/zsh
# Run p25tool on a gen_lsm.ts capture and score it against the truth.
#   lsm_eval.sh <prefix> "<p25tool options>" ["<options>" ...]
prefix=$1; shift
here=${0:A:h}
tool=(${=P25TOOL:-$here/cpp/p25tool})  # e.g. P25TOOL="../../target/release/trunk-lite tool"
tmp=$(mktemp -d)
freqs=($(python3 -c "import json; print(' '.join(str(c['hz']) for c in json.load(open('$prefix.truth.json'))['calls']))"))
for opts in "$@"; do
  o=(${=opts})
  echo "== $opts"
  $tool cc $prefix.cu8 --fs 2400000 --center 773100000 --cc 773000000 $o > $tmp/cc.jsonl 2> /dev/null
  v=()
  for f in $freqs; do
    $tool voice $prefix.cu8 --fs 2400000 --center 773100000 --freq $f $o > $tmp/voice_$f.jsonl 2> /dev/null
    v+=($tmp/voice_$f.jsonl)
  done
  python3 $here/score_lsm.py $prefix.truth.json $tmp/cc.jsonl $v | sed 's/^/   /'
done
rm -rf $tmp
