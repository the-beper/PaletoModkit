#!/bin/sh
# Publishes a verified, signed release: a draft GitHub release with the files, then the Flatpak repository on
# gh-pages, then the release itself. Safe to run again after any failure: it reads the state of the remote first
# and continues, replaces or refuses (the table is in docs/release.md).
#   release-publish.sh --mode release|publish-flatpak --tag vX.Y.Z --commit SHA --in DIR [--bootstrap]
#       DIR: the artifacts of the earlier jobs in one folder (see release-verify.sh final)
#       --bootstrap  gh-pages does not exist yet and this creates it
#       release          the release (a prerelease for -rc.N: nothing on gh-pages) and, for a stable tag, Pages
#       publish-flatpak  only Pages, for a release that is already published and is the highest
# Environment: GH_TOKEN, GH_REPO (the gh CLI; the token is also handed to git by a credential helper that reads
# it, never in a URL), REMOTE, RELEASE_WAIT (seconds to wait for the list of releases to show a change, default 60)
# and RELEASE_POLL (3), PAGES_WAIT (seconds to wait for the site, default 600) and PAGES_POLL (10), GITHUB_RUN_ID
# and GITHUB_RUN_ATTEMPT (recorded), MANIFEST_SHA256 and SIGNED_SHA256 (the hashes that the jobs verify and finish-flatpak pinned
# as job outputs: both are needed, exit 2 otherwise). Exit codes: see release-lib.sh (5: the remote is not as it was when signing).
set -eu
{ set +x; } 2> /dev/null
# shellcheck source=/dev/null
. "$(dirname "$0")/release-lib.sh"

TAG="" COMMIT="" MODE="" IN="" BOOTSTRAP=0
while [ $# -gt 0 ]; do
    case $1 in
        --tag) TAG=${2:?}; shift 2 ;;
        --commit) COMMIT=${2:?}; shift 2 ;;
        --mode) MODE=${2:?}; shift 2 ;;
        --in) IN=${2:?}; shift 2 ;;
        --bootstrap) BOOTSTRAP=1; shift ;;
        *) die 1 "unknown argument $1" ;;
    esac
