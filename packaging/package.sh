#!/usr/bin/env bash
# Package a built trunk-pro binary for release.
#
#   packaging/package.sh <kind> <version> <binary> <out-dir>
#
#   kind: macos | linux-x86_64 | linux-aarch64 | windows-x86_64 | browser
#   (browser: <binary> is the built web/dist-web folder)
#
# Writes into <out-dir>:
#   macos    trunk-pro-<v>-macos.dmg               Trunk Recorder Pro.app (universal)
#            trunk-pro-<v>-macos-universal.tar.gz  the command-line binary
#   linux-*  trunk-pro-<v>-linux-<arch>.tar.gz     binary (glibc ≥ 2.28), install.sh, udev rule, service, menu entry
#   windows  trunk-pro-<v>-windows-x86_64.zip
#   browser  trunk-pro-<v>-browser.zip             the WebAssembly build, for any static web server
#
# Each package carries README.md, LICENSE and THIRD-PARTY-NOTICES.txt (made by
# scripts/third_party_notices.py if <out-dir> doesn't have one yet).
#
# macOS signing (optional; otherwise the app is signed ad hoc):
#   MACOS_SIGN_IDENTITY   "Developer ID Application: …" (in the keychain)
#   APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD   notarize and staple the DMG
set -euo pipefail

kind=$1 version=$2 bin=$3 out=$4
root=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

py=$(command -v python3 || command -v python)
notices="$out/THIRD-PARTY-NOTICES.txt"
if [ ! -s "$notices" ]; then
  "$py" "$root/scripts/third_party_notices.py" > "$notices"
fi

# Plain tarballs: no macOS extended attributes / AppleDouble files.
tgz() { # <dir> <archive>
  if tar --version 2>/dev/null | grep -q bsdtar; then
    COPYFILE_DISABLE=1 tar --no-xattrs --no-mac-metadata -C "$(dirname "$1")" -czf "$2" "$(basename "$1")"
  else
    tar -C "$(dirname "$1")" -czf "$2" "$(basename "$1")"
  fi
}

docs() { # <dir> [license name]
  cp "$root/README.md" "$1/README.md"
  cp "$root/LICENSE" "$1/${2:-LICENSE}"
  cp "$notices" "$1/THIRD-PARTY-NOTICES.txt"
}

case "$kind" in
macos)
  app="$work/dmg/Trunk Recorder Pro.app"
  mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
  cp "$bin" "$app/Contents/MacOS/trunk-pro"
  chmod 755 "$app/Contents/MacOS/trunk-pro"
  sed "s/@VERSION@/$version/g" "$root/packaging/macos/Info.plist" > "$app/Contents/Info.plist"
  cp "$root/packaging/icons/AppIcon.icns" "$app/Contents/Resources/AppIcon.icns"
  if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
    codesign --force --options runtime --timestamp --entitlements "$root/packaging/macos/entitlements.plist" --sign "$MACOS_SIGN_IDENTITY" "$app"
  else
    codesign --force --sign - "$app"
  fi
  codesign --verify --strict "$app"
  ln -s /Applications "$work/dmg/Applications"
  docs "$work/dmg"
  dmg="$out/trunk-pro-$version-macos.dmg"
  rm -f "$dmg"
  hdiutil create -quiet -volname "Trunk Recorder Pro" -srcfolder "$work/dmg" -fs HFS+ -format UDZO -ov "$dmg"
  if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
    codesign --force --timestamp --sign "$MACOS_SIGN_IDENTITY" "$dmg"
    if [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ]; then
      xcrun notarytool submit "$dmg" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD" --wait
      xcrun stapler staple "$dmg"
    fi
  fi
  # The command-line binary alone (servers, scripts).
  d="$work/trunk-pro-$version-macos-universal"
  mkdir -p "$d"
  cp "$app/Contents/MacOS/trunk-pro" "$d/"
  docs "$d"
  tgz "$d" "$out/trunk-pro-$version-macos-universal.tar.gz"
  ;;
linux-*)
  d="$work/trunk-pro-$version-$kind"
  mkdir -p "$d"
  install -m755 "$bin" "$d/trunk-pro"
  install -m755 "$root/packaging/linux/install.sh" "$d/install.sh"
  cp "$root/packaging/linux/60-trunk-pro-rtlsdr.rules" "$root/packaging/linux/trunk-pro.service" "$root/packaging/linux/trunk-pro.desktop" "$d/"
  cp "$root/packaging/icons/trunk-pro.png" "$d/"
  docs "$d"
  tgz "$d" "$out/trunk-pro-$version-$kind.tar.gz"
  ;;
windows-*)
  d="$work/trunk-pro-$version-$kind"
  mkdir -p "$d"
  cp "$bin" "$d/trunk-pro.exe"
  docs "$d" LICENSE.txt
  (cd "$work" && "$py" -m zipfile -c "$out/trunk-pro-$version-$kind.zip" "$(basename "$d")")
  ;;
browser)
  d="$work/trunk-pro-$version-browser"
  cp -R "$bin" "$d"
  docs "$d"
  (cd "$work" && "$py" -m zipfile -c "$out/trunk-pro-$version-browser.zip" "$(basename "$d")")
  ;;
*)
  echo "unknown kind: $kind" >&2
  exit 2
  ;;
esac
ls -la "$out"
