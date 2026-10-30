#!/bin/sh
# Shared functions of the release scripts (sourced, never run): errors, release.conf, versions, the remote
# repository, the GitHub CLI wrappers and the checks of OSTree repositories and archives.
# Exit codes of every release script: 0 ok or nothing to do; 1 usage or internal error; 2 validation (format,
# mismatch, missing file, checksum, structure); 3 policy refusal (downgrade, same tag with another commit or
# content, Flatpak identity change); 4 signing (secret missing, wrong key, gpg or ostree failure); 5 remote
# conflict or unreadable remote (lease lost, gh-pages missing, tag moved, several releases for one tag, Pages ahead
# of the release, GitHub answer missing or malformed).
#   RELEASE_CONF  the pins file (default: release.conf next to the scripts)
#   REMOTE        git URL of the repository (default: https://github.com/$GITHUB_REPOSITORY.git)
#   GH_REPO, GH_TOKEN  for the gh wrappers; the token never goes in a URL, a git config or a command line
#   RELEASE_SIM=1 (set by packaging/tests/release-sim.sh) refuses a REMOTE, repository or site on GitHub, or a
#                 gh that is not the test stub, so a test can never touch the real repository
# This file is not executable: every script starts with  . "$(dirname "$0")/release-lib.sh"

PKG_DIR=$(cd "$(dirname "$0")" && pwd)
RELEASE_CONF=${RELEASE_CONF:-$PKG_DIR/release.conf}
PROG=$(basename "$0")

die() {
    code=$1
    shift
    echo "$PROG: $*" >&2
    exit "$code"
}

# conf KEY: the value in release.conf (parsed, never sourced).
conf() { sed -n "s/^$1=//p" "$RELEASE_CONF" | head -n1; }

# one_line VALUE: succeeds unless the value has a newline or a carriage return (grep works line by line, so a
# multi-line value could pass a pattern check on one of its lines).
LF=$(printf '\n_')
LF=${LF%_}
CR=$(printf '\r')
one_line() {
    case $1 in
        *"$LF"* | *"$CR"*) return 1 ;;
    esac
    return 0
}

is_hex() { one_line "$1" && printf '%s\n' "$1" | grep -Eq "^[0-9a-fA-F]{$2}\$"; }

# conf_hex KEY LENGTH: the value, or exit 2 when empty or not hex.
conf_hex() {
    v=$(conf "$1")
    [ -n "$v" ] || die 2 "$1 is empty in $RELEASE_CONF"
    is_hex "$v" "$2" || die 2 "$1 in $RELEASE_CONF is not a $2 digit hex value"
    printf '%s\n' "$v"
}

# Refuses anything that could reach the real repository when a test is running.
sim_guard() {
    [ -n "${RELEASE_SIM:-}" ] || return 0
    case ${REMOTE:-} in file://* | /*) ;; *) die 1 "RELEASE_SIM: REMOTE must be a local path or file:// URL" ;; esac
    case "${REMOTE:-}${GH_REPO:-}${GITHUB_REPOSITORY:-}${SITE_URL:-}" in
        *github.com* | *github.io* | *the-beper*) die 1 "RELEASE_SIM: a real repository or site was given" ;;
    esac
    case $(conf SITE_URL) in file://*) ;; *) die 1 "RELEASE_SIM: SITE_URL in the test conf must be a file:// URL" ;; esac
    [ "$(command -v gh)" = "${RELEASE_SIM_GH:-}" ] || die 1 "RELEASE_SIM: gh is not the test stub"
}

# release_init: call once after sourcing; sets REMOTE and the conf values used everywhere.
release_init() {
    [ -f "$RELEASE_CONF" ] || die 1 "$RELEASE_CONF not found"
    if [ -z "${REMOTE:-}" ]; then
        REMOTE=https://github.com/${GITHUB_REPOSITORY:-}.git
        [ -n "${GITHUB_REPOSITORY:-}" ] || die 1 "set REMOTE or GITHUB_REPOSITORY"
    fi
    GH_REPO=${GH_REPO:-${GITHUB_REPOSITORY:-}}
    export GH_REPO
    APP_ID=$(conf APP_ID)
    BRANCH=$(conf FLATPAK_BRANCH)
    [ -n "$APP_ID" ] && [ -n "$BRANCH" ] || die 2 "APP_ID or FLATPAK_BRANCH is empty in $RELEASE_CONF"
    APP_REF=app/$APP_ID/x86_64/$BRANCH
    DEBUG_REF=runtime/$APP_ID.Debug/x86_64/$BRANCH
    sim_guard
}

