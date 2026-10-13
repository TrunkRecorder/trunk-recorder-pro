# P25 Phase 1 encryption cross-check: OP25 and dsd-fme vs our synthetic signals

Run 2026-10-12 (results in research/p25-encryption.md, Log). These are the
scripts and patches; the tools were built in a scratch directory.

Inputs, `../synth/signals/{clear,adp,des,tdes,aes128,aes256}.{cf32,wav}`, come
from `tool p25synth` with a real clear call's frames, at 240 kS/s with the
signal at DC, one keys file per cipher (the keys are listed under Run):

```
trunk-pro tool p25synth signals/aes256.cf32 --center 851100000 --freq 851100000 --rate 240000 \
  --keys keys-aes256.json --voice captures/reference/bugfix-output/conv-p25-fm/729-63_859037500.frames.jsonl \
  --wav signals/aes256.wav
```

(without `--keys` for clear). `pastub/` was a no-op libpulse (headers and an
empty `libpulse.dylib` exporting the symbols dsd-fme links), not kept here.

## Versions
- OP25 boatbod/op25 master `71abcd0ead32f86f51615ea6cc8a6a4dba4c949a` (2026-08-23), against Homebrew GNU Radio 3.10.12.0 / Python 3.14
- dsd-fme lwvmobile/dsd-fme default branch `audio_work` `4fe32db7f5affb0484aa0e4f8bbc50eabe3c8531` ("AW 2026-52-g4fe32db")
- mbelib lwvmobile/mbelib `34adf9f054bc5650ace162a4917dcbc2cfa6102e` (1.3.2), built into `prefix/`

## Build
OP25 (`op25-build.patch` = the two build fixes needed on macOS/CMake 4/libc++):
- removed `cmake_policy(SET CMP0026 OLD)` / `CMP0045 OLD` from top CMakeLists.txt
- `op25_audio.h`: added `op25_audio(const op25_audio&) = delete;` (std::map<int,op25_audio> fails to instantiate in libc++ otherwise)
```
cd op25 && mkdir build && cd build
cmake .. -DCMAKE_POLICY_VERSION_MINIMUM=3.5 -DCMAKE_INSTALL_PREFIX=$PWD/../../prefix -DCMAKE_PREFIX_PATH=/opt/homebrew
make -j10 && make install
```
`op25-patched/` (+ `prefix-op25-patched/`) additionally has the 1-line u7 fix in `op25_imbe_frame.h` (see findings), `op25-crosscheck.patch`.

mbelib: `cmake .. -DCMAKE_POLICY_VERSION_MINIMUM=3.5 -DCMAKE_INSTALL_PREFIX=../../prefix && make install`

dsd-fme hard-requires PulseAudio. `pastub/` is a no-op libpulse stand-in (headers + `libpulse.dylib`) so it links on macOS:
```
cd dsd-fme && mkdir build && cd build
CC=<this dir>
cmake .. -DCMAKE_POLICY_VERSION_MINIMUM=3.5 -DCMAKE_PREFIX_PATH="$CC/prefix;/opt/homebrew;/opt/homebrew/opt/ncurses" \
  -DPULSEAUDIO_INCLUDE_DIR=$CC/pastub/include -DPULSEAUDIO_LIBRARY=$CC/pastub/libpulse.dylib \
  -DPULSEAUDIO_MAINLOOP_LIBRARY=$CC/pastub/libpulse.dylib -DPULSEAUDIO_SIMPLE_LIBRARY=$CC/pastub/libpulse.dylib \
  -DMBE_INCLUDE_DIR=$CC/prefix/include -DMBE_LIBRARY=$CC/prefix/lib/libmbe.dylib
make -j10
```
`dsd-fme-patched/` = same + `dsd-fme-crosscheck.patch` (AES keystream 14->15 blocks; unmute audio for ALGID 0x83).

## Run
- OP25: `scripts/run_op25.sh <signal> <op25 keys json|none> <tag> [verbosity]` -> `out/op25/<tag>.{wav,log}`
  (rx.py -F file.cf32 -S 240000 -D fsk4 -V -w -W 127.0.0.1 -u 23456 -l 56111 -v 9 --crypt-behavior 0 -k keys.json;
  audio is caught off UDP 23456 by `scripts/udpcap.py` as 8 kHz WAV; `-v 9` logs per-frame IMBE hex
  "CLEARTEXT"/"CIPHERTXT"/"PLAINTEXT"; `-v 10` also logs ESS algid/keyid/MI.)
  Env: `RAW=out/sym/x.bin` adds `-r` (dump demodulated dibits); `OP25DIR=op25-patched OP25PREFIX=prefix-op25-patched` uses the u7-fixed build.
  Keys: `keys/op25-*.json`, format `{ "0x370e": {"algid": "0x84", "key": ["0x00", ...]} }`.
  OP25 only implements 0x81 DES-OFB, 0x84 AES-256, 0xAA ADP.
- dsd-fme: `scripts/run_dsd_all.sh` (both builds, wav and OP25-symbol .bin inputs) -> `out/dsd-wav/`, `out/dsd-bin/`.
  Per run: `dsd-fme -f1 -i <in.wav|in.bin> -o udp:127.0.0.1:23999 -w out.wav -Z <key opts>`
  (`-o null` makes `-w` write silence, so a dead UDP target is used.)
  Keys: ADP `-1 1234567890`; DES `-1 0123456789ABCDEF`;
  3DES `-H '0123456789ABCDEF 23456789ABCDEF01 456789ABCDEF0123'`; AES-128 `-H '0001020304050607 08090A0B0C0D0E0F'`;
  AES-256 `-H '0001020304050607 08090A0B0C0D0E0F 1011121314151617 18191A1B1C1D1E1F'`. (Manual keys ignore key ID.)
- Compare:
  - `python3 scripts/op25_bits.py out/op25/clear.log out/op25/<x>.log` (OP25 decrypted IMBE vs OP25 clear)
  - `python3 scripts/vs_truth.py out/op25/clear.log out/dsd-*/<x>.log` (dsd-fme -Z IMBE vs OP25's error-free clear decode; same u0..u7 packing)
  - `python3 scripts/audio_cmp.py <clear.wav> <others.wav...>` (lag-aligned waveform NCC + log-spectrogram correlation)
  - `python3 scripts/mi_check.py out/op25/aes256-v10.log` (logged LDU2 MIs follow the 64-step LFSR)
