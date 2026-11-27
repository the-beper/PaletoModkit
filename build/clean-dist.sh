#!/bin/sh
# Makes dist/ ready for build-release.sh: removes what that script makes (the tarball, the AppImage and its
# .zsync, linux-build.txt, and its two working folders) and nothing else, so other files kept in dist/ (the
# Flatpak bundle, notes, work files) stay. Creates dist/ if it is missing.
#   clean-dist.sh DIST NAME ARCH     NAME is paleto-modkit-<version>
set -eu

[ $# -eq 3 ] || { echo "usage: $0 DIST NAME ARCH" >&2; exit 2; }
DIST=$1
NAME=$2
ARCH=$3
case $NAME in '' | */* | . | ..) echo "clean-dist.sh: bad NAME: $NAME" >&2; exit 2 ;; esac
case $ARCH in '' | */* | . | ..) echo "clean-dist.sh: bad ARCH: $ARCH" >&2; exit 2 ;; esac

mkdir -p "$DIST"
rm -rf "${DIST:?}/$NAME" "$DIST/AppDir"
rm -f "$DIST/PaletoModkit-$ARCH.tar.gz" "$DIST/PaletoModkit-$ARCH.AppImage" "$DIST/PaletoModkit-$ARCH.AppImage.zsync" "$DIST/linux-build.txt"