# ---- versions -------------------------------------------------------------------------------------------------

semver_valid() { one_line "$1" && printf '%s\n' "$1" | grep -Eq '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-rc\.[1-9][0-9]*)?$'; }
is_rc() { case $1 in *-rc.*) return 0 ;; *) return 1 ;; esac; }

# semver_key TAG: "major minor patch stable rc", the order key (a release candidate is below its release).
semver_key() {
    v=${1#v}
    if is_rc "$v"; then
        core=${v%-rc.*}
        rc=${v##*-rc.}
        st=0
    else
        core=$v
        rc=0
        st=1
    fi
    oldifs=$IFS
    IFS=.
    # shellcheck disable=SC2086
    set -- $core
    IFS=$oldifs
    printf '%s %s %s %s %s\n' "$1" "$2" "$3" "$st" "$rc"
}

# semver_cmp A B: -1, 0 or 1. The components are compared as digit strings (shorter is smaller, then
# lexicographic): awk numbers are doubles and are equal above 2^53.
semver_cmp() {
    { semver_key "$1"; semver_key "$2"; } | awk 'function cmp(x, y) {
            if (length(x) != length(y)) return (length(x) < length(y)) ? -1 : 1
            if (x == y) return 0
            return (x < y) ? -1 : 1
        }
        NR == 1 { for (i = 1; i <= 5; i++) a[i] = $i "" }
        NR == 2 { for (i = 1; i <= 5; i++) { c = cmp(a[i], $i ""); if (c != 0) { print c; exit } } print 0 }'
}

# ---- artifacts of a workflow run -------------------------------------------------------------------------------
# A file that goes from one job to another is a run artifact, which a compromised job can replace (it can read the token of the
# run). The hash of the file is a job output of the job that made it, which no other job can change; the job that uses the file
# gets the hash in the environment (MANIFEST_SHA256, SIGNED_SHA256) and checks it before it reads anything.
# job_output NAME VALUE: sets a job output when run by GitHub Actions, else nothing.
job_output() { [ -z "${GITHUB_OUTPUT:-}" ] || echo "$1=$2" >> "$GITHUB_OUTPUT"; }
# check_pinned FILE SHA256 WHAT: exit 2 unless SHA256 is a hash (an empty one is a job that pinned nothing) and FILE has it.
check_pinned() {
    is_hex "$2" 64 || die 2 "$3: the job that made it pinned no SHA-256 (it is empty or not a hash): nothing is trusted"
    [ "$(sha256_of "$1")" = "$(printf '%s' "$2" | tr 'A-F' 'a-f')" ] || die 2 "$3 is not the file the job that made it pinned"
}

# ---- small files ----------------------------------------------------------------------------------------------

# kv_get FILE KEY
kv_get() { sed -n "s/^$2=//p" "$1" | head -n1; }

sha256_of() { sha256sum "$1" | cut -d' ' -f1; }

# xml_first_release FILE: version and date of the first <release> of a metainfo, "version date type".
xml_first_release() {
    awk '/<release / && !done { v = $0; sub(/.*version="/, "", v); sub(/".*/, "", v); d = $0; sub(/.*date="/, "", d); sub(/".*/, "", d)
        t = ($0 ~ /type="development"/) ? "development" : "stable"; print v, d, t; done = 1 }' "$1"
}

# ---- git remote -----------------------------------------------------------------------------------------------

# Push (and fetch) with the token handed over by a credential helper that reads the environment: it is in no
# URL, no config file and no command line.
git_auth() {
    # shellcheck disable=SC2016
    git -c credential.helper= -c 'credential.helper=!f() { test "$1" = get && printf "username=x-access-token\npassword=%s\n" "$GH_TOKEN"; }; f' "$@"
}

