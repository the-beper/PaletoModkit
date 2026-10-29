#!/bin/sh
# Downloads a file whose SHA-256 is known beforehand, and only then makes it usable.
# Usage: packaging/fetch-pinned.sh <url> <sha256> <dest> [executable]
#   The file is fetched to <dest>.part, checked with sha256sum, and moved to <dest> (made executable if the
#   fourth argument is "executable"). A <dest> that already has the right hash is kept as it is; one that has
#   not is replaced. On a mismatch nothing is left behind and nothing is ever run: exit 2.
#   file:// URLs work (the tests use them); others need curl.
set -eu

[ $# -ge 3 ] || { echo "usage: $0 <url> <sha256> <dest> [executable]" >&2; exit 1; }
url=$1
sha=$2
dest=$3
mode=${4:-}
printf '%s\n' "$sha" | grep -Eq '^[0-9a-f]{64}$' || { echo "fetch-pinned: not a SHA-256: $sha" >&2; exit 2; }

if [ -f "$dest" ] && printf '%s  %s\n' "$sha" "$dest" | sha256sum -c --status 2> /dev/null; then
    [ "$mode" != executable ] || chmod +x "$dest"
    exit 0
fi
mkdir -p "$(dirname "$dest")"
rm -f "$dest" "$dest.part"
trap 'rm -f "$dest.part"' EXIT
case $url in
    file://*) cp "${url#file://}" "$dest.part" ;;
    https://*) curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 --output "$dest.part" "$url" ;;
    *) echo "fetch-pinned: only https:// and file:// URLs: $url" >&2; exit 1 ;;
esac
if ! printf '%s  %s\n' "$sha" "$dest.part" | sha256sum -c --status 2> /dev/null; then
    echo "fetch-pinned: $url does not have the SHA-256 $sha (got $(sha256sum "$dest.part" | cut -d' ' -f1))" >&2
    exit 2
fi
[ "$mode" != executable ] || chmod +x "$dest.part"
mv "$dest.part" "$dest"
