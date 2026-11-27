#!/usr/bin/env bash
# Builds Trunk Recorder Pro 0.5.0 (the latest release) plus what isn't upstream yet: the EDACS
# data-burst fix, SoapySDR (RSPduo) Find keeping the tuner mode and live AGC / gain stages, the
# multi-site fix for sites naming a talker differently, the silent tail cut from trunked analog
# calls (EDACS hang time), multi-site copies on the air together kept once, the build tag on the page, and Setup marks for what applies while recording, page tones whose edges read slightly off no longer dropped, a plugin page's settings above its lists (build kit v39).
# Run it as your normal user on Ubuntu (x86_64). 10-20 minutes the first time.
#   bash build.sh
# Result: ./trunk-pro-0.5.0-custom (next to this script).
set -euo pipefail
KIT="$(cd "$(dirname "$0")" && pwd)"
WORK="$KIT/work"
NODE_VER=22.20.0
UPSTREAM_COMMIT=a5c43761134c95abcc1ab892eaf8fce34baf90e9   # upstream v0.5.0
mkdir -p "$WORK"

echo "== System packages (needs sudo once)"
sudo apt-get update -qq
sudo apt-get install -y -qq build-essential git curl pkg-config ca-certificates xz-utils

echo "== Rust (rustup, in your home folder) + the WebAssembly target"
if ! command -v rustup >/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
source "$HOME/.cargo/env"
rustup toolchain install stable --profile minimal
rustup default stable
rustup target add wasm32-unknown-unknown
if ! wasm-bindgen --version 2>/dev/null | grep -q 0.2.129; then
  cargo install wasm-bindgen-cli --version 0.2.129 --locked
fi

echo "== Node $NODE_VER (private copy, only used by this build)"
if [ ! -x "$WORK/node/bin/node" ]; then
  curl -sSfL "https://nodejs.org/dist/v$NODE_VER/node-v$NODE_VER-linux-x64.tar.xz" | tar xJ -C "$WORK"
  mv "$WORK/node-v$NODE_VER-linux-x64" "$WORK/node"
fi
export PATH="$WORK/node/bin:$PATH"

echo "== Source: upstream v0.5.0 (${UPSTREAM_COMMIT:0:7}) + our changes"
rm -rf "$WORK/src"
git clone -q https://github.com/TrunkRecorder/trunk-recorder-pro "$WORK/src"
cd "$WORK/src"
git checkout -q "$UPSTREAM_COMMIT"
git -c user.name=build -c user.email=build@local am -q "$KIT"/patches/*.patch

echo "== Web interface (WebAssembly + dashboard)"
cd web
npm ci --no-audit --no-fund
npm run schema -- --check
npm run wasm
npm run build
cd ..

echo "== The app"
export TRUNK_PRO_BUILD=custom-v39
cargo build --locked -p trunk-pro --profile dist
cp target/dist/trunk-pro "$KIT/trunk-pro-0.5.0-custom"
echo
echo "Done: $KIT/trunk-pro-0.5.0-custom"
"$KIT/trunk-pro-0.5.0-custom" --version || true