# remote_head BRANCH: the commit of a branch of the remote, empty if it does not exist; exit 5 if the remote is unreachable.
remote_head() {
    out=$(git ls-remote "$REMOTE" "refs/heads/$1") || die 5 "cannot reach $REMOTE"
    printf '%s\n' "$out" | awk '{ print $1; exit }'
}

# remote_tag TAG: the commit a tag of the remote points to (peeled), empty if it does not exist.
remote_tag() {
    out=$(git ls-remote "$REMOTE" "refs/tags/$1" "refs/tags/$1^{}") || die 5 "cannot reach $REMOTE"
    peeled=$(printf '%s\n' "$out" | awk '$2 ~ /\^\{\}$/ { print $1 }')
    [ -n "$peeled" ] || peeled=$(printf '%s\n' "$out" | awk '{ print $1; exit }')
    printf '%s\n' "$peeled"
}

# pages_state DIR: the state of gh-pages without downloading its blobs but release.txt, in DIR: head, prev
# (SHAs, empty when the branch does not exist) and record (release.txt, or for the first versions, which have
# none, what the subject "Flatpak repository for vX.Y.Z" says; format=0).
pages_state() {
    rm -rf "$1"
    mkdir -p "$1"
    head=$(remote_head gh-pages)
    prev=$(remote_head gh-pages-previous)
    printf '%s\n' "$head" > "$1/head"
    printf '%s\n' "$prev" > "$1/prev"
    : > "$1/record"
    [ -n "$head" ] || return 0
    g=$1/git
    git init -q --bare "$g"
    git -C "$g" remote add origin "$REMOTE"
    git -C "$g" config remote.origin.promisor true
    git -C "$g" config remote.origin.partialclonefilter blob:none
    git -C "$g" fetch -q --depth 1 --filter=blob:none origin refs/heads/gh-pages || die 5 "cannot fetch gh-pages"
    head=$(git -C "$g" rev-parse FETCH_HEAD)
    printf '%s\n' "$head" > "$1/head"
    if git -C "$g" cat-file -e FETCH_HEAD:release.txt 2> /dev/null; then
        [ "$(git -C "$g" cat-file -t FETCH_HEAD:release.txt)" = blob ] || die 5 "release.txt of gh-pages is not a plain file"
        git -C "$g" show FETCH_HEAD:release.txt > "$1/record" || die 5 "cannot read release.txt of gh-pages"
    else
        subject=$(git -C "$g" log -1 --format=%s FETCH_HEAD)
        t=${subject#Flatpak repository for }
        if [ "$t" != "$subject" ] && semver_valid "$t"; then
            printf 'format=0\ntag=%s\nversion=%s\n' "$t" "${t#v}" > "$1/record"
        fi
    fi
    rm -rf "$g"
}

# ---- GitHub CLI (the only place gh is called) -----------------------------------------------------------------

# gh_releases: id, tag, draft, prerelease of every release (drafts included with a write token), tab separated.
gh_releases() {
    gh api --paginate "repos/$GH_REPO/releases?per_page=100" --jq '.[] | [.id, .tag_name, .draft, .prerelease] | @tsv'
}
# gh_wait_release KIND ARG: the list of releases is a few seconds behind a create, a delete or an edit, so every
# change is waited for in the list (RELEASE_POLL seconds apart, up to RELEASE_WAIT; 3 and 60) before it is used.
# KIND: listed (a release of tag ARG is listed), gone (the release with id ARG is not), published (the release of
# tag ARG is not a draft). Prints the matching rows of the last list; fails when the time is up.
gh_wait_release() {
    waited=0
    while :; do
        all=$(gh_releases) || die 5 "cannot list the releases of $GH_REPO"
        case $1 in
            listed) hit=$(printf '%s\n' "$all" | awk -F '\t' -v t="$2" '$2 == t') ;;
            gone) hit=$(printf '%s\n' "$all" | awk -F '\t' -v i="$2" '$1 == i') ;;
            published) hit=$(printf '%s\n' "$all" | awk -F '\t' -v t="$2" '$2 == t && $3 == "false"') ;;
            *) die 1 "gh_wait_release: unknown kind $1" ;;
        esac
        if [ "$1" = gone ]; then
            [ -z "$hit" ] && return 0
        elif [ -n "$hit" ]; then
            printf '%s\n' "$hit"
            return 0
        fi
        [ "$waited" -lt "${RELEASE_WAIT:-60}" ] || return 1
        sleep "${RELEASE_POLL:-3}"
        waited=$((waited + ${RELEASE_POLL:-3}))
    done
}
# gh_assets ID: asset id, name, size, state, digest, tab separated.
gh_assets() {
    gh api --paginate "repos/$GH_REPO/releases/$1/assets?per_page=100" --jq '.[] | [.id, .name, .size, .state, .digest] | @tsv'
}
# gh_create TAG PRERELEASE(0|1)
gh_create() {
    if [ "$2" = 1 ]; then
        gh release create "$1" --draft --verify-tag --generate-notes --title "$1" --prerelease
    else
        gh release create "$1" --draft --verify-tag --generate-notes --title "$1"
    fi
}
# gh_upload TAG FILE...
gh_upload() {
    t=$1
    shift
    gh release upload "$t" "$@" --clobber
}
gh_delete_asset() { gh api -X DELETE "repos/$GH_REPO/releases/assets/$1"; }
gh_delete_release() { gh api -X DELETE "repos/$GH_REPO/releases/$1"; }
# gh_publish TAG PRERELEASE(0|1)
gh_publish() {
    if [ "$2" = 1 ]; then
        gh release edit "$1" --draft=false --prerelease --latest=false
    else
        gh release edit "$1" --draft=false --latest
    fi
}

