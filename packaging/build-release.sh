#!/bin/sh
# Builds the release artifacts in dist/:
#   PaletoModkit-<arch>.tar.gz     binaries + desktop files + install.sh (in a paleto-modkit-<version>/ folder)
# Both include paleto-cwconv (lib/paleto-modkit/cwconv), which needs the
# CodeWalker submodule and the .NET 10 SDK (or podman, see build-cwconv.sh).
#   PaletoModkit-<arch>.AppImage   single portable file, plus its .zsync for AppImageUpdate
#   linux-build.txt                what was built: version, commit, hashes of the tools, compiler versions
# The names carry no version, so a release's download links are the same every time
# (releases/latest/download/PaletoModkit-x86_64.AppImage); the Flatpak bundle is made
# by the release workflow as PaletoModkit-x86_64.flatpak. No checksum file is written here: the release
# workflow writes SHA256SUMS for the final files (packaging/release-verify.sh).
# appimagetool and the AppImage runtime are downloaded by URL and SHA-256 from packaging/release.conf
# (packaging/fetch-pinned.sh) and hashed again before use; an empty pin or another architecture than x86_64
# refuses to build (exit 2), unless you give your own tool:
#   EXPECT_VERSION=x.y.z  refuse (exit 2) unless Cargo.toml has this version (the release workflow sets it)
#   SOURCE_DATE_EPOCH=n   mtime of every file in the tarball (default: the commit time of HEAD)
#   APPIMAGETOOL=file APPIMAGETOOL_SHA256=hash  use this appimagetool (and, optionally, APPIMAGE_RUNTIME=file
#                         APPIMAGE_RUNTIME_SHA256=hash), for other architectures or local experiments
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
# shellcheck source=/dev/null
. "$ROOT/packaging/release-lib.sh"
ID=$(conf APP_ID)
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
if [ -n "${EXPECT_VERSION:-}" ] && [ "$EXPECT_VERSION" != "$VERSION" ]; then
    die 2 "Cargo.toml has version $VERSION, expected $EXPECT_VERSION"
fi
ARCH=$(uname -m)
DIST=$ROOT/dist
NAME=paleto-modkit-$VERSION

# The tools first, so that a bad pin stops the build before anything is compiled.
TOOLS=$ROOT/target/tools
RUNTIME=""
RUNTIME_SHA=""
if [ -n "${APPIMAGETOOL:-}" ]; then
    [ -n "${APPIMAGETOOL_SHA256:-}" ] || die 2 "APPIMAGETOOL needs APPIMAGETOOL_SHA256"
    TOOL=$APPIMAGETOOL
    TOOL_SHA=$APPIMAGETOOL_SHA256
    if [ -n "${APPIMAGE_RUNTIME:-}" ]; then
        [ -n "${APPIMAGE_RUNTIME_SHA256:-}" ] || die 2 "APPIMAGE_RUNTIME needs APPIMAGE_RUNTIME_SHA256"
        RUNTIME=$APPIMAGE_RUNTIME
        RUNTIME_SHA=$APPIMAGE_RUNTIME_SHA256
    fi
else
    [ "$ARCH" = x86_64 ] || die 2 "the pinned appimagetool is for x86_64, this is $ARCH: give APPIMAGETOOL and APPIMAGETOOL_SHA256"
    TOOL_SHA=$(conf_hex APPIMAGETOOL_SHA256 64)
    RUNTIME_SHA=$(conf_hex APPIMAGE_RUNTIME_SHA256 64)
    TOOL_URL=$(conf APPIMAGETOOL_URL)
    RUNTIME_URL=$(conf APPIMAGE_RUNTIME_URL)
    [ -n "$TOOL_URL" ] && [ -n "$RUNTIME_URL" ] || die 2 "APPIMAGETOOL_URL or APPIMAGE_RUNTIME_URL is empty in $RELEASE_CONF"
    TOOL=$TOOLS/appimagetool-$TOOL_SHA
    RUNTIME=$TOOLS/runtime-$RUNTIME_SHA
    "$ROOT/packaging/fetch-pinned.sh" "$TOOL_URL" "$TOOL_SHA" "$TOOL" executable
    "$ROOT/packaging/fetch-pinned.sh" "$RUNTIME_URL" "$RUNTIME_SHA" "$RUNTIME"
