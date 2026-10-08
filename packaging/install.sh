#!/bin/sh
# Installs Paleto Modkit with desktop integration (menu entry, icon, file
# associations). Run from a release archive or, after `cargo build --release`,
# from the source tree.
#
#   ./install.sh               install for the current user (~/.local)
#   PREFIX=/usr/local sudo -E ./install.sh
#   ./install.sh --uninstall
set -eu

ID=io.github.the_beper.PaletoModkit
PREFIX=${PREFIX:-$HOME/.local}
HERE=$(cd "$(dirname "$0")" && pwd)

# Release archive layout (bin/ + share/) or source tree (target/release + packaging/).
if [ -x "$HERE/bin/paleto-modkit" ]; then
    BIN=$HERE/bin
    SHARE=$HERE/share
    CWCONV=$HERE/lib/paleto-modkit/cwconv
else
    BIN=$HERE/../target/release
    SHARE=
    CWCONV=$HERE/../target/cwconv
fi

files() {
    echo "bin/paleto-modkit"
    echo "bin/paleto"
    echo "share/applications/$ID.desktop"
    echo "share/metainfo/$ID.metainfo.xml"
    echo "share/mime/packages/$ID.xml"
    echo "share/icons/hicolor/scalable/apps/$ID.svg"
    echo "share/icons/hicolor/symbolic/apps/$ID-symbolic.svg"
    for s in 16 24 32 48 64 128 256 512; do
        echo "share/icons/hicolor/${s}x${s}/apps/$ID.png"
    done
}

refresh() {
    command -v update-desktop-database >/dev/null && update-desktop-database -q "$PREFIX/share/applications" || true
    command -v update-mime-database >/dev/null && update-mime-database "$PREFIX/share/mime" || true
    command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" || true
    # KDE Plasma rebuilds its menu cache from this.
    command -v kbuildsycoca6 >/dev/null && kbuildsycoca6 >/dev/null 2>&1 || true
}

if [ "${1:-}" = "--uninstall" ]; then
    files | while read -r f; do rm -f "$PREFIX/$f"; done
    rm -rf "$PREFIX/lib/paleto-modkit"
    refresh
    echo "Paleto Modkit removed from $PREFIX"
    exit 0
fi

[ -x "$BIN/paleto-modkit" ] || { echo "paleto-modkit not built: run cargo build --release" >&2; exit 1; }

install -Dm755 "$BIN/paleto-modkit" "$PREFIX/bin/paleto-modkit"
install -Dm755 "$BIN/paleto" "$PREFIX/bin/paleto"
# The Legacy -> Enhanced converter, when built (packaging/build-cwconv.sh).
if [ -x "$CWCONV/paleto-cwconv" ]; then
    rm -rf "$PREFIX/lib/paleto-modkit/cwconv"
    mkdir -p "$PREFIX/lib/paleto-modkit"
    cp -r "$CWCONV" "$PREFIX/lib/paleto-modkit/cwconv"
else
    echo "note: paleto-cwconv not built, so Legacy files cannot be converted (packaging/build-cwconv.sh)"
fi
if [ -n "$SHARE" ]; then
    (cd "$SHARE" && find . -type f) | while read -r f; do
        install -Dm644 "$SHARE/$f" "$PREFIX/share/$f"
    done
else
    install -Dm644 "$HERE/$ID.desktop" "$PREFIX/share/applications/$ID.desktop"
    install -Dm644 "$HERE/$ID.metainfo.xml" "$PREFIX/share/metainfo/$ID.metainfo.xml"
    install -Dm644 "$HERE/$ID.mime.xml" "$PREFIX/share/mime/packages/$ID.xml"
    (cd "$HERE/icons" && find hicolor -type f) | while read -r f; do
        install -Dm644 "$HERE/icons/$f" "$PREFIX/share/icons/$f"
    done
fi
refresh
echo "Paleto Modkit installed in $PREFIX"
case ":$PATH:" in *":$PREFIX/bin:"*) ;; *) echo "note: $PREFIX/bin is not in your PATH" ;; esac
