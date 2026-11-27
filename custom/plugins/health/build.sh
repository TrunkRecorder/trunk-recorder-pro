#!/usr/bin/env bash
# Builds the Health check plugin and puts it in ~/trp-plugins/health/health.
set -euo pipefail
cd "$(dirname "$0")"
command -v cargo >/dev/null || { echo "Rust isn't installed (cargo not found)."; exit 1; }
echo "Building the Health check plugin (a minute or two)..."
cargo build --release
mkdir -p "$HOME/trp-plugins/health"
install -m 755 target/release/health "$HOME/trp-plugins/health/health"
echo
"$HOME/trp-plugins/health/health" --version | sed 's/^/Built: health /'
echo "Installed: $HOME/trp-plugins/health/health"
echo "Next: Plugins page -> Add from a file... -> $HOME/trp-plugins/health/health"
