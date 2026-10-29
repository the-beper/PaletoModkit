#!/bin/sh
# Signs the Flatpak repository. The only step that sees the signing key: no build, no checkout token, no
# container. The key comes in FLATPAK_GPG_PRIVATE_KEY (an environment variable: never an argument, never a
# file outside a private temporary keyring that is deleted on exit).
#   flatpak-sign.sh --tag vX.Y.Z --commit SHA --manifest release-manifest.txt --unsigned flatpak-unsigned-repo.tar \
#                   (--site DIR [--bootstrap] | --fresh) --out DIR
#       --site DIR  the published repository (packaging/release-pages.sh fetch DIR): the new commit is added to
#                   it, so every version has the previous one as its parent and clients get deltas; it must
#                   verify with the public key, and the tag must be above its version (the same commit again:
#                   nothing is signed, flatpak-signed.txt says skip=1)
#       --bootstrap the first publication, DIR is empty (with --site)
#       --fresh     a repository of its own (release candidates, tests): no history, no Pages
#       Writes to DIR: flatpak-signed-repo.tar, PaletoModkit-x86_64.flatpak, flatpak-pubkey.gpg, flatpak-signed.txt.
#   flatpak-sign.sh --cleanup   stops gpg agents and removes the temporary keyrings and work folders (run it
#                               in an "if: always()" step)
# The key must be the one of FLATPAK_KEY_FPR in release.conf and, with --site, the published one: no rotation.
# Temporary files go to $RUNNER_TEMP (else $TMPDIR, else /tmp). Exit codes: see release-lib.sh (4: signing).
set -eu
# shellcheck source=/dev/null
. "$(dirname "$0")/release-lib.sh"
umask 077
{ set +x; } 2> /dev/null
TMPBASE=${RUNNER_TEMP:-${TMPDIR:-/tmp}}

if [ "${1:-}" = --cleanup ]; then
    for d in "$TMPBASE"/gnupg.*; do
        [ -d "$d" ] || continue
        gpgconf --homedir "$d" --kill all > /dev/null 2>&1 || true
        rm -rf "$d"
    done
    for d in "$TMPBASE"/flatpak-sign.*; do
        [ -d "$d" ] && rm -rf "$d"
    done
    exit 0
fi

TAG="" COMMIT="" MANIFEST="" UNSIGNED="" SITE="" OUT="" BOOTSTRAP=0 FRESH=0
while [ $# -gt 0 ]; do
    case $1 in
        --tag) TAG=${2:?}; shift 2 ;;
        --commit) COMMIT=${2:?}; shift 2 ;;
        --manifest) MANIFEST=${2:?}; shift 2 ;;
        --unsigned) UNSIGNED=${2:?}; shift 2 ;;
        --site) SITE=${2:?}; shift 2 ;;
        --bootstrap) BOOTSTRAP=1; shift ;;
        --fresh) FRESH=1; shift ;;
        --out) OUT=${2:?}; shift 2 ;;
        *) die 1 "unknown argument $1" ;;
    esac