# ---- the decision: may this tag be released? --------------------------------------------------------------------

# release_state DIR: reads the remote and writes, in DIR: pages/ (pages_state), releases.tsv. Sets PG_HEAD,
# PG_PREV, PG_TAG, PG_VER, PG_COMMIT, PG_SUMS from the record.
release_state() {
    pages_state "$1/pages"
    gh_releases > "$1/releases.tsv" || die 5 "cannot list the releases of $GH_REPO"
    # shellcheck disable=SC2034
    PG_HEAD=$(cat "$1/pages/head")
    # shellcheck disable=SC2034
    PG_PREV=$(cat "$1/pages/prev")
    PG_TAG=$(kv_get "$1/pages/record" tag)
    PG_VER=$(kv_get "$1/pages/record" version)
    PG_COMMIT=$(kv_get "$1/pages/record" source_commit)
    # shellcheck disable=SC2034
    PG_SUMS=$(kv_get "$1/pages/record" sums_sha256)
    # A record that cannot be read must not silently lift the limits.
    if [ -n "$PG_HEAD" ] && [ -z "$PG_TAG" ]; then
        die 5 "gh-pages says no version: release.txt is missing or has no tag, and the subject is not 'Flatpak repository for vX.Y.Z'"
    fi
    if [ -n "$PG_TAG" ]; then
        semver_valid "$PG_TAG" || die 5 "gh-pages has an invalid tag in release.txt: $PG_TAG"
        [ "v$PG_VER" = "$PG_TAG" ] || die 5 "gh-pages release.txt: version $PG_VER does not match tag $PG_TAG"
    fi
}

