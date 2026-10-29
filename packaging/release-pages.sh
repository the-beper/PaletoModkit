#!/bin/sh
# The published Flatpak repository (the gh-pages branch), without credentials, for the signing job.
#   release-pages.sh fetch DIR [--bootstrap]
#       clones gh-pages (one commit deep) to DIR, without its .git, recreates the empty directories OSTree
#       needs in DIR/repo (git keeps none), and writes DIR.base with what was fetched: gh_pages and
#       gh_pages_previous (the commits, empty if the branch does not exist) and subject (of the gh-pages
#       commit). The branch missing is an error (exit 5) unless --bootstrap, the very first publication, in
#       which case DIR is empty; --bootstrap with an existing gh-pages is an error too.
# Environment: REMOTE (default https://github.com/$GITHUB_REPOSITORY.git), RELEASE_CONF.
# Exit codes: see release-lib.sh.
set -eu
# shellcheck source=/dev/null
. "$(dirname "$0")/release-lib.sh"

[ "${1:-}" = fetch ] && [ $# -ge 2 ] || die 1 "usage: $0 fetch DIR [--bootstrap]"
DIR=${2%/}
BOOTSTRAP=0
case ${3:-} in
    "") ;;
    --bootstrap) BOOTSTRAP=1 ;;
    *) die 1 "unknown argument $3" ;;
esac
release_init

rm -rf "$DIR" "$DIR.base"
head=$(remote_head gh-pages)
prev=$(remote_head gh-pages-previous)
if [ -z "$head" ]; then
    [ "$BOOTSTRAP" = 1 ] || die 5 "the gh-pages branch does not exist: --bootstrap creates it, once"
    mkdir -p "$DIR"
    printf 'gh_pages=\ngh_pages_previous=%s\nsubject=\n' "$prev" > "$DIR.base"
    echo "gh-pages does not exist: bootstrap"
    exit 0
fi
[ "$BOOTSTRAP" = 0 ] || die 5 "gh-pages exists ($head): --bootstrap is only for the very first publication"
git clone -q --depth 1 --branch gh-pages "$REMOTE" "$DIR" || die 5 "cannot clone gh-pages"
got=$(git -C "$DIR" rev-parse HEAD)
subject=$(git -C "$DIR" log -1 --format=%s)
rm -rf "$DIR/.git"
if [ -d "$DIR/repo" ]; then
    mkdir -p "$DIR/repo/tmp" "$DIR/repo/state" "$DIR/repo/refs/heads" "$DIR/repo/refs/mirrors" "$DIR/repo/refs/remotes"
fi
printf 'gh_pages=%s\ngh_pages_previous=%s\nsubject=%s\n' "$got" "$prev" "$subject" > "$DIR.base"
echo "fetched gh-pages $got"
