#!/usr/bin/env bash
# Turn the Tauri .deb into an apt repository under apt-repo/.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

VERSION=$(python3 -c 'import json; print(json.load(open("src-tauri/tauri.conf.json"))["version"])')
DEB_VERSION="${VERSION}-1"
ARCH=$(dpkg --print-architecture)
MAINTAINER=${MAINTAINER:-"Nexus DB <dev@nexusdb.app>"}

SRC_DEB=$(find src-tauri/target/release/bundle/deb -maxdepth 1 -type f -name '*.deb' -printf '%T@ %p\n' | sort -n | tail -1 | cut -d' ' -f2-)
if [[ -z "${SRC_DEB}" || ! -f "$SRC_DEB" ]]; then
  echo "No .deb found. Build one with: npm run tauri build -- --bundles deb" >&2
  exit 1
fi

WORKDIR=$(mktemp -d)
trap 'rm -rf "$WORKDIR"' EXIT
dpkg-deb -R "$SRC_DEB" "$WORKDIR/pkg"

DESKTOP_DIR="$WORKDIR/pkg/usr/share/applications"
mkdir -p "$DESKTOP_DIR"
rm -f "$DESKTOP_DIR"/*.desktop
cat > "$DESKTOP_DIR/nexus-db.desktop" <<'EOF'
[Desktop Entry]
Categories=Development;Database;
Comment=One client for every database
Exec=nexus-db
StartupWMClass=nexus-db
Icon=nexus-db
Name=Nexus DB
Terminal=false
Type=Application
EOF

DOC="$WORKDIR/pkg/usr/share/doc/nexus-db"
mkdir -p "$DOC"
cat > "$WORKDIR/changelog" <<EOF
nexus-db (${DEB_VERSION}) stable; urgency=medium

  * Package Nexus DB ${VERSION} for apt.

 -- ${MAINTAINER}  $(date -R)
EOF
gzip -9n -c "$WORKDIR/changelog" > "$DOC/changelog.Debian.gz"
chmod 644 "$DOC/changelog.Debian.gz" "$DESKTOP_DIR/nexus-db.desktop"

INSTALLED_SIZE=$(du -sk --exclude=DEBIAN "$WORKDIR/pkg" | cut -f1)
cat > "$WORKDIR/pkg/DEBIAN/control" <<EOF
Package: nexus-db
Version: ${DEB_VERSION}
Architecture: ${ARCH}
Maintainer: ${MAINTAINER}
Installed-Size: ${INSTALLED_SIZE}
Section: database
Priority: optional
Depends: libwebkit2gtk-4.1-0, libgtk-3-0
Description: one client for every database
 Nexus DB is a desktop client for PostgreSQL, MySQL, SQLite, SQL Server,
 MongoDB, ArangoDB, Redis, ClickHouse, and Elasticsearch.
EOF

chmod -R u+rwX,go+rX,go-w "$WORKDIR/pkg"
(cd "$WORKDIR/pkg" && find . -path ./DEBIAN -prune -o -type f -print0 | sort -z | xargs -0 md5sum | sed 's|  \./|  |') > "$WORKDIR/pkg/DEBIAN/md5sums"

REPO="$ROOT/apt-repo"
POOL="$REPO/pool/main/n/nexus-db"
DIST="$REPO/dists/stable/main/binary-${ARCH}"
rm -rf "$REPO"
mkdir -p "$POOL" "$DIST"
dpkg-deb --root-owner-group -Zxz --build "$WORKDIR/pkg" "$POOL/nexus-db_${DEB_VERSION}_${ARCH}.deb" >/dev/null

(cd "$REPO" && dpkg-scanpackages --arch "$ARCH" pool /dev/null > "dists/stable/main/binary-${ARCH}/Packages")
gzip -9n -c "$DIST/Packages" > "$DIST/Packages.gz"

python3 - "$REPO" "$ARCH" <<'PY'
import hashlib, os, sys
from datetime import datetime, timezone
from email.utils import format_datetime
from pathlib import Path

repo, arch = Path(sys.argv[1]), sys.argv[2]
dist = repo / "dists" / "stable"
files = [
    dist / "main" / f"binary-{arch}" / "Packages",
    dist / "main" / f"binary-{arch}" / "Packages.gz",
]

def sums(algo):
    lines = []
    for path in files:
        data = path.read_bytes()
        digest = hashlib.new(algo, data).hexdigest()
        rel = path.relative_to(dist).as_posix()
        lines.append(f" {digest} {len(data)} {rel}")
    return "\n".join(lines)

release = f"""Origin: Nexus DB
Label: Nexus DB
Suite: stable
Codename: stable
Architectures: {arch}
Components: main
Description: Nexus DB packages
Date: {format_datetime(datetime.now(timezone.utc))}
MD5Sum:
{sums("md5")}
SHA256:
{sums("sha256")}
"""
(dist / "Release").write_text(release)
uri = repo.resolve().as_uri()
(repo / "nexus-db.sources").write_text(
    "Types: deb\n"
    f"URIs: {uri}\n"
    "Suites: stable\n"
    "Components: main\n"
    "Trusted: yes\n"
)
print(f"deb [trusted=yes] {uri} stable main")
PY

echo "Repository: $REPO"
echo "Package: $POOL/nexus-db_${DEB_VERSION}_${ARCH}.deb"