# release_plan DIR TAG COMMIT MODE: after release_state, exits 3 or 5 on a refusal, else prints "publish" or
# "noop" (MODE release or publish-flatpak). The rules are in docs/release.md.
release_plan() {
    d=$1 t=$2 c=$3 mode=$4
    n=$(awk -F '\t' -v t="$t" '$2 == t' "$d/releases.tsv" | wc -l)
    [ "$n" -le 1 ] || die 5 "$n releases exist for $t"
    pub_id=$(awk -F '\t' -v t="$t" '$2 == t && $3 == "false" { print $1 }' "$d/releases.tsv")
    complete=0
    if [ -n "$pub_id" ]; then
        if assets_complete "$pub_id"; then complete=1; fi
    fi
    # A release published without the files is usually one made by hand on the web page; an immutable release
    # can never get them, and the tag cannot move: only the next version can be released.
    incomplete="the release $t is published but incomplete (made by hand? push only the tag: the workflow makes the release); this tag can no longer be released, cut the next version (docs/release.md)"
    if is_rc "$t"; then
        [ "$mode" = release ] || die 3 "a release candidate is not published to the Flatpak repository"
        others=$(awk -F '\t' -v t="$t" '$2 != t && $3 == "false" { print $2 }' "$d/releases.tsv")
        for x in $others $PG_TAG; do
            semver_valid "$x" || continue
            [ "$(semver_cmp "$t" "$x")" = 1 ] || die 3 "$t is not above the published $x"
        done
        if [ -n "$pub_id" ]; then
            [ "$complete" = 1 ] || die 3 "$incomplete"
            echo noop
        else
            echo publish
        fi
        return 0
    fi
    # Stable tags: not below any published stable release or the Pages version.
    highest=
    # shellcheck disable=SC2013
    for x in $(awk -F '\t' '$3 == "false" && $4 == "false" { print $2 }' "$d/releases.tsv"); do
        semver_valid "$x" || continue
        is_rc "$x" && continue
        if [ -z "$highest" ] || [ "$(semver_cmp "$x" "$highest")" = 1 ]; then highest=$x; fi
    done
    if [ -n "$highest" ] && [ "$(semver_cmp "$t" "$highest")" = -1 ]; then die 3 "$t is older than the published $highest"; fi
    pages_cmp=
    if [ -n "$PG_TAG" ]; then
        pages_cmp=$(semver_cmp "$t" "$PG_TAG")
        [ "$pages_cmp" != -1 ] || die 3 "$t is older than $PG_TAG, which gh-pages has"
        if [ "$pages_cmp" = 0 ] && [ "$PG_COMMIT" != "$c" ]; then
            die 3 "$t is already on gh-pages for the commit ${PG_COMMIT:-unknown}, not $c"
        fi
    fi
    if [ "$mode" = publish-flatpak ]; then
        [ -n "$pub_id" ] || die 3 "publish-flatpak needs the published release of $t"
        [ "$highest" = "$t" ] || die 3 "$t is not the highest published release (${highest:-none})"
        if [ "$pages_cmp" = 0 ]; then echo noop; else echo publish; fi
        return 0
    fi
    if [ -n "$pub_id" ]; then
        [ "$complete" = 1 ] || die 3 "$incomplete"
        [ "$pages_cmp" = 0 ] || die 3 "$t is published but not matched by gh-pages: use publish-flatpak"
        echo noop
    else
        echo publish
    fi
}

# assets_complete RELEASE_ID: the release has exactly SHA256SUMS and RELEASE_FILES, all uploaded.
assets_complete() {
    # shellcheck disable=SC2046
    want=$(printf '%s\n' SHA256SUMS $(conf RELEASE_FILES) | LC_ALL=C sort)
    have=$(gh_assets "$1" | awk -F '\t' '$4 == "uploaded" { print $2 }' | LC_ALL=C sort)
    [ "$want" = "$have" ]
}

# ---- archives and OSTree repositories ------------------------------------------------------------------------

# reproducible_tar PARENT NAME OUT EPOCH: OUT.tar.gz holding PARENT/NAME, the same bytes for the same contents
# whatever the file times, owners, umask or directory order of the machine: sorted names, every mtime EPOCH,
# owner 0:0, modes normalised (644/755, no group or other write), no gzip name or time. Needs GNU tar (--sort).
reproducible_tar() {
    LC_ALL=C tar --sort=name --format=gnu --mtime="@$4" --owner=0 --group=0 --numeric-owner --mode='u+rw,go+rX,go-w' -C "$1" -cf "$3.tmp" "$2" || return 1
    gzip -n -9 -c "$3.tmp" > "$3" || return 1
    rm -f "$3.tmp"
}

