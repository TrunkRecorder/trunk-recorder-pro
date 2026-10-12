#!/bin/bash
# usage: run_op25.sh <signal-name> <keysfile|none> <outtag> [verbosity]
CC=$(cd "$(dirname "$0")/.." && pwd)
SIG=${SIG:-../synth/signals}
GRPY=/opt/homebrew/Cellar/gnuradio/3.10.12.0_8/libexec/venv/bin/python
name=$1; keys=$2; tag=$3; v=${4:-9}
OUT=$CC/out/op25; mkdir -p $OUT
KOPT=(); [ "$keys" != none ] && KOPT=(-k "$keys")
/opt/homebrew/bin/python3 $CC/scripts/udpcap.py 23456 $OUT/$tag.wav 6 > $OUT/$tag.cap.txt &
CAP=$!
sleep 0.5
cd $CC/${OP25DIR:-op25}/op25/gr-op25_repeater/apps
PYTHONPATH=$CC/${OP25PREFIX:-prefix}/lib/python3.14/site-packages DYLD_LIBRARY_PATH=$CC/${OP25PREFIX:-prefix}/lib \
  perl -e 'alarm shift; exec @ARGV' 16 $GRPY rx.py -F $SIG/$name.cf32 -S 240000 -D fsk4 -V -w -W 127.0.0.1 -u 23456 \
  -l 56111 -v $v --crypt-behavior 0 "${KOPT[@]}" ${RAW:+-r "$RAW"} > $OUT/$tag.stdout.txt 2> $OUT/$tag.log
wait $CAP; cat $OUT/$tag.cap.txt
