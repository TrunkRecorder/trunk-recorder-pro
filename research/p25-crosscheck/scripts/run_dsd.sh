#!/bin/bash
# usage: [DSD=path/to/dsd-fme] run_dsd.sh <input .wav|.bin> <outdir> <outtag> [key options...]
# -o udp:... (nothing listening) is used because with -o null dsd-fme writes a silent -w wav.
CC=$(cd "$(dirname "$0")/.." && pwd)
in=$1; out=$2; tag=$3; shift 3
mkdir -p $out; rm -f $out/$tag.wav
perl -e 'alarm shift; exec @ARGV' 60 ${DSD:-$CC/dsd-fme/build/dsd-fme} -f1 -i $in -o udp:127.0.0.1:23999 -w $out/$tag.wav -Z "$@" > /dev/null 2> $out/$tag.log
echo "$tag rc=$? imbe=$(grep -c ' IMBE ' $out/$tag.log)"