fi
# The hash of what is about to run, checked again right before it does.
verify_tool() { printf '%s  %s\n' "$1" "$2" | sha256sum -c --status 2> /dev/null || die 2 "$2 does not have the pinned SHA-256 $1"; }
verify_tool "$TOOL_SHA" "$TOOL"
[ -z "$RUNTIME" ] || verify_tool "$RUNTIME_SHA" "$RUNTIME"
command -v zsyncmake > /dev/null || die 1 "missing zsyncmake: install zsync"

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
    install -Dm644 "packaging/$ID.desktop" "$1/share/applications/$ID.desktop"
    install -Dm644 "packaging/$ID.metainfo.xml" "$1/share/metainfo/$ID.metainfo.xml"
    install -Dm644 "packaging/$ID.mime.xml" "$1/share/mime/packages/$ID.xml"
    (cd packaging/icons && find hicolor -type f) | while read -r f; do
        install -Dm644 "packaging/icons/$f" "$1/share/icons/$f"
    done
}

# Tarball.
stage "$DIST/$NAME"
install -Dm755 packaging/install.sh "$DIST/$NAME/install.sh"
cp README.md LICENSE THIRD_PARTY.md "$DIST/$NAME/"
# Fixed order, owner, modes and time (the commit's), so the same files give the same archive.
reproducible_tar "$DIST" "$NAME" "$DIST/PaletoModkit-$ARCH.tar.gz" "${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}"
rm -rf "$DIST/${NAME:?}"

# AppImage.
APPDIR=$DIST/AppDir
stage "$APPDIR/usr"
cp "packaging/$ID.desktop" "$APPDIR/"
cp "packaging/icons/hicolor/256x256/apps/$ID.png" "$APPDIR/"
for lib in libxkbcommon-x11.so.0 libxcb-xkb.so.1; do
    src=$(ldconfig -p | awk -v l="$lib" '$1 == l && /x86-64|AArch64/ { print $NF; exit }')
    [ -n "$src" ] || { echo "missing $lib: install libxkbcommon-x11-0" >&2; exit 1; }
    install -Dm644 "$(readlink -f "$src")" "$APPDIR/usr/lib/$lib"
done
cat > "$APPDIR/AppRun" <<'RUN'
#!/bin/sh
HERE=$(dirname "$(readlink -f "$0")")
export LD_LIBRARY_PATH="$HERE/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$HERE/usr/bin/paleto-modkit" "$@"
RUN
chmod +x "$APPDIR/AppRun"
# Extract-and-run works without FUSE (CI runners, containers).
# The update information lets AppImageUpdate fetch deltas. appimagetool writes the .zsync file
# in the current folder, not next to the AppImage, and needs zsyncmake (the zsync package).
UPDATE="gh-releases-zsync|the-beper|PaletoModkit|latest|PaletoModkit-$ARCH.AppImage.zsync"
verify_tool "$TOOL_SHA" "$TOOL"
[ -z "$RUNTIME" ] || verify_tool "$RUNTIME_SHA" "$RUNTIME"
(cd "$DIST" && export APPIMAGE_EXTRACT_AND_RUN=1 ARCH && "$TOOL" --no-appstream ${RUNTIME:+--runtime-file "$RUNTIME"} -u "$UPDATE" "$APPDIR" "PaletoModkit-$ARCH.AppImage")
rm -rf "$APPDIR"

{
    echo "format=1"
    echo "version=$VERSION"
    echo "commit=$(git rev-parse HEAD)"
    echo "arch=$ARCH"
    echo "appimagetool_sha256=$TOOL_SHA"
    echo "appimage_runtime_sha256=${RUNTIME_SHA:-default}"
    echo "rustc=$(rustc -V)"
    echo "dotnet=$(dotnet --version 2> /dev/null || echo container)"
} > "$DIST/linux-build.txt"
ls -l "$DIST"
