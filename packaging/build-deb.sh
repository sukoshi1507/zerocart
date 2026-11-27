#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Builds the Debian package of ZeroCart (arm64, statically linked: it needs no library of the device).
#
#   packaging/build-deb.sh [--revision rev1]
#
# Needs the aarch64 Rust target and aarch64-linux-gnu-gcc (see .cargo/config.toml). Set OFFLINE=1 to build with
# `cargo --offline`. Output: dist/ZeroCart_<version>_<revision>_arm64.deb
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
CARGO=${CARGO:-cargo}
REVISION=rev1
while [ $# -gt 0 ]; do
    case "$1" in
        --revision) shift; REVISION=$1 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)
[ -n "$VERSION" ] || { echo "cannot read the version from Cargo.toml" >&2; exit 1; }
ARCH=arm64
NAME=ZeroCart_${VERSION}_${REVISION}_${ARCH}
STAGE="$ROOT/dist/deb/$NAME"
OUT="$ROOT/dist/$NAME.deb"
MAINTAINER="sukoshi1507 <17269210+sukoshi1507@users.noreply.github.com>"
OFFLINE_FLAG=""
[ "${OFFLINE:-0}" = 1 ] && OFFLINE_FLAG="--offline"

echo "== building zerocart $VERSION for aarch64"
(cd "$ROOT" && "$CARGO" build --release $OFFLINE_FLAG --target aarch64-unknown-linux-gnu \
    -p zerocart-app -p zerocart-core -p zerocart-flashchips -p zerocart-send)
BINDIR="$ROOT/target/aarch64-unknown-linux-gnu/release"
for b in zerocart-app zerocart zerocart-flash zerocart-chis zerocart-chis-flash zerocart-chis-gba-flash zerocart-md zerocart-share; do
    [ -x "$BINDIR/$b" ] || { echo "missing $BINDIR/$b" >&2; exit 1; }
done

echo "== staging $STAGE"
rm -rf "$STAGE"
install -d -m 755 "$STAGE/DEBIAN" "$STAGE/usr/bin" "$STAGE/usr/share/zerocart/configs" \
    "$STAGE/usr/share/APPLaunch/applications" "$STAGE/usr/share/APPLaunch/share/images" "$STAGE/usr/share/doc/zerocart"
for b in zerocart-app zerocart zerocart-flash zerocart-chis zerocart-chis-flash zerocart-chis-gba-flash zerocart-md zerocart-share; do
    install -m 755 "$BINDIR/$b" "$STAGE/usr/bin/$b"
done
for f in "$ROOT"/configs/db_*.json "$ROOT"/configs/fc_*.txt; do
    install -m 644 "$f" "$STAGE/usr/share/zerocart/configs/"
done
for icon in zerocart.png zerocart_100.png zerocart_80.png; do
    install -m 644 "$ROOT/assets/images/$icon" "$STAGE/usr/share/APPLaunch/share/images/$icon"
done
cat > "$STAGE/usr/share/APPLaunch/applications/zerocart.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=ZeroCart
Comment=Game Boy / Color / Advance cartridge reader and flasher (GBFlash)
Exec=/usr/bin/zerocart-app
Icon=share/images/zerocart.png
Terminal=false
Categories=Utility;
StartupNotify=false
DESKTOP
chmod 644 "$STAGE/usr/share/APPLaunch/applications/zerocart.desktop"
install -m 644 "$ROOT/README.md" "$STAGE/usr/share/doc/zerocart/README.md"
install -m 644 "$ROOT/THIRD_PARTY.md" "$STAGE/usr/share/doc/zerocart/THIRD_PARTY.md"
install -m 644 "$ROOT/packaging/copyright" "$STAGE/usr/share/doc/zerocart/copyright"

# No maintainer scripts: nothing to configure, nothing runs as root at install time.
SIZE=$(du -sk --exclude=DEBIAN "$STAGE" | cut -f1)
cat > "$STAGE/DEBIAN/control" <<CONTROL
Package: zerocart
Version: ${VERSION}-${REVISION}
Architecture: ${ARCH}
Maintainer: ${MAINTAINER}
Installed-Size: ${SIZE}
Section: utils
Priority: optional
Depends: fonts-noto-cjk, alsa-utils
Homepage: https://github.com/lesserkuma/FlashGBX
Description: Game Boy / Color / Advance cartridge reader and flasher for CardputerZero
 Reads and writes Game Boy, Game Boy Color and Game Boy Advance cartridges
 with a GBFlash reader plugged into the USB port: ROM and save dumps checked
 against the FlashGBX database, save restore, and ROM flashing on rewritable
 cartridges, always after a double-read backup and with a read-back check.
 Protocol and chip databases ported from FlashGBX by Lesserkuma. Statically
 linked Rust build; the fonts of fonts-noto-cjk draw the interface.
CONTROL

echo "== packaging $OUT"
mkdir -p "$ROOT/dist"
dpkg-deb --root-owner-group --build "$STAGE" "$OUT"
dpkg-deb --info "$OUT" | sed -n 1,20p
echo "== contents"
dpkg-deb --contents "$OUT" | awk '{print $1, $3, $6}' | grep -v "configs/fc_" | head -30
echo "== done: $OUT ($(du -h "$OUT" | cut -f1))"