done
[ -n "$TAG" ] && [ -n "$COMMIT" ] && [ -n "$MANIFEST" ] && [ -n "$UNSIGNED" ] && [ -n "$OUT" ] || die 1 "usage: see the header of $0"
[ -n "$SITE" ] || [ "$FRESH" = 1 ] || die 1 "give --site DIR or --fresh"
[ -z "$SITE" ] || [ "$FRESH" = 0 ] || die 1 "--site and --fresh exclude each other"
[ "$BOOTSTRAP" = 0 ] || [ -n "$SITE" ] || die 1 "--bootstrap goes with --site"
release_init
semver_valid "$TAG" || die 2 "$TAG is not a release tag"
VERSION=${TAG#v}
if is_rc "$TAG" && [ -n "$SITE" ]; then die 3 "a release candidate never goes into the published repository: use --fresh"; fi
FPR=$(conf_hex FLATPAK_KEY_FPR 40 | tr 'a-f' 'A-F')
for c in flatpak ostree gpg; do command -v "$c" > /dev/null || die 1 "$c is needed"; done

W=$(mktemp -d "$TMPBASE/flatpak-sign.XXXXXX")
KEYHOME=""
cleanup() {
    if [ -n "$KEYHOME" ] && [ -d "$KEYHOME" ]; then
        gpgconf --homedir "$KEYHOME" --kill all > /dev/null 2>&1 || true
        rm -rf "$KEYHOME"
    fi
    rm -rf "$W"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
unset GNUPGHOME

# ---- what to sign: checked against what the verify job recorded -------------------------------------------------
[ -f "$MANIFEST" ] && [ -f "$UNSIGNED" ] || die 2 "the manifest or the unsigned repository is missing"
[ "$(kv_get "$MANIFEST" tag)" = "$TAG" ] && [ "$(kv_get "$MANIFEST" commit)" = "$COMMIT" ] || die 2 "the manifest is not for $TAG at $COMMIT"
UNSIGNED_SHA=$(sha256_of "$UNSIGNED")
[ "$UNSIGNED_SHA" = "$(kv_get "$MANIFEST" flatpak_unsigned_tar_sha256)" ] || die 2 "the unsigned repository is not the verified one"
extract_repo_tar "$UNSIGNED" "$W/u"
check_unsigned_repo "$W/u/repo" "$VERSION"

# ---- the published state --------------------------------------------------------------------------------------
BASE_PAGES="" BASE_PREV="" BASE_APP="" BASE_DEBUG="" MODE=fresh
if [ -n "$SITE" ]; then
    SITE=${SITE%/}
    [ -d "$SITE" ] && [ -f "$SITE.base" ] || die 2 "$SITE is not the output of release-pages.sh fetch"
    BASE_PAGES=$(kv_get "$SITE.base" gh_pages)
    BASE_PREV=$(kv_get "$SITE.base" gh_pages_previous)
    if [ -z "$BASE_PAGES" ]; then
        [ "$BOOTSTRAP" = 1 ] || die 5 "gh-pages does not exist: --bootstrap is for the very first publication"
        MODE=bootstrap
    else
        [ "$BOOTSTRAP" = 0 ] || die 5 "gh-pages exists: --bootstrap is only for the very first publication"
        MODE=site
        if [ -f "$SITE/release.txt" ]; then
            PG_TAG=$(kv_get "$SITE/release.txt" tag)
            PG_COMMIT=$(kv_get "$SITE/release.txt" source_commit)
        else
            subject=$(kv_get "$SITE.base" subject)
            PG_TAG=${subject#Flatpak repository for }
            PG_COMMIT=""
        fi
        semver_valid "$PG_TAG" || die 5 "cannot tell which version gh-pages has (tag '$PG_TAG')"
        [ -d "$SITE/repo/objects" ] || die 2 "$SITE has no OSTree repository"
        c=$(semver_cmp "$TAG" "$PG_TAG")
        if [ "$c" = -1 ]; then die 3 "$TAG is older than $PG_TAG, which is published"; fi
        if [ "$c" = 0 ]; then
            [ "$PG_COMMIT" = "$COMMIT" ] || die 3 "$TAG is published for the commit ${PG_COMMIT:-unknown}, not $COMMIT"
            mkdir -p "$OUT"
            {
                echo "format=1"
                echo "tag=$TAG"
                echo "version=$VERSION"
                echo "commit=$COMMIT"
                echo "skip=1"
            } > "$OUT/flatpak-signed.txt"
            echo "$TAG is already published for this commit: nothing to sign"
            exit 0
        fi
        repo_key_file "$SITE/paleto-modkit.flatpakrepo" "$W/published.gpg"
        [ "$(pubkey_fpr "$W/published.gpg")" = "$FPR" ] || die 3 "the published key is not $FPR: a new key is not a release, see docs/release.md"
        # The published repository must verify like a client does it, and be the one the record says.
        verify_signed_repo "$SITE/repo" "$W/published.gpg" "$W/pg" "${PG_TAG#v}"
        BASE_APP=$(ostree --repo="$SITE/repo" rev-parse "$APP_REF")
        BASE_DEBUG=$(ostree --repo="$SITE/repo" rev-parse "$DEBUG_REF")
        if [ -f "$SITE/release.txt" ]; then
            [ "$BASE_APP" = "$(kv_get "$SITE/release.txt" ostree_app_commit)" ] || die 5 "the published app commit is not the one release.txt records"
            [ "$BASE_DEBUG" = "$(kv_get "$SITE/release.txt" ostree_debug_commit)" ] || die 5 "the published Debug commit is not the one release.txt records"
        fi
    fi
fi

# ---- the key --------------------------------------------------------------------------------------------------
[ -n "${FLATPAK_GPG_PRIVATE_KEY:-}" ] || die 4 "the secret FLATPAK_GPG_PRIVATE_KEY is empty or not set"
KEYHOME=$(mktemp -d "$TMPBASE/gnupg.XXXXXX")
chmod 700 "$KEYHOME"
export GNUPGHOME="$KEYHOME"
printf '%s\n' "$FLATPAK_GPG_PRIVATE_KEY" | gpg --batch --quiet --import > "$W/import.log" 2>&1 || {
    unset FLATPAK_GPG_PRIVATE_KEY
    die 4 "the signing key cannot be imported"
}
unset FLATPAK_GPG_PRIVATE_KEY
secret=$(gpg --batch --list-secret-keys --with-colons 2> /dev/null | awk -F: '/^sec/ { s = 1; n++; next } s && /^fpr/ { print $10; s = 0 } END { if (n != 1) print "count:" n + 0 }')
[ "$secret" = "$FPR" ] || die 4 "the signing key is $secret, not $FPR"
gpg --batch --export "$FPR" > "$W/pub.gpg" 2> /dev/null
[ -s "$W/pub.gpg" ] || die 4 "no public key for $FPR"
# With the published repository the key in its .flatpakrepo is the one users have: use that file's bytes.
if [ "$MODE" = site ]; then cp "$W/published.gpg" "$W/pub.gpg"; fi

# ---- sign -----------------------------------------------------------------------------------------------------
if [ "$MODE" = site ]; then
    cp -a "$SITE/repo" "$W/repo"
else
    ostree --repo="$W/repo" init --mode=archive-z2 || die 4 "ostree init failed"
fi
flatpak build-commit-from --untrusted --src-repo="$W/u/repo" --gpg-sign="$FPR" --gpg-homedir="$GNUPGHOME" --no-update-summary \
    --subject="Paleto Modkit $VERSION ($TAG)" --body="source $COMMIT" "$W/repo" "$APP_REF" "$DEBUG_REF" > "$W/commit.log" 2>&1 || {
    sed 's/^/    /' "$W/commit.log" >&2
    die 4 "flatpak build-commit-from failed"
}
flatpak build-update-repo --gpg-sign="$FPR" --gpg-homedir="$GNUPGHOME" --generate-static-deltas --prune --prune-depth=3 "$W/repo" > "$W/update.log" 2>&1 || {
    sed 's/^/    /' "$W/update.log" >&2
    die 4 "flatpak build-update-repo failed"
}
rm -rf "$W/repo/tmp"
mkdir "$W/repo/tmp"

# ---- verify what was made, with the public key only -----------------------------------------------------------
(verify_signed_repo "$W/repo" "$W/pub.gpg" "$W/new" "$VERSION") || die 4 "the signed repository does not verify"
APP_COMMIT=$(ostree --repo="$W/repo" rev-parse "$APP_REF")
DEBUG_COMMIT=$(ostree --repo="$W/repo" rev-parse "$DEBUG_REF")
if [ -n "$BASE_APP" ]; then
    [ "$(ostree --repo="$W/repo" rev-parse "$APP_REF^")" = "$BASE_APP" ] || die 4 "the new app commit does not continue the published one"
    [ "$(ostree --repo="$W/repo" rev-parse "$DEBUG_REF^")" = "$BASE_DEBUG" ] || die 4 "the new Debug commit does not continue the published one"
fi
[ "$(content_sum "$W/repo" "$APP_REF")" = "$(kv_get "$MANIFEST" flatpak_app_content)" ] || die 4 "the signed app content is not the verified build"

# ---- results --------------------------------------------------------------------------------------------------
mkdir -p "$OUT"
flatpak build-bundle --gpg-keys="$W/pub.gpg" "$W/repo" "$OUT/PaletoModkit-x86_64.flatpak" "$APP_ID" "$BRANCH" \
    --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo > "$W/bundle.log" 2>&1 || {
    sed 's/^/    /' "$W/bundle.log" >&2
    die 4 "flatpak build-bundle failed"
}
tar -C "$W" --owner=0 --group=0 --numeric-owner -cf "$OUT/flatpak-signed-repo.tar" repo
cp "$W/pub.gpg" "$OUT/flatpak-pubkey.gpg"
{
    echo "format=1"
    echo "tag=$TAG"
    echo "version=$VERSION"
    echo "commit=$COMMIT"
    echo "mode=$MODE"
    echo "skip=0"
    echo "base_gh_pages=$BASE_PAGES"
    echo "base_gh_pages_previous=$BASE_PREV"
    echo "base_app_commit=$BASE_APP"
    echo "base_debug_commit=$BASE_DEBUG"
    echo "app_commit=$APP_COMMIT"
    echo "debug_commit=$DEBUG_COMMIT"
    echo "summary_sha256=$(sha256_of "$W/repo/summary")"
    echo "bundle_sha256=$(sha256_of "$OUT/PaletoModkit-x86_64.flatpak")"
    echo "bundle_size=$(stat -c %s "$OUT/PaletoModkit-x86_64.flatpak")"
    echo "repo_tar_sha256=$(sha256_of "$OUT/flatpak-signed-repo.tar")"
    echo "unsigned_tar_sha256=$UNSIGNED_SHA"
    echo "key_fpr=$FPR"
} > "$OUT/flatpak-signed.txt"
echo "signed $TAG: app commit $APP_COMMIT"
