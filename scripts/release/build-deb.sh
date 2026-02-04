#!/usr/bin/env bash
set -euo pipefail

VERSION="${VERSION:-0.1.0}"
ARCH="${ARCH:-arm64}"
PKGNAME="${PKGNAME:-aadk}"

ROOT="dist/deb/${PKGNAME}_${VERSION}_${ARCH}"
INSTALL_ROOT="$ROOT/opt/aadk-${VERSION}"
BIN_DIR="$INSTALL_ROOT/bin"
DOC_DIR="$ROOT/usr/share/doc/${PKGNAME}"
APP_DIR="$ROOT/usr/share/applications"
ICON_DIR="$ROOT/usr/share/icons/hicolor/scalable/apps"
DEBIAN_DIR="$ROOT/DEBIAN"
OUTPUT="dist/${PKGNAME}_${VERSION}_${ARCH}.deb"

if ! command -v dpkg-deb >/dev/null 2>&1; then
  echo "ERROR: dpkg-deb not found. Install dpkg-dev." >&2
  exit 1
fi

if [ ! -f "scripts/release/aadk-start.sh" ]; then
  echo "ERROR: missing scripts/release/aadk-start.sh" >&2
  exit 1
fi

if [ ! -f "packaging/deb/control.in" ]; then
  echo "ERROR: missing packaging/deb/control.in" >&2
  exit 1
fi

if [ ! -f "packaging/deb/aadk.desktop" ]; then
  echo "ERROR: missing packaging/deb/aadk.desktop" >&2
  exit 1
fi

if [ ! -f "assets/aadk.svg" ]; then
  echo "ERROR: missing assets/aadk.svg" >&2
  exit 1
fi

mkdir -p dist

cargo build --release --workspace

rm -rf "$ROOT"
install -d "$BIN_DIR" "$DOC_DIR" "$APP_DIR" "$ICON_DIR" "$DEBIAN_DIR" "$ROOT/usr/bin" "$ROOT/opt"

install -m 755 target/release/aadk-{core,workflow,toolchain,project,build,targets,observe,ui,cli} "$BIN_DIR/"
install -m 755 scripts/release/aadk-start.sh "$BIN_DIR/aadk-start"

install -m 644 README.md LICENSE "$DOC_DIR/"
install -m 644 packaging/deb/aadk.desktop "$APP_DIR/aadk.desktop"
install -m 644 assets/aadk.svg "$ICON_DIR/aadk.svg"

ln -s "aadk-${VERSION}" "$ROOT/opt/aadk"
ln -s /opt/aadk/bin/aadk-start "$ROOT/usr/bin/aadk"
ln -s /opt/aadk/bin/aadk-ui "$ROOT/usr/bin/aadk-ui"
ln -s /opt/aadk/bin/aadk-cli "$ROOT/usr/bin/aadk-cli"

sed -e "s/@VERSION@/${VERSION}/g" -e "s/@ARCH@/${ARCH}/g" packaging/deb/control.in > "$DEBIAN_DIR/control"
install -m 755 packaging/deb/postinst "$DEBIAN_DIR/postinst"
install -m 755 packaging/deb/postrm "$DEBIAN_DIR/postrm"

rm -f "$OUTPUT"
dpkg-deb --build --root-owner-group "$ROOT" "$OUTPUT"
sha256sum "$OUTPUT" > "$OUTPUT.sha256"

echo "Built $OUTPUT"
