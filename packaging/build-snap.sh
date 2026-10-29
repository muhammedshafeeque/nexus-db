#!/usr/bin/env bash
# Pack the release binary into a core26 snap. Requires snapcraft.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

BIN="src-tauri/target/release/nexus-db"
if [[ ! -x "$BIN" ]]; then
  echo "Release binary missing. Build it with: npm run tauri build" >&2
  exit 1
fi
if ! command -v snapcraft >/dev/null; then
  echo "snapcraft is not installed. Run: sudo snap install snapcraft --classic" >&2
  exit 1
fi

STAGE="snap/local"
rm -rf "$STAGE"
mkdir -p \
  "$STAGE/usr/bin" \
  "$STAGE/usr/share/applications" \
  "$STAGE/usr/share/icons/hicolor/32x32/apps" \
  "$STAGE/usr/share/icons/hicolor/128x128/apps" \
  "$STAGE/usr/share/icons/hicolor/256x256/apps" \
  snap/gui

install -m755 "$BIN" "$STAGE/usr/bin/nexus-db"
install -m644 snap/gui/nexus-db.desktop "$STAGE/usr/share/applications/nexus-db.desktop"
install -m644 src-tauri/icons/32x32.png "$STAGE/usr/share/icons/hicolor/32x32/apps/nexus-db.png"
install -m644 src-tauri/icons/128x128.png "$STAGE/usr/share/icons/hicolor/128x128/apps/nexus-db.png"
install -m644 src-tauri/icons/128x128@2x.png "$STAGE/usr/share/icons/hicolor/256x256/apps/nexus-db.png"
install -m644 src-tauri/icons/icon.png snap/gui/icon.png

if id -nG | tr ' ' '\n' | grep -qx lxd; then
  snapcraft
else
  echo "No LXD access; building with --destructive-mode."
  snapcraft --destructive-mode
fi