# tar_safe TAR PREFIX: exit 2 unless every member of the archive is a regular file or a
# directory inside PREFIX/ (no absolute path, no .., no link or device).
tar_safe() {
    ts=$(mktemp -d "${TMPDIR:-/tmp}/tar-safe.XXXXXX")
    tar -tf "$1" > "$ts/names" 2> /dev/null || { rm -rf "$ts"; die 2 "$1 is not a readable archive"; }
    tar -tvf "$1" 2> /dev/null | cut -c1 > "$ts/types"
    bad=$(grep -Ev "^$2(/|\$)" "$ts/names" | head -n1 || true)
    badpath=$(grep -E '(^|/)\.\.(/|$)|^/' "$ts/names" | head -n1 || true)
    badtype=$(grep -v '^[-d]$' "$ts/types" | head -n1 || true)
    lines=$(wc -l < "$ts/names")
    lines2=$(wc -l < "$ts/types")
    rm -rf "$ts"
    [ -z "$bad" ] || die 2 "$1: member outside $2/: $bad"
    [ -z "$badpath" ] || die 2 "$1: unsafe path: $badpath"
    [ -z "$badtype" ] || die 2 "$1: a member is not a regular file or a directory"
    [ "$lines" = "$lines2" ] || die 2 "$1: unreadable listing"
}

# extract_repo_tar TAR DIR: unpacks a repo/ archive after tar_safe.
extract_repo_tar() {
    tar_safe "$1" repo
    mkdir -p "$2"
    tar -C "$2" --no-same-owner --no-same-permissions -xf "$1" || die 2 "cannot unpack $1"
    [ -d "$2/repo/objects" ] || die 2 "$1 holds no OSTree repository"
}

# repo_refs REPO: the refs of an OSTree repository, one per line, sorted.
repo_refs() { ostree --repo="$1" refs | LC_ALL=C sort; }

# check_refs REPO: only the app, its Debug extension and the appstream refs, the first two always there.
check_refs() {
    refs=$(repo_refs "$1")
    printf '%s\n' "$refs" | grep -qx "$APP_REF" || die 2 "$1: ref $APP_REF is missing"
    printf '%s\n' "$refs" | grep -qx "$DEBUG_REF" || die 2 "$1: ref $DEBUG_REF is missing"
    extra=$(printf '%s\n' "$refs" | grep -vxF -e "$APP_REF" -e "$DEBUG_REF" -e appstream/x86_64 -e appstream2/x86_64 || true)
    [ -z "$extra" ] || die 2 "$1: unexpected refs: $extra"
}

# check_commit_content REPO VERSION: the app commit holds a metainfo with VERSION on top and metadata of the app.
check_commit_content() {
    top=$(ostree --repo="$1" cat "$APP_REF" "/files/share/metainfo/$APP_ID.metainfo.xml" 2> /dev/null | awk '/<release / && !d { v = $0; sub(/.*version="/, "", v); sub(/".*/, "", v); print v; d = 1 }') || true
    [ "$top" = "$2" ] || die 2 "$1: the metainfo of the app commit has ${top:-no release} on top, not $2"
    name=$(ostree --repo="$1" cat "$APP_REF" /metadata 2> /dev/null | sed -n 's/^name=//p' | head -n1) || true
    [ "$name" = "$APP_ID" ] || die 2 "$1: the app metadata names ${name:-nothing}, not $APP_ID"
    dname=$(ostree --repo="$1" cat "$DEBUG_REF" /metadata 2> /dev/null | sed -n 's/^name=//p' | head -n1) || true
    [ "$dname" = "$APP_ID.Debug" ] || die 2 "$1: the Debug metadata names ${dname:-nothing}"
    xa=$(ostree --repo="$1" show --print-metadata-key=xa.metadata "$APP_REF" 2> /dev/null | sed 's/\\n/\n/g' | sed -n 's/^.*name=//p' | head -n1) || true
    [ "$xa" = "$APP_ID" ] || die 2 "$1: xa.metadata names ${xa:-nothing}, not $APP_ID"
}

