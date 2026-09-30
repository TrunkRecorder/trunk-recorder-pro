#!/bin/sh
# Install Trunk Recorder Lite from this folder:
#   sudo ./install.sh            binary → /usr/local/bin, udev rule for RTL-SDR
#                                access, desktop menu entry
#   sudo ./install.sh --uninstall
# Headless (a Raspberry Pi, a server): see trunk-lite.service.
set -eu
cd "$(dirname "$0")"
PREFIX=${PREFIX:-/usr/local}
if [ "$(id -u)" -ne 0 ]; then
  echo "Run as root: sudo $0 $*" >&2
  exit 1
fi
if [ "${1:-}" = "--uninstall" ]; then
  rm -f "$PREFIX/bin/trunk-lite" /etc/udev/rules.d/60-trunk-lite-rtlsdr.rules \
    "$PREFIX/share/applications/trunk-lite.desktop" "$PREFIX/share/icons/hicolor/256x256/apps/trunk-lite.png"
  udevadm control --reload-rules 2>/dev/null || true
  echo "Removed. Your settings (~/.config/trunk-lite) and recordings are kept."
  exit 0
fi
install -Dm755 trunk-lite "$PREFIX/bin/trunk-lite"
install -Dm644 60-trunk-lite-rtlsdr.rules /etc/udev/rules.d/60-trunk-lite-rtlsdr.rules
install -Dm644 trunk-lite.desktop "$PREFIX/share/applications/trunk-lite.desktop"
install -Dm644 trunk-lite.png "$PREFIX/share/icons/hicolor/256x256/apps/trunk-lite.png"
udevadm control --reload-rules 2>/dev/null && udevadm trigger --subsystem-match=usb 2>/dev/null || true
echo "Installed $PREFIX/bin/trunk-lite ($("$PREFIX/bin/trunk-lite" --version))."
echo "Unplug and replug the dongle(s), then run: trunk-lite"
