#!/bin/sh
# Checks reproducible_tar (packaging/release-lib.sh), which makes the release tarball: the same contents give the
# same bytes whatever the file times, creation order or umask, and different contents or another time do not.
# Needs GNU tar, gzip, sha256sum. No network, no git.
#   sh packaging/tests/tarball-test.sh
set -eu

tar --version 2> /dev/null | grep -q "GNU tar" || { echo "SKIP: GNU tar is needed"; exit 77; }

HERE=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=/dev/null
. "$HERE/../release-lib.sh"

T=$(mktemp -d "${TMPDIR:-/tmp}/tarball-test.XXXXXX")
trap 'rm -rf "$T"' EXIT
fail=0

# tree DIR ORDER UMASK MTIME: the same files, made in the order "a" (a.txt first) or "b" (z.txt first), under
# UMASK, with every mtime set to MTIME.
tree() {
    (
        umask "$3"
        mkdir -p "$1/pkg/bin" "$1/pkg/share"
        if [ "$2" = a ]; then
            echo one > "$1/pkg/share/a.txt"
            echo two > "$1/pkg/share/z.txt"
        else
            echo two > "$1/pkg/share/z.txt"
            echo one > "$1/pkg/share/a.txt"
        fi
        printf '#!/bin/sh\n' > "$1/pkg/bin/run"
        chmod 755 "$1/pkg/bin/run"
        find "$1/pkg" -exec touch -h -d "$4" {} +
    )
}
sum() { sha256sum "$1" | cut -d' ' -f1; }
ok() { echo "ok   $1"; }
bad() {
    echo "FAIL $1"
    fail=1
}
same() { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 ($2 != $3)"; fi; }
differ() { if [ "$2" != "$3" ]; then ok "$1"; else bad "$1 (both $2)"; fi; }

tree "$T/one" a 022 '2020-01-01 00:00:00'
tree "$T/two" b 077 '2025-06-30 12:34:56'
reproducible_tar "$T/one" pkg "$T/one.tar.gz" 1700000000
reproducible_tar "$T/two" pkg "$T/two.tar.gz" 1700000000
same "other mtimes, order and umask: same archive" "$(sum "$T/one.tar.gz")" "$(sum "$T/two.tar.gz")"

reproducible_tar "$T/one" pkg "$T/later.tar.gz" 1700000001
differ "other epoch: other archive" "$(sum "$T/one.tar.gz")" "$(sum "$T/later.tar.gz")"

echo three > "$T/two/pkg/share/z.txt"
reproducible_tar "$T/two" pkg "$T/changed.tar.gz" 1700000000
differ "other contents: other archive" "$(sum "$T/one.tar.gz")" "$(sum "$T/changed.tar.gz")"

# The metadata the archive carries.
names=$(tar -tf "$T/one.tar.gz")
same "names are sorted" "$names" "$(printf '%s\n' "$names" | LC_ALL=C sort)"
listing=$(TZ=UTC tar -tvf "$T/one.tar.gz" --numeric-owner)
if printf '%s\n' "$listing" | grep -qv -- ' 0/0 '; then bad "a member is not owned by 0/0"; else ok "every member is owned by 0/0"; fi
if printf '%s\n' "$listing" | grep -qv -- ' 2023-11-14 22:13 '; then bad "a member has another mtime"; else ok "every member has the given mtime"; fi
if printf '%s\n' "$listing" | grep -q -- '^-rwxr-xr-x .* pkg/bin/run$' && printf '%s\n' "$listing" | grep -q -- '^-rw-r--r-- .* pkg/share/a.txt$'; then ok "modes are 755 and 644"; else bad "modes: $listing"; fi
# gzip -n: the time field of the header (bytes 4 to 7) is zero.
same "gzip header has no time" "$(od -An -tx1 -j4 -N4 "$T/one.tar.gz" | tr -d ' ')" 00000000

exit "$fail"