# check_unsigned_repo REPO VERSION: what the build job hands over: sound, only the expected refs, no signature.
check_unsigned_repo() {
    ostree --repo="$1" fsck > /dev/null 2>&1 || die 2 "$1: ostree fsck failed"
    check_refs "$1"
    for r in "$APP_REF" "$DEBUG_REF"; do
        s=$(ostree --repo="$1" show --print-detached-metadata-key=ostree.gpgsigs "$r" 2> /dev/null || true)
        [ -z "$s" ] || die 2 "$1: $r is already signed"
    done
    check_commit_content "$1" "$2"
}

# repo_config: the config of every repository of the release, to the byte: what "ostree init --mode=archive-z2" and flatpak
# build-update-repo write, and what the published repository has. The file is not signed, but every ostree and flatpak call
# reads it, and flatpak build-update-repo copies [flatpak] redirect-url, gpg-keys and title from it into the summary that
# the key signs: it is refused, never rewritten.
repo_config() { printf '[core]\nrepo_version=1\nmode=archive-z2\nindexed-deltas=true\n'; }

# check_repo_config REPO: succeeds if the config of REPO is a plain file with exactly the bytes of repo_config.
check_repo_config() { [ -f "$1/config" ] && [ ! -L "$1/config" ] && repo_config | cmp -s - "$1/config"; }

# The metadata keys of an honest run (flatpak 1.14.6, ostree 2024.5): exactly these on the new app and Debug commits of
# build-commit-from and on the appstream commits of build-update-repo; for the summary of build-update-repo the allowed
# ones (static deltas are listed only when there are some). New versions of flatpak add keys (xa.title, xa.redirect-url,
# xa.gpg-keys, ...): a fixed list fails closed.
# shellcheck disable=SC2034
APP_KEYS="ostree.collection-binding ostree.ref-binding xa.download-size xa.from_commit xa.installed-size xa.metadata xa.ref"
# shellcheck disable=SC2034
APPSTREAM_KEYS="ostree.collection-binding ostree.ref-binding"
# shellcheck disable=SC2034
SUMMARY_KEYS="ostree.summary.mode ostree.summary.tombstone-commits ostree.summary.indexed-deltas ostree.summary.last-modified ostree.static-deltas xa.cache-version xa.cache xa.sparse-cache"

# commit_keys REPO COMMIT, summary_keys REPO: the metadata keys, one per line (a key with a newline in it is listed as two
# lines, which no list below allows).
commit_keys() { ostree --repo="$1" show --list-metadata-keys "$2"; }
summary_keys() { ostree --repo="$1" summary --list-metadata-keys; }

# unknown_keys "KEY..." < LIST: the lines of LIST that are not one of the KEYs.
unknown_keys() { awk -v ok="$1" 'BEGIN { n = split(ok, a, " "); for (i = 1; i <= n; i++) known[a[i]] = 1 } !($0 in known)'; }

# keys_are "KEY..." < LIST: the lines of LIST, sorted, are exactly the KEYs (duplicates count).
keys_are() { [ "$(LC_ALL=C sort)" = "$(printf '%s\n' "$1" | tr ' ' '\n' | LC_ALL=C sort)" ]; }

# xa_sum REPO REF: SHA-256 of the printed xa.ref, xa.metadata and sizes of the head commit of a ref. flatpak build-commit-from
# keeps these four as the build made them (it adds xa.from_commit, the commit of the build), so the sum of the new commit is
# the one of the unsigned build.
xa_sum() {
    for xs_k in xa.ref xa.metadata xa.download-size xa.installed-size; do
        printf '%s=' "$xs_k"
        ostree --repo="$1" show --print-metadata-key="$xs_k" "$2" 2> /dev/null || echo none
    done | sha256sum | cut -d' ' -f1
}

# commit_time REPO COMMIT: the date of a commit, in seconds since 1970; fails if there is none.
commit_time() {
    ct=$(ostree --repo="$1" show "$2" | sed -n 's/^Date: *//p' | head -n1)
    [ -n "$ct" ] && date -u -d "$ct" +%s
}

