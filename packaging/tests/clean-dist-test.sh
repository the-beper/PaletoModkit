#!/bin/sh
# Checks clean-dist.sh, which build-release.sh runs before it builds: it removes what the build makes and
# nothing else, so a foreign file or folder kept in dist/ survives. No network, no git.
#   sh packaging/tests/clean-dist-test.sh
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
CLEAN=$HERE/../clean-dist.sh

T=$(mktemp -d "${TMPDIR:-/tmp}/clean-dist-test.XXXXXX")
trap 'rm -rf "$T"' EXIT
fail=0
ok() { echo "ok   $1"; }
bad() {
    echo "FAIL $1"
    fail=1
}
gone() { if [ ! -e "$2" ] && [ ! -L "$2" ]; then ok "$1"; else bad "$1 ($2 is still there)"; fi; }
kept() { if [ -e "$2" ] || [ -L "$2" ]; then ok "$1"; else bad "$1 ($2 is gone)"; fi; }

D=$T/dist
mkdir -p "$D/paleto-modkit-1.2.3/bin" "$D/AppDir/usr" "$D/notes" "$D/other-version/x"
for f in PaletoModkit-x86_64.tar.gz PaletoModkit-x86_64.AppImage PaletoModkit-x86_64.AppImage.zsync linux-build.txt paleto-modkit-1.2.3/bin/run AppDir/usr/f; do
    echo old > "$D/$f"
done
# Foreign: what other tools and the user keep in dist/, a flatpak bundle, another architecture, a link to a folder outside.
for f in PaletoModkit-x86_64.flatpak flatpak-build.txt PaletoModkit-aarch64.tar.gz work.md notes/todo.txt other-version/x/f paleto-modkit-1.2.2.txt; do
    echo mine > "$D/$f"
done
mkdir "$T/outside"
echo precious > "$T/outside/f"
ln -s "$T/outside" "$D/link"

sh "$CLEAN" "$D" paleto-modkit-1.2.3 x86_64
for f in PaletoModkit-x86_64.tar.gz PaletoModkit-x86_64.AppImage PaletoModkit-x86_64.AppImage.zsync linux-build.txt paleto-modkit-1.2.3 AppDir; do
    gone "removes $f" "$D/$f"
done
for f in PaletoModkit-x86_64.flatpak flatpak-build.txt PaletoModkit-aarch64.tar.gz work.md notes/todo.txt other-version/x/f paleto-modkit-1.2.2.txt link; do
    kept "keeps $f" "$D/$f"
done
kept "does not follow a link out of dist/" "$T/outside/f"
if [ "$(cat "$D/work.md")" = mine ]; then ok "a foreign file is not changed"; else bad "a foreign file changed"; fi

# A missing dist/ is made; a second run on a clean one changes nothing.
sh "$CLEAN" "$T/new/dist" paleto-modkit-1.2.3 x86_64
if [ -d "$T/new/dist" ]; then ok "makes a missing dist/"; else bad "no dist/ made"; fi
sh "$CLEAN" "$D" paleto-modkit-1.2.3 x86_64
kept "a second run keeps the foreign files" "$D/work.md"

# Names that would reach outside dist/ are refused.
for bad_name in "" ".." "../x" "a/b"; do
    if sh "$CLEAN" "$D" "$bad_name" x86_64 2> /dev/null; then bad "accepted the name '$bad_name'"; else ok "refuses the name '$bad_name'"; fi
done
kept "the refused names removed nothing" "$D/work.md"

exit "$fail"
