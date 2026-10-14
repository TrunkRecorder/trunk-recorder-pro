#!/bin/bash
# Runs stock and AES-patched dsd-fme on (a) the 48 kHz discriminator WAVs and (b) OP25's symbol dumps.
CC=$(cd "$(dirname "$0")/.." && pwd)
SIG=${SIG:-../synth/signals}
R=$CC/scripts/run_dsd.sh
K_ADP="-1 1234567890"; K_DES="-1 0123456789ABCDEF"
K_TDES='0123456789ABCDEF 23456789ABCDEF01 456789ABCDEF0123'
K_A128='0001020304050607 08090A0B0C0D0E0F'
K_A256='0001020304050607 08090A0B0C0D0E0F 1011121314151617 18191A1B1C1D1E1F'
for src in wav bin; do
  for build in dsd-fme dsd-fme-patched; do
    export DSD=$CC/$build/build/dsd-fme
    O=$CC/out/dsd-$src; s=""; [ $build = dsd-fme-patched ] && s="-patched"
    f(){ [ $src = wav ] && echo $SIG/$1.wav || echo $CC/out/sym/$1.bin; }
    $R $(f clear) $O clear$s
    $R $(f adp) $O adp$s $K_ADP
    $R $(f des) $O des$s $K_DES
    $R $(f tdes) $O tdes$s -H "$K_TDES"
    $R $(f aes128) $O aes128$s -H "$K_A128"
    $R $(f aes256) $O aes256$s -H "$K_A256"
    if [ -z "$s" ]; then
      $R $(f des) $O des-nokey
      $R $(f des) $O des-wrongkey -1 1123456789ABCDEF
      $R $(f aes256) $O aes256-nokey
      $R $(f clear) $O clear-run2
    fi
  done
done