done
[ -n "$TAG" ] && [ -n "$COMMIT" ] && [ -n "$MODE" ] && [ -n "$IN" ] || die 1 "usage: see the header of $0"
case $MODE in release | publish-flatpak) ;; *) die 1 "unknown mode $MODE" ;; esac
: "${GH_TOKEN:?set GH_TOKEN}"
release_init
semver_valid "$TAG" || die 2 "$TAG is not a release tag"
is_hex "$COMMIT" 40 || die 2 "$COMMIT is not a commit"
VERSION=${TAG#v}
PRERELEASE=0
if is_rc "$TAG"; then PRERELEASE=1; fi
[ "$PRERELEASE" = 0 ] || [ "$MODE" = release ] || die 2 "a release candidate is never published to the Flatpak repository"
SITE_URL=$(conf SITE_URL)
PAGES_WAIT=${PAGES_WAIT:-600}
PAGES_POLL=${PAGES_POLL:-10}
S=$IN/flatpak-signed.txt
M=$IN/release-manifest.txt
[ -f "$S" ] && [ -f "$M" ] || die 2 "flatpak-signed.txt or release-manifest.txt is missing in $IN"
# Both are run artifacts: they are the ones the jobs that made them pinned (job outputs), before anything is read from them.
# flatpak-signed.txt holds the hashes of the signed repository and of the bundle, so the pin covers those files too.
check_pinned "$M" "${MANIFEST_SHA256:-}" release-manifest.txt
check_pinned "$S" "${SIGNED_SHA256:-}" flatpak-signed.txt
TMP=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/release-publish.XXXXXX")
trap 'rm -rf "$TMP"' EXIT
trap 'exit 143' TERM
trap 'exit 130' INT

# ---- 1. the remote as it is now ---------------------------------------------------------------------------------
now=$(remote_tag "$TAG")
[ "$now" = "$COMMIT" ] || die 5 "the tag $TAG is ${now:-gone} on the remote, this run built $COMMIT"
release_state "$TMP/state"

# The signing job found $TAG already on gh-pages for this commit and signed nothing. The record of a skip is made by jobs that
# read untrusted data: gh-pages, read from the remote now, must say it.
if [ "$(kv_get "$S" skip)" = 1 ]; then
    [ "$PG_TAG" = "$TAG" ] && [ "$PG_COMMIT" = "$COMMIT" ] || die 5 "the signing job skipped $TAG, but gh-pages has ${PG_TAG:-no version} at ${PG_COMMIT:-an unknown commit}, not $TAG at $COMMIT: nothing was published"
    [ "$MODE" = release ] || { echo "$TAG is already on gh-pages: nothing to do"; exit 0; }
    pub_id=$(awk -F '\t' -v t="$TAG" '$2 == t && $3 == "false" { print $1 }' "$TMP/state/releases.tsv")
    [ -n "$pub_id" ] || die 5 "$TAG is on gh-pages but the release is not published: Pages is ahead of the release (docs/release.md)"
    list=$(gh_assets "$pub_id")
    for f in "PaletoModkit-x86_64.tar.gz:tarball_sha256" "PaletoModkit-x86_64.AppImage:appimage_sha256" "PaletoModkit-x86_64.AppImage.zsync:zsync_sha256"; do
        got=$(printf '%s\n' "$list" | awk -F '\t' -v n="${f%%:*}" '$2 == n { print $5 }')
        [ "$got" = "sha256:$(kv_get "$M" "${f##*:}")" ] || die 3 "$TAG is published with other files than this run built"
    done
    echo "$TAG is already published and on gh-pages: nothing to do"
    exit 0
fi

# ---- 2. what was signed ---------------------------------------------------------------------------------------
if [ "$MODE" = release ]; then
    [ "$(kv_get "$M" linux)" = 1 ] || die 2 "the manifest has no Linux files: not a release run"
else
    [ "$(kv_get "$M" linux)" = 0 ] || die 2 "publish-flatpak builds no Linux files"
fi
signed_mode=$(kv_get "$S" mode)
if [ "$PRERELEASE" = 1 ]; then
    [ "$signed_mode" = fresh ] || die 2 "a release candidate must be signed with --fresh"
else
    [ "$signed_mode" = site ] || [ "$signed_mode" = bootstrap ] || die 2 "the signed repository of $TAG is not made for gh-pages ($signed_mode)"
fi
"$PKG_DIR/release-verify.sh" final --tag "$TAG" --commit "$COMMIT" --in "$IN" --out "$TMP/final"

# ---- 3. may it go on? -------------------------------------------------------------------------------------------
if [ "$PRERELEASE" = 0 ]; then
    if [ -z "$PG_HEAD" ]; then
        [ "$BOOTSTRAP" = 1 ] || die 5 "gh-pages does not exist: --bootstrap creates it, once"
    else
        [ "$BOOTSTRAP" = 0 ] || die 5 "gh-pages exists: --bootstrap is only for the very first publication"
    fi
fi
PLAN=$(release_plan "$TMP/state" "$TAG" "$COMMIT" "$MODE")

# assets_match ID DIR: the release has exactly the files of DIR, uploaded, with their size and SHA-256.
assets_match() {
    list=$(gh_assets "$1")
    have=$(printf '%s\n' "$list" | awk -F '\t' '$4 == "uploaded" { print $2 }' | LC_ALL=C sort)
    want=$(cd "$2" && find . -mindepth 1 -maxdepth 1 -type f | sed 's|^\./||' | LC_ALL=C sort)
    [ "$have" = "$want" ] || return 1
    for n in $want; do
        row=$(printf '%s\n' "$list" | awk -F '\t' -v n="$n" '$2 == n')
        [ "$(printf '%s\n' "$row" | cut -f3)" = "$(stat -c %s "$2/$n")" ] || return 1
        [ "$(printf '%s\n' "$row" | cut -f5)" = "sha256:$(sha256_of "$2/$n")" ] || return 1
    done
}

if [ "$MODE" = release ]; then
    OURS=$(sha256_of "$TMP/final/SHA256SUMS")
else
    pub_id=$(awk -F '\t' -v t="$TAG" '$2 == t && $3 == "false" { print $1 }' "$TMP/state/releases.tsv")
    OURS=$(gh_assets "$pub_id" | awk -F '\t' '$2 == "SHA256SUMS" { print $5 }')
    OURS=${OURS#sha256:}
    is_hex "$OURS" 64 || die 5 "the release $TAG has no SHA256SUMS to record"
fi
if [ "$PLAN" = noop ]; then
    if [ "$MODE" = release ]; then
        pub_id=$(awk -F '\t' -v t="$TAG" '$2 == t && $3 == "false" { print $1 }' "$TMP/state/releases.tsv")
        assets_match "$pub_id" "$TMP/final" || die 3 "$TAG is published with other files than this run built"
        [ "$PRERELEASE" = 1 ] || [ "$PG_SUMS" = "$OURS" ] || die 3 "$TAG is published and gh-pages has another build of it"
    fi
    echo "$TAG is already published: nothing to do"
    exit 0
fi

PAGES_NEED=0
if [ "$PRERELEASE" = 0 ]; then
    if [ -z "$PG_TAG" ] || [ "$(semver_cmp "$TAG" "$PG_TAG")" = 1 ]; then
        PAGES_NEED=1
        [ "$PG_HEAD" = "$(kv_get "$S" base_gh_pages)" ] || die 5 "gh-pages changed since the repository was signed (now ${PG_HEAD:-gone}): run all jobs again"
    else
        # gh-pages is at this tag already: it must be this build.
        [ "$PG_SUMS" = "$OURS" ] || die 5 "gh-pages has another build of $TAG: Pages is ahead of the release (docs/release.md)"
    fi
fi

# ---- 4. the draft release ---------------------------------------------------------------------------------------
if [ "$MODE" = release ]; then
    rel=$(awk -F '\t' -v t="$TAG" '$2 == t { print $1 }' "$TMP/state/releases.tsv")
    if [ -n "$rel" ]; then
        sums=$(gh_assets "$rel" | awk -F '\t' '$2 == "SHA256SUMS" && $4 == "uploaded" { print $5 }')
        if [ "$sums" != "sha256:$OURS" ]; then
            echo "the draft of $TAG is another build: replacing it"
            gh_delete_release "$rel" > /dev/null || die 1 "cannot delete the draft"
            gh_wait_release gone "$rel" || die 5 "the old draft of $TAG is still listed after ${RELEASE_WAIT:-60}s"
            rel=""
        fi
    fi
    if [ -z "$rel" ]; then
        # The notes are the list of the metainfo's <release> for this version (release-notes.py), not the merged pull requests.
        python3 -I "$PKG_DIR/release-notes.py" "$PKG_DIR/$APP_ID.metainfo.xml" "$VERSION" --repo "$GH_REPO" > "$TMP/notes.md" || die 2 "cannot write the release notes from the metainfo"
        gh_create "$TAG" "$PRERELEASE" "$TMP/notes.md" > /dev/null || die 1 "cannot create the draft release"
        gh_wait_release listed "$TAG" > "$TMP/releases2.tsv" || die 5 "the new draft of $TAG is not listed after ${RELEASE_WAIT:-60}s"
        [ "$(wc -l < "$TMP/releases2.tsv")" = 1 ] || die 5 "not exactly one release for $TAG after creating it"
        rel=$(cut -f1 "$TMP/releases2.tsv")
    fi
    # Drop what does not belong or is not complete, then upload what is missing (the sums first).
    gh_assets "$rel" > "$TMP/assets.tsv"
    while IFS="$(printf '\t')" read -r aid name size state digest; do
        [ -n "$aid" ] || continue
        if [ -f "$TMP/final/$name" ] && [ "$state" = uploaded ] && [ "$size" = "$(stat -c %s "$TMP/final/$name")" ] && [ "$digest" = "sha256:$(sha256_of "$TMP/final/$name")" ]; then
            continue
        fi
        gh_delete_asset "$aid" > /dev/null || die 1 "cannot delete the asset $name"
    done < "$TMP/assets.tsv"
    have=$(gh_assets "$rel" | awk -F '\t' '{ print $2 }')
    printf '%s\n' "$have" | grep -qx SHA256SUMS || gh_upload "$TAG" "$TMP/final/SHA256SUMS" || die 1 "cannot upload SHA256SUMS"
    missing=""
    for n in $(conf RELEASE_FILES); do
        printf '%s\n' "$have" | grep -qx "$n" || missing="$missing $TMP/final/$n"
    done
    # shellcheck disable=SC2086
    [ -z "$missing" ] || gh_upload "$TAG" $missing || die 1 "cannot upload the release files"
    assets_match "$rel" "$TMP/final" || die 5 "the draft of $TAG does not have the files of this run"
fi

# ---- 5. gh-pages ------------------------------------------------------------------------------------------------
push_pages() {
    g=$TMP/pg
    rm -rf "$g" "$TMP/old"
    mkdir -p "$TMP/old"
    if [ -n "$PG_HEAD" ]; then
        git clone -q --depth 1 --branch gh-pages "$REMOTE" "$g" || die 5 "cannot clone gh-pages"
        [ "$(git -C "$g" rev-parse HEAD)" = "$PG_HEAD" ] || die 5 "gh-pages moved while publishing"
        link=$(find "$g" -path "$g/.git" -prune -o -type l -print | head -n1)
        [ -z "$link" ] || die 2 "gh-pages holds a symbolic link ($link)"
        cp "$g/paleto-modkit.flatpakrepo" "$g/paleto-modkit.flatpakref" "$TMP/old/" || die 5 "gh-pages has no .flatpakrepo or .flatpakref"
        git -C "$g" checkout -q --orphan paleto-new
        git -C "$g" rm -rqf .
        git -C "$g" clean -fdxq
    else
        git init -q --initial-branch=paleto-new "$g"
    fi
    extract_repo_tar "$IN/flatpak-signed-repo.tar" "$TMP/ex"
    mv "$TMP/ex/repo" "$g/repo"
    rm -rf "$g/repo/tmp"
    # The key in the files users added the remote with is the PUBLISHED one, byte for byte (a fresh export of the same
    # key need not have the same bytes): exactly one key, the pinned one, or nothing is written. The first
    # publication has no published file and uses the key of the signing keyring (verified by release-verify.sh).
    keyfile=$IN/flatpak-pubkey.gpg
    if [ -n "$PG_HEAD" ]; then
        repo_key_file "$TMP/old/paleto-modkit.flatpakrepo" "$TMP/old/published.gpg"
        [ "$(pubkey_fpr "$TMP/old/published.gpg")" = "$(conf_hex FLATPAK_KEY_FPR 40 | tr 'a-f' 'A-F')" ] ||
            die 3 "the published key file does not hold exactly the pinned key"
        keyfile=$TMP/old/published.gpg
    fi
    "$PKG_DIR/publish-flatpak-repo.sh" "$g" "$SITE_URL" "$keyfile"
    # The files users added the remote with: the identity does not change.
    if [ -n "$PG_HEAD" ]; then
        for f in paleto-modkit.flatpakrepo paleto-modkit.flatpakref; do
            cmp -s "$TMP/old/$f" "$g/$f" || die 3 "the new $f differs from the published one: the Flatpak identity must not change"
        done
    fi
    {
        echo "format=1"
        echo "tag=$TAG"
        echo "version=$VERSION"
        echo "source_commit=$COMMIT"
        echo "ostree_app_commit=$(kv_get "$S" app_commit)"
        echo "ostree_debug_commit=$(kv_get "$S" debug_commit)"
        echo "sums_sha256=$OURS"
        echo "bundle_sha256=$(kv_get "$S" bundle_sha256)"
        echo "key_fpr=$(kv_get "$S" key_fpr)"
        echo "run=${GITHUB_RUN_ID:-0}.${GITHUB_RUN_ATTEMPT:-0}"
    } > "$g/release.txt"
    git -C "$g" add --all
    git -C "$g" -c user.name=github-actions -c user.email=github-actions@users.noreply.github.com commit -q -m "Flatpak repository for $TAG"
    NEW=$(git -C "$g" rev-parse HEAD)
    # One commit, both branches together, and only if nobody else wrote since we looked.
    if [ -n "$PG_HEAD" ]; then
        git_auth -C "$g" push -q --atomic --force-with-lease="refs/heads/gh-pages:$PG_HEAD" --force-with-lease="refs/heads/gh-pages-previous:$PG_PREV" \
            "$REMOTE" "$NEW:refs/heads/gh-pages" "$PG_HEAD:refs/heads/gh-pages-previous" || die 5 "the push to gh-pages was rejected: another run or writer changed it"
    else
        git_auth -C "$g" push -q --atomic --force-with-lease=refs/heads/gh-pages: "$REMOTE" "$NEW:refs/heads/gh-pages" ||
            die 5 "the push to gh-pages was rejected: another run or writer changed it"
    fi
    [ "$(remote_head gh-pages)" = "$NEW" ] || die 5 "gh-pages is not the commit just pushed"
    echo "pushed gh-pages $NEW"
}

# wait_pages: the site serves release.txt of this tag and build.
wait_pages() {
    waited=0
    while :; do
        if curl -fsS "$SITE_URL/release.txt" 2> /dev/null > "$TMP/served.txt" && [ "$(kv_get "$TMP/served.txt" tag)" = "$TAG" ] && [ "$(kv_get "$TMP/served.txt" sums_sha256)" = "$OURS" ]; then
            return 0
        fi
        [ "$waited" -lt "$PAGES_WAIT" ] || die 5 "$SITE_URL does not serve $TAG after ${PAGES_WAIT}s: the draft stays, run the publish job again"
        sleep "$PAGES_POLL"
        waited=$((waited + PAGES_POLL))
    done
}

if [ "$PRERELEASE" = 0 ]; then
    if [ "$PAGES_NEED" = 1 ]; then push_pages; fi
    wait_pages
fi

# ---- 6. the release ---------------------------------------------------------------------------------------------
if [ "$MODE" = release ]; then
    assets_match "$rel" "$TMP/final" || die 5 "the draft of $TAG changed while publishing"
    gh_publish "$TAG" "$PRERELEASE" || die 1 "cannot publish the release $TAG"
    gh_wait_release published "$TAG" > /dev/null || die 5 "$TAG is not published after the edit"
fi
echo "published $TAG"
