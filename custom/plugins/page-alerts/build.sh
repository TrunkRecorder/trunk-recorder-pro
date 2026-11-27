#!/usr/bin/env bash
# Builds the Page alerts plugin and puts it in ~/trp-plugins/page-alerts/page-alerts.
set -euo pipefail
cd "$(dirname "$0")"
command -v cargo >/dev/null || { echo "Rust isn't installed (cargo not found)."; exit 1; }
echo "Building the Page alerts plugin (a minute or two)..."
cargo build --release
mkdir -p "$HOME/trp-plugins/page-alerts"
install -m 755 target/release/page-alerts "$HOME/trp-plugins/page-alerts/page-alerts"
echo
echo "Installed: $HOME/trp-plugins/page-alerts/page-alerts"
echo "Next: Plugins page -> Add from a file... -> $HOME/trp-plugins/page-alerts/page-alerts"