# chain_ok REPO COMMIT BASE: COMMIT is BASE (published already) or its parent is BASE (BASE empty: it has no parent).
chain_ok() {
    [ -z "$3" ] || [ "$2" != "$3" ] || return 0
    cp_par=$(ostree --repo="$1" rev-parse "$2^" 2> /dev/null) || cp_par=""
    [ "$cp_par" = "$3" ]
}

# content_sum REPO REF: the content checksum of the head commit of a ref (the same for a commit and its signed copy).
content_sum() { ostree --repo="$1" log "$2" | sed -n 's/^ContentChecksum: *//p' | head -n1; }

# summary_refs REPO: the refs listed in the summary, sorted.
summary_refs() { ostree --repo="$1" summary --view 2> /dev/null | sed -n 's/^\* //p' | LC_ALL=C sort; }

# verify_signed_repo REPO PUBKEY SCRATCH VERSION: pulls the repository like a client that has only the public key
# (commits and summary must be signed by it) and checks refs and content. Prints nothing; exits 2 on a failure.
verify_signed_repo() {
    rm -rf "$3/chk"
    mkdir -p "$3"
    ostree --repo="$3/chk" init --mode=archive || die 2 "cannot create a scratch repository"
    ostree --repo="$3/chk" remote add --set=gpg-verify=true --set=gpg-verify-summary=true --gpg-import="$2" chk "file://$1" ||
        die 2 "cannot import the public key"
    check_refs "$1"
    want=$(repo_refs "$1")
    [ "$(summary_refs "$1")" = "$want" ] || die 2 "$1: the summary lists other refs than the repository"
    # shellcheck disable=SC2086
    ostree --repo="$3/chk" pull --untrusted chk $want > "$3/pull.log" 2>&1 || {
        sed 's/^/    /' "$3/pull.log" >&2
        die 2 "$1: the signed repository does not verify with the public key"
    }
    ostree --repo="$3/chk" fsck > /dev/null 2>&1 || die 2 "$1: ostree fsck failed after the pull"
    for r in $want; do
        ostree --repo="$3/chk" rev-parse "$r" > /dev/null || die 2 "$1: $r was not pulled"
        [ "$(ostree --repo="$3/chk" rev-parse "$r")" = "$(ostree --repo="$1" rev-parse "$r")" ] || die 2 "$1: $r differs after the pull"
        # The pull (depth 0) fetched and verified the head commit of the ref only: check that the head has a signature.
        s=$(ostree --repo="$1" show --print-detached-metadata-key=ostree.gpgsigs "$r" 2> /dev/null || true)
        [ -n "$s" ] || die 2 "$1: $r has no signature"
    done
    check_commit_content "$3/chk" "$4"
}

# pubkey_fpr FILE: the fingerprint of the key in a public key file, upper case; empty unless the file holds
# exactly one public primary key and nothing secret (a second key in a file meant to hold one is an error for
# the caller, which compares the result with the pinned fingerprint).
pubkey_fpr() {
    h=$(mktemp -d "${TMPDIR:-/tmp}/pubfpr.XXXXXX")
    chmod 700 "$h"
    f=$(gpg --homedir "$h" --batch --quiet --show-keys --with-colons "$1" 2> /dev/null |
        awk -F: '/^pub:/ { n++; w = 1; next } /^(sec|ssb):/ { bad = 1 } /^fpr:/ && w { f = $10; w = 0 } END { if (n == 1 && !bad && f != "") print f }') || true
    gpgconf --homedir "$h" --kill all > /dev/null 2>&1 || true
    rm -rf "$h"
    printf '%s\n' "$f" | tr 'a-f' 'A-F'
}

# repo_key_file REPOFILE OUT: writes the key of a .flatpakrepo (GPGKey, base64) to OUT.
repo_key_file() {
    k=$(sed -n 's/^GPGKey=//p' "$1" | head -n1)
    [ -n "$k" ] || die 2 "no GPGKey in $1"
    printf '%s' "$k" | base64 -d > "$2" 2> /dev/null || die 2 "the key in $1 is not valid base64"
}
