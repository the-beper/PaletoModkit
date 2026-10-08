#!/bin/sh
# Builds the release artifacts in dist/:
#   PaletoModkit-<arch>.tar.gz     binaries + desktop files + install.sh (in a paleto-modkit-<version>/ folder)
# Both include paleto-cwconv (lib/paleto-modkit/cwconv), which needs the
# CodeWalker submodule and the .NET 10 SDK (or podman, see build-cwconv.sh).
#   PaletoModkit-<arch>.AppImage   single portable file
# The names carry no version, so a release's download links are the same every time
# (releases/latest/download/PaletoModkit-x86_64.AppImage); the Flatpak bundle is made
# by the release workflow as PaletoModkit-x86_64.flatpak.
set -eu

ID=io.github.the_beper.PaletoModkit
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
ARCH=$(uname -m)
DIST=$ROOT/dist
NAME=paleto-modkit-$VERSION

cargo build --release --locked -p paleto-modkit -p paleto-cli
# The Legacy -> Enhanced converter (.NET, CodeWalker based).
OUT=target/cwconv ./packaging/build-cwconv.sh
rm -rf "$DIST" && mkdir -p "$DIST"

# Shared install tree: bin/ + share/.
stage() {
    install -Dm755 target/release/paleto-modkit "$1/bin/paleto-modkit"
    install -Dm755 target/release/paleto "$1/bin/paleto"
    mkdir -p "$1/lib/paleto-modkit"
    cp -r target/cwconv "$1/lib/paleto-modkit/cwconv"
    install -Dm644 packaging/$ID.desktop "$1/share/applications/$ID.desktop"
    install -Dm644 packaging/$ID.metainfo.xml "$1/share/metainfo/$ID.metainfo.xml"
    install -Dm644 packaging/$ID.mime.xml "$1/share/mime/packages/$ID.xml"
    (cd packaging/icons && find hicolor -type f) | while read -r f; do
        install -Dm644 "packaging/icons/$f" "$1/share/icons/$f"
    done
}

# Tarball.
stage "$DIST/$NAME"
install -Dm755 packaging/install.sh "$DIST/$NAME/install.sh"
cp README.md LICENSE THIRD_PARTY.md "$DIST/$NAME/"
tar -C "$DIST" -czf "$DIST/PaletoModkit-$ARCH.tar.gz" "$NAME"
rm -rf "$DIST/$NAME"

# AppImage.
APPDIR=$DIST/AppDir
stage "$APPDIR/usr"
cp packaging/$ID.desktop "$APPDIR/"
cp packaging/icons/hicolor/256x256/apps/$ID.png "$APPDIR/"
ln -s usr/bin/paleto-modkit "$APPDIR/AppRun"
TOOL=$ROOT/target/tools/appimagetool-$ARCH.AppImage
if [ ! -x "$TOOL" ]; then
    mkdir -p "$(dirname "$TOOL")"
    curl -fsSL -o "$TOOL" "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage"
    chmod +x "$TOOL"
fi
# Extract-and-run works without FUSE (CI runners, containers).
APPIMAGE_EXTRACT_AND_RUN=1 ARCH=$ARCH "$TOOL" --no-appstream "$APPDIR" "$DIST/PaletoModkit-$ARCH.AppImage"
rm -rf "$APPDIR"

(cd "$DIST" && sha256sum ./* > SHA256SUMS)
ls -l "$DIST"
