#!/bin/sh
# Signs the Flatpak repository, in steps that run in different jobs of the release workflow, so that the code that
# parses untrusted data (the build output, gh-pages) never runs where the key is. The key comes in
# FLATPAK_GPG_PRIVATE_KEY, in the environment of the --sign step only: never an argument, never a file outside a
# private temporary keyring that is deleted on exit.
#   flatpak-sign.sh --prepare --tag vX.Y.Z --commit SHA --manifest release-manifest.txt --unsigned flatpak-unsigned-repo.tar \
#                   (--site DIR [--bootstrap] | --fresh) --work DIR --out DIR      job prepare-flatpak, NO secret
#       Everything that reads untrusted data: the unsigned repository against the manifest and its structure, the
#       published repository (packaging/release-pages.sh fetch DIR) verified like a client does with the pinned
#       public key, the version rules, the base commits; then the new, UNSIGNED commits are added to a copy of
#       the published repository (parent chain) and its summary and appstream data are generated and old commits
#       pruned. Writes to --out: flatpak-prepared-repo.tar and flatpak-prepared.txt (what it is, the base, the
#       hash of the tar). --site: the tag must be above the published version (the same commit
#       again: nothing is signed, skip=1); --bootstrap: the first publication, DIR of --site is empty; --fresh: a
#       repository of its own (release candidates, tests). Refuses to run with the secret in the environment.
#   flatpak-sign.sh --check-prepared --tag vX.Y.Z --commit SHA --manifest release-manifest.txt --in DIR --work DIR
#                   [--check-remote]                      job sign-flatpak, first, NO secret (refuses to run with it)
#       DIR is the --out of --prepare, which is no proof of anything (another job made it). Checks the hash of the tar
#       against its description, that it is for this tag and commit, and (--check-remote) that gh-pages is still at
#       the base the preparation had; unpacks it; checks its refs; and compares the content checksums of the app and
#       Debug commits with release-manifest.txt, which the verify job made from the build output on its own. Writes
#       DIR of --work with the repository and the list of refs and commits that --sign may sign: the heads of the app,
#       Debug, appstream and appstream2 refs, as they are now.
#   flatpak-sign.sh --sign --tag vX.Y.Z --commit SHA --work DIR --out DIR     job sign-flatpak, the secret, and only it
#       DIR of --work is the one of --check-prepared. Imports the key into a private keyring in memory (/dev/shm), checks
#       that it is the pinned one, signs the commits of the list (ostree gpg-sign) and the summary and its index,
#       removes the keyring. Writes to --out flatpak-sign-result-repo.tar, flatpak-pubkey.gpg (exported from the
#       keyring), flatpak-prepared.txt, flatpak-sign-result.txt. Nothing is unpacked, made or generated here: no
#       commit, no appstream data, no static delta. The residual surface, with the key at hand: ostree reading the
#       commit objects to sign them and flatpak build-update-repo reading the commit metadata of the refs to write the
#       summary: no file of the app is read, parsed or interpreted.
#       NO STATIC DELTAS: a delta embeds the signature of its commit, so it can only be made after the commit is
#       signed, and the signed summary has to list it, so the summary would have to be signed again afterwards (tried:
#       deltas made after the signature, without the key, are not in the signed summary and clients never use them;
#       a regenerated summary has no signature and clients refuse it). Making them with the key present would read
#       every content object of the build with the key at hand, so there are none: an update downloads the changed
#       objects (the compressed files that differ) instead of a delta. To bring deltas back: a keyless job that
#       makes them from the signed repository, then a second signing step that only signs the summary and its index.
#   flatpak-sign.sh --finish --tag vX.Y.Z --commit SHA --manifest release-manifest.txt --in DIR --work DIR --out DIR
#                                                              job finish-flatpak, NO secret, another machine
#       DIR of --in is the --out of --sign. Verifies the signed repository with the public key only (the one exported
#       from the signing keyring), that it is the prepared one plus signatures, and that its content is the verified
#       build's; makes the bundle and writes to --out: flatpak-signed-repo.tar, PaletoModkit-x86_64.flatpak,
#       flatpak-pubkey.gpg, flatpak-signed.txt (with skip=1 only that file, when --prepare found the tag published).
#   flatpak-sign.sh --cleanup [--work DIR]   stops gpg agents and removes the temporary keyrings and the work
#                                            folder (run it in an "if: always()" step)
# The key must be the one of FLATPAK_KEY_FPR in release.conf and, with --site, the published one: no rotation.
# Temporary files go to $RUNNER_TEMP (else $TMPDIR, else /tmp). Exit codes: see release-lib.sh (4: signing).
set -eu
# shellcheck source=/dev/null
. "$(dirname "$0")/release-lib.sh"
umask 077
{ set +x; } 2> /dev/null
TMPBASE=${RUNNER_TEMP:-${TMPDIR:-/tmp}}

[ $# -ge 1 ] || die 1 "usage: see the header of $0"
CMD=$1
shift
TAG="" COMMIT="" MANIFEST="" UNSIGNED="" SITE="" OUT="" IN="" WORK="" BOOTSTRAP=0 FRESH=0 CHECK_REMOTE=0
while [ $# -gt 0 ]; do
    case $1 in
        --tag) TAG=${2:?}; shift 2 ;;
        --commit) COMMIT=${2:?}; shift 2 ;;
        --manifest) MANIFEST=${2:?}; shift 2 ;;
        --unsigned) UNSIGNED=${2:?}; shift 2 ;;
        --site) SITE=${2:?}; shift 2 ;;
        --bootstrap) BOOTSTRAP=1; shift ;;
        --fresh) FRESH=1; shift ;;
        --check-remote) CHECK_REMOTE=1; shift ;;
        --work) WORK=${2:?}; shift 2 ;;
        --in) IN=${2:?}; shift 2 ;;
        --out) OUT=${2:?}; shift 2 ;;
        *) die 1 "unknown argument $1" ;;
    esac
done
case $WORK in
    / | . | ..) die 1 "--work $WORK is not a work folder" ;;
esac

if [ "$CMD" = --cleanup ]; then
    for d in /dev/shm/paleto-gnupg.* "$TMPBASE"/gnupg.*; do
        [ -d "$d" ] || continue
        gpgconf --homedir "$d" --kill all > /dev/null 2>&1 || true
        rm -rf "$d"
    done
    [ -z "$WORK" ] || rm -rf "$WORK"
    exit 0
fi

[ -n "$WORK" ] || die 1 "--work is needed"
release_init
for c in flatpak ostree gpg; do command -v "$c" > /dev/null || die 1 "$c is needed"; done
FPR=$(conf_hex FLATPAK_KEY_FPR 40 | tr 'a-f' 'A-F')
unset GNUPGHOME

# ---- the secret is in the environment of --sign and of nothing else ---------------------------------------------
case $CMD in
--prepare | --check-prepared | --finish)
    [ -z "${FLATPAK_GPG_PRIVATE_KEY:-}" ] || die 1 "FLATPAK_GPG_PRIVATE_KEY is set: $CMD reads untrusted data and must run without the secret"
    ;;
esac

# ---- prepare ----------------------------------------------------------------------------------------------------
prepare() {
    [ -n "$TAG" ] && [ -n "$COMMIT" ] && [ -n "$MANIFEST" ] && [ -n "$UNSIGNED" ] && [ -n "$OUT" ] || die 1 "usage: see the header of $0"
    [ -n "$SITE" ] || [ "$FRESH" = 1 ] || die 1 "give --site DIR or --fresh"
    [ -z "$SITE" ] || [ "$FRESH" = 0 ] || die 1 "--site and --fresh exclude each other"
    [ "$BOOTSTRAP" = 0 ] || [ -n "$SITE" ] || die 1 "--bootstrap goes with --site"
    semver_valid "$TAG" || die 2 "$TAG is not a release tag"
    is_hex "$COMMIT" 40 || die 2 "$COMMIT is not a commit"
    VERSION=${TAG#v}
    if is_rc "$TAG" && [ -n "$SITE" ]; then die 3 "a release candidate never goes into the published repository: use --fresh"; fi
    rm -rf "$WORK" "$OUT"
    mkdir -p "$WORK" "$OUT"

    # What to sign: checked against what the verify job recorded.
    [ -f "$MANIFEST" ] && [ -f "$UNSIGNED" ] || die 2 "the manifest or the unsigned repository is missing"
    [ "$(kv_get "$MANIFEST" tag)" = "$TAG" ] && [ "$(kv_get "$MANIFEST" commit)" = "$COMMIT" ] || die 2 "the manifest is not for $TAG at $COMMIT"
    UNSIGNED_SHA=$(sha256_of "$UNSIGNED")
    [ "$UNSIGNED_SHA" = "$(kv_get "$MANIFEST" flatpak_unsigned_tar_sha256)" ] || die 2 "the unsigned repository is not the verified one"
    extract_repo_tar "$UNSIGNED" "$WORK/u"
    check_unsigned_repo "$WORK/u/repo" "$VERSION"

    # The published state.
    BASE_PAGES="" BASE_PREV="" BASE_APP="" BASE_DEBUG="" MODE=fresh SKIP=0
    if [ -n "$SITE" ]; then
        SITE=${SITE%/}
        [ -d "$SITE" ] && [ -f "$SITE.base" ] || die 2 "$SITE is not the output of release-pages.sh fetch"
        link=$(find "$SITE" -type l | head -n1)
        [ -z "$link" ] || die 2 "$SITE holds a symbolic link ($link)"
        BASE_PAGES=$(kv_get "$SITE.base" gh_pages)
        BASE_PREV=$(kv_get "$SITE.base" gh_pages_previous)
        if [ -z "$BASE_PAGES" ]; then
            [ "$BOOTSTRAP" = 1 ] || die 5 "gh-pages does not exist: --bootstrap is for the very first publication"
            MODE=bootstrap
        else
            [ "$BOOTSTRAP" = 0 ] || die 5 "gh-pages exists: --bootstrap is only for the very first publication"
            MODE=site
            if [ -L "$SITE/release.txt" ] || { [ -e "$SITE/release.txt" ] && [ ! -f "$SITE/release.txt" ]; }; then
                die 5 "release.txt of gh-pages is not a plain file"
            fi
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
                SKIP=1
            else
                # Exactly one key in the published file, the pinned one: no rotation, no second key.
                repo_key_file "$SITE/paleto-modkit.flatpakrepo" "$WORK/published.gpg"
                [ "$(pubkey_fpr "$WORK/published.gpg")" = "$FPR" ] || die 3 "the published key file does not hold exactly the key $FPR: a new key is not a release, see docs/release.md"
                # The published repository must verify like a client does it, and be the one the record says.
                verify_signed_repo "$SITE/repo" "$WORK/published.gpg" "$WORK/pg" "${PG_TAG#v}"
                BASE_APP=$(ostree --repo="$SITE/repo" rev-parse "$APP_REF")
                BASE_DEBUG=$(ostree --repo="$SITE/repo" rev-parse "$DEBUG_REF")
                if [ -f "$SITE/release.txt" ]; then
                    [ "$BASE_APP" = "$(kv_get "$SITE/release.txt" ostree_app_commit)" ] || die 5 "the published app commit is not the one release.txt records"
                    [ "$BASE_DEBUG" = "$(kv_get "$SITE/release.txt" ostree_debug_commit)" ] || die 5 "the published Debug commit is not the one release.txt records"
                fi
            fi
        fi
    fi

    # The new commits, unsigned, on top of the published repository, with summary and appstream data.
    PREP_SHA="" PREP_APP="" PREP_DEBUG=""
    if [ "$SKIP" = 0 ]; then
        if [ "$MODE" = site ]; then
            cp -a "$SITE/repo" "$WORK/repo"
        else
            ostree --repo="$WORK/repo" init --mode=archive-z2 || die 2 "ostree init failed"
        fi
        flatpak build-commit-from --untrusted --src-repo="$WORK/u/repo" --no-update-summary \
            --subject="Paleto Modkit $VERSION ($TAG)" --body="source $COMMIT" "$WORK/repo" "$APP_REF" "$DEBUG_REF" > "$WORK/commit.log" 2>&1 || {
            sed 's/^/    /' "$WORK/commit.log" >&2
            die 2 "flatpak build-commit-from failed"
        }
        flatpak build-update-repo --prune --prune-depth=3 "$WORK/repo" > "$WORK/update.log" 2>&1 || {
            sed 's/^/    /' "$WORK/update.log" >&2
            die 2 "flatpak build-update-repo failed"
        }
        rm -rf "$WORK/repo/tmp"
        mkdir "$WORK/repo/tmp"
        PREP_APP=$(ostree --repo="$WORK/repo" rev-parse "$APP_REF")
        PREP_DEBUG=$(ostree --repo="$WORK/repo" rev-parse "$DEBUG_REF")
        if [ -n "$BASE_APP" ]; then
            [ "$(ostree --repo="$WORK/repo" rev-parse "$APP_REF^")" = "$BASE_APP" ] || die 2 "the new app commit does not continue the published one"
            [ "$(ostree --repo="$WORK/repo" rev-parse "$DEBUG_REF^")" = "$BASE_DEBUG" ] || die 2 "the new Debug commit does not continue the published one"
        fi
        [ "$(content_sum "$WORK/repo" "$APP_REF")" = "$(kv_get "$MANIFEST" flatpak_app_content)" ] || die 2 "the prepared app content is not the verified build"
        tar -C "$WORK" --owner=0 --group=0 --numeric-owner -cf "$OUT/flatpak-prepared-repo.tar" repo
        PREP_SHA=$(sha256_of "$OUT/flatpak-prepared-repo.tar")
    fi
    {
        echo "format=1"
        echo "tag=$TAG"
        echo "version=$VERSION"
        echo "commit=$COMMIT"
        echo "mode=$MODE"
        echo "skip=$SKIP"
        echo "base_gh_pages=$BASE_PAGES"
        echo "base_gh_pages_previous=$BASE_PREV"
        echo "base_app_commit=$BASE_APP"
        echo "base_debug_commit=$BASE_DEBUG"
        echo "unsigned_tar_sha256=$UNSIGNED_SHA"
        echo "prepared_tar_sha256=$PREP_SHA"
        echo "prepared_app_commit=$PREP_APP"
        echo "prepared_debug_commit=$PREP_DEBUG"
    } > "$OUT/flatpak-prepared.txt"
    if [ "$SKIP" = 1 ]; then echo "$TAG is already published for this commit: nothing to sign"; else echo "prepared $TAG ($MODE)"; fi
}

# prepared_state FILE: reads the description of a prepared repository and checks it is for TAG and COMMIT
prepared_state() {
    P=$1
    [ -f "$P" ] || die 1 "$P is missing: run --prepare first"
    [ "$(kv_get "$P" format)" = 1 ] || die 2 "$P: unknown format"
    [ "$(kv_get "$P" tag)" = "$TAG" ] && [ "$(kv_get "$P" commit)" = "$COMMIT" ] || die 2 "$P is not for $TAG at $COMMIT"
    VERSION=$(kv_get "$P" version)
    [ "v$VERSION" = "$TAG" ] || die 2 "$P: version $VERSION is not $TAG"
    MODE=$(kv_get "$P" mode)
    SKIP=$(kv_get "$P" skip)
}

# ---- check-prepared: before the key exists, in the job that signs ----------------------------------------------------
# The prepared repository comes from another job: its description is no proof. What it holds is compared with the
# manifest of the verify job (which parsed the build output on its own), and the refs and commits to sign are named.
check_prepared() {
    [ -n "$TAG" ] && [ -n "$COMMIT" ] && [ -n "$IN" ] && [ -n "$MANIFEST" ] || die 1 "usage: see the header of $0"
    semver_valid "$TAG" || die 2 "$TAG is not a release tag"
    is_hex "$COMMIT" 40 || die 2 "$COMMIT is not a commit"
    prepared_state "$IN/flatpak-prepared.txt"
    rm -rf "$WORK"
    mkdir -p "$WORK"
    cp "$IN/flatpak-prepared.txt" "$WORK/prepared.txt"
    if [ "$SKIP" = 1 ]; then
        echo "nothing to sign"
        return 0
    fi
    [ -f "$MANIFEST" ] || die 2 "$MANIFEST is missing"
    [ "$(kv_get "$MANIFEST" tag)" = "$TAG" ] && [ "$(kv_get "$MANIFEST" commit)" = "$COMMIT" ] || die 2 "the manifest is not for $TAG at $COMMIT"
    [ "$(kv_get "$MANIFEST" flatpak_unsigned_tar_sha256)" = "$(kv_get "$IN/flatpak-prepared.txt" unsigned_tar_sha256)" ] || die 2 "the prepared repository does not come from the verified build"
    [ -f "$IN/flatpak-prepared-repo.tar" ] && [ ! -L "$IN/flatpak-prepared-repo.tar" ] || die 2 "$IN/flatpak-prepared-repo.tar is missing"
    [ "$(sha256_of "$IN/flatpak-prepared-repo.tar")" = "$(kv_get "$IN/flatpak-prepared.txt" prepared_tar_sha256)" ] || die 2 "the prepared repository is not the one that was described"
    if [ "$CHECK_REMOTE" = 1 ] && [ "$MODE" != fresh ]; then
        [ "$(remote_head gh-pages)" = "$(kv_get "$IN/flatpak-prepared.txt" base_gh_pages)" ] || die 5 "gh-pages is not at the base the preparation had: run all jobs again"
    fi
    extract_repo_tar "$IN/flatpak-prepared-repo.tar" "$WORK/x"
    mv "$WORK/x/repo" "$WORK/repo"
    rm -rf "$WORK/x"
    check_refs "$WORK/repo"
    # The content, against the manifest of the verify job: independent of the job that prepared it.
    [ "$(content_sum "$WORK/repo" "$APP_REF")" = "$(kv_get "$MANIFEST" flatpak_app_content)" ] || die 2 "the prepared app content is not the verified build"
    [ "$(content_sum "$WORK/repo" "$DEBUG_REF")" = "$(kv_get "$MANIFEST" flatpak_debug_content)" ] || die 2 "the prepared Debug content is not the verified build"
    [ "$(ostree --repo="$WORK/repo" rev-parse "$APP_REF")" = "$(kv_get "$IN/flatpak-prepared.txt" prepared_app_commit)" ] || die 2 "the prepared app commit is not the described one"
    [ "$(ostree --repo="$WORK/repo" rev-parse "$DEBUG_REF")" = "$(kv_get "$IN/flatpak-prepared.txt" prepared_debug_commit)" ] || die 2 "the prepared Debug commit is not the described one"
    # The only commits the Sign step will sign: the heads of the four named refs, as they are now.
    : > "$WORK/signlist"
    for ref in "$APP_REF" "$DEBUG_REF" appstream/x86_64 appstream2/x86_64; do
        repo_refs "$WORK/repo" | grep -qxF "$ref" || continue
        echo "$ref $(ostree --repo="$WORK/repo" rev-parse "$ref")" >> "$WORK/signlist"
    done
    [ "$(wc -l < "$WORK/signlist")" -ge 2 ] || die 2 "nothing to sign"
    echo "checked $TAG: $(wc -l < "$WORK/signlist") refs to sign"
}

# ---- sign: the only step with the secret ------------------------------------------------------------------------
# The keyring lives in a tmpfs (memory), so that nothing of the key is left on a disk when the job ends.
sign() {
    [ -n "$TAG" ] && [ -n "$COMMIT" ] && [ -n "$OUT" ] || die 1 "usage: see the header of $0"
    semver_valid "$TAG" || die 2 "$TAG is not a release tag"
    is_hex "$COMMIT" 40 || die 2 "$COMMIT is not a commit"
    # The secret is moved into a variable of this shell and out of the environment first of all: no child process
    # (gpg, flatpak, ostree) inherits it. (The environment of this process itself cannot be cleared.)
    secret=${FLATPAK_GPG_PRIVATE_KEY:-}
    unset FLATPAK_GPG_PRIVATE_KEY
    prepared_state "$WORK/prepared.txt"
    mkdir -p "$OUT"
    cp "$WORK/prepared.txt" "$OUT/flatpak-prepared.txt"
    if [ "$SKIP" = 1 ]; then
        echo "nothing to sign"
        return 0
    fi
    [ -f "$WORK/signlist" ] && [ -d "$WORK/repo/objects" ] || die 1 "$WORK is not the output of --check-prepared"
    [ -n "$secret" ] || die 4 "the secret FLATPAK_GPG_PRIVATE_KEY is empty or not set"

    shm=""
    if [ -d /dev/shm ] && [ -w /dev/shm ]; then shm=/dev/shm; fi
    if [ -n "$shm" ]; then
        KEYHOME=$(mktemp -d "$shm/paleto-gnupg.XXXXXX")
    else
        KEYHOME=$(mktemp -d "$TMPBASE/gnupg.XXXXXX")
    fi
    chmod 700 "$KEYHOME"
    trap 'gpgconf --homedir "$KEYHOME" --kill all > /dev/null 2>&1 || true; rm -rf "$KEYHOME"' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    trap 'exit 129' HUP
    export GNUPGHOME="$KEYHOME"
    printf '%s\n' "$secret" | gpg --batch --quiet --import > "$KEYHOME/import.log" 2>&1 || {
        unset secret
        die 4 "the signing key cannot be imported"
    }
    unset secret
    have=$(gpg --batch --list-secret-keys --with-colons 2> /dev/null | awk -F: '/^sec/ { s = 1; n++; next } s && /^fpr/ { print $10; s = 0 } END { if (n != 1) print "count:" n + 0 }')
    [ "$have" = "$FPR" ] || die 4 "the signing key is $have, not $FPR"
    # The public key that goes into the bundle comes from this keyring: one key, the pinned one.
    gpg --batch --export "$FPR" > "$OUT/flatpak-pubkey.gpg" 2> /dev/null
    [ "$(pubkey_fpr "$OUT/flatpak-pubkey.gpg")" = "$FPR" ] || die 4 "the public key exported from the keyring is not exactly $FPR"
    # Sign the heads --check-prepared named, and nothing else (a head that has a signature already came with the
    # published history).
    while read -r ref want; do
        c=$(ostree --repo="$WORK/repo" rev-parse "$ref")
        [ "$c" = "$want" ] || die 2 "$ref is not the commit that was checked"
        s=$(ostree --repo="$WORK/repo" show --print-detached-metadata-key=ostree.gpgsigs "$c" 2> /dev/null || true)
        [ -z "$s" ] || continue
        ostree --repo="$WORK/repo" gpg-sign --gpg-homedir="$GNUPGHOME" "$c" "$FPR" > "$KEYHOME/gpgsign.log" 2>&1 || {
            sed 's/^/    /' "$KEYHOME/gpgsign.log" >&2
            die 4 "ostree gpg-sign failed for $ref"
        }
    done < "$WORK/signlist"
    # The summary and its index, signed. The appstream data is not regenerated and there are no static deltas (see the
    # header).
    flatpak build-update-repo --no-update-appstream --gpg-sign="$FPR" --gpg-homedir="$GNUPGHOME" "$WORK/repo" > "$KEYHOME/update.log" 2>&1 || {
        sed 's/^/    /' "$KEYHOME/update.log" >&2
        die 4 "flatpak build-update-repo (summary signature) failed"
    }
    rm -rf "$WORK/repo/tmp"
    mkdir "$WORK/repo/tmp"
    tar -C "$WORK" --owner=0 --group=0 --numeric-owner -cf "$OUT/flatpak-sign-result-repo.tar" repo
    {
        echo "format=1"
        echo "tag=$TAG"
        echo "commit=$COMMIT"
        echo "signed_tar_sha256=$(sha256_of "$OUT/flatpak-sign-result-repo.tar")"
        echo "key_fpr=$FPR"
    } > "$OUT/flatpak-sign-result.txt"
    echo "signed $TAG"
}

# ---- finish -----------------------------------------------------------------------------------------------------
finish() {
    [ -n "$TAG" ] && [ -n "$COMMIT" ] && [ -n "$MANIFEST" ] && [ -n "$IN" ] && [ -n "$OUT" ] || die 1 "usage: see the header of $0"
    prepared_state "$IN/flatpak-prepared.txt"
    P=$IN/flatpak-prepared.txt
    mkdir -p "$OUT"
    if [ "$SKIP" = 1 ]; then
        {
            echo "format=1"
            echo "tag=$TAG"
            echo "version=$VERSION"
            echo "commit=$COMMIT"
            echo "skip=1"
        } > "$OUT/flatpak-signed.txt"
        echo "$TAG is already published for this commit: nothing was signed"
        return 0
    fi
    [ -f "$MANIFEST" ] || die 2 "$MANIFEST is missing"
    [ "$(kv_get "$MANIFEST" tag)" = "$TAG" ] && [ "$(kv_get "$MANIFEST" commit)" = "$COMMIT" ] || die 2 "the manifest is not for $TAG at $COMMIT"
    [ "$(kv_get "$MANIFEST" flatpak_unsigned_tar_sha256)" = "$(kv_get "$P" unsigned_tar_sha256)" ] || die 2 "the prepared repository does not come from the verified build"
    R=$IN/flatpak-sign-result.txt
    for f in flatpak-sign-result-repo.tar flatpak-pubkey.gpg flatpak-sign-result.txt; do
        [ -f "$IN/$f" ] && [ ! -L "$IN/$f" ] || die 2 "$IN/$f is missing"
    done
    [ "$(kv_get "$R" tag)" = "$TAG" ] && [ "$(kv_get "$R" commit)" = "$COMMIT" ] || die 2 "$R is not for $TAG at $COMMIT"
    [ "$(sha256_of "$IN/flatpak-sign-result-repo.tar")" = "$(kv_get "$R" signed_tar_sha256)" ] || die 2 "the signed repository is not the one that was described"
    rm -rf "$WORK"
    mkdir -p "$WORK"
    extract_repo_tar "$IN/flatpak-sign-result-repo.tar" "$WORK/x"
    mv "$WORK/x/repo" "$WORK/repo"
    rm -rf "$WORK/x"
    # Verified like a client does it, with the public key only.
    [ "$(pubkey_fpr "$IN/flatpak-pubkey.gpg")" = "$FPR" ] || die 4 "the public key is not exactly $FPR"
    (verify_signed_repo "$WORK/repo" "$IN/flatpak-pubkey.gpg" "$WORK/new" "$VERSION") || die 4 "the signed repository does not verify"
    APP_COMMIT=$(ostree --repo="$WORK/repo" rev-parse "$APP_REF")
    DEBUG_COMMIT=$(ostree --repo="$WORK/repo" rev-parse "$DEBUG_REF")
    # Signing only added signatures: the commits are the prepared ones.
    [ "$APP_COMMIT" = "$(kv_get "$P" prepared_app_commit)" ] || die 4 "the signed app commit is not the prepared one"
    [ "$DEBUG_COMMIT" = "$(kv_get "$P" prepared_debug_commit)" ] || die 4 "the signed Debug commit is not the prepared one"
    BASE_APP=$(kv_get "$P" base_app_commit)
    BASE_DEBUG=$(kv_get "$P" base_debug_commit)
    if [ -n "$BASE_APP" ]; then
        [ "$(ostree --repo="$WORK/repo" rev-parse "$APP_REF^")" = "$BASE_APP" ] || die 4 "the new app commit does not continue the published one"
        [ "$(ostree --repo="$WORK/repo" rev-parse "$DEBUG_REF^")" = "$BASE_DEBUG" ] || die 4 "the new Debug commit does not continue the published one"
    fi
    [ "$(content_sum "$WORK/repo" "$APP_REF")" = "$(kv_get "$MANIFEST" flatpak_app_content)" ] || die 4 "the signed app content is not the verified build"
    [ "$(content_sum "$WORK/repo" "$DEBUG_REF")" = "$(kv_get "$MANIFEST" flatpak_debug_content)" ] || die 4 "the signed Debug content is not the verified build"

    flatpak build-bundle --gpg-keys="$IN/flatpak-pubkey.gpg" "$WORK/repo" "$OUT/PaletoModkit-x86_64.flatpak" "$APP_ID" "$BRANCH" \
        --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo > "$WORK/bundle.log" 2>&1 || {
        sed 's/^/    /' "$WORK/bundle.log" >&2
        die 4 "flatpak build-bundle failed"
    }
    tar -C "$WORK" --owner=0 --group=0 --numeric-owner -cf "$OUT/flatpak-signed-repo.tar" repo
    cp "$IN/flatpak-pubkey.gpg" "$OUT/flatpak-pubkey.gpg"
    {
        echo "format=1"
        echo "tag=$TAG"
        echo "version=$VERSION"
        echo "commit=$COMMIT"
        echo "mode=$MODE"
        echo "skip=0"
        echo "base_gh_pages=$(kv_get "$P" base_gh_pages)"
        echo "base_gh_pages_previous=$(kv_get "$P" base_gh_pages_previous)"
        echo "base_app_commit=$BASE_APP"
        echo "base_debug_commit=$BASE_DEBUG"
        echo "app_commit=$APP_COMMIT"
        echo "debug_commit=$DEBUG_COMMIT"
        echo "summary_sha256=$(sha256_of "$WORK/repo/summary")"
        echo "bundle_sha256=$(sha256_of "$OUT/PaletoModkit-x86_64.flatpak")"
        echo "bundle_size=$(stat -c %s "$OUT/PaletoModkit-x86_64.flatpak")"
        echo "repo_tar_sha256=$(sha256_of "$OUT/flatpak-signed-repo.tar")"
        echo "unsigned_tar_sha256=$(kv_get "$P" unsigned_tar_sha256)"
        echo "key_fpr=$FPR"
    } > "$OUT/flatpak-signed.txt"
    echo "signed $TAG: app commit $APP_COMMIT"
}

case $CMD in
--prepare) prepare ;;
--check-prepared) check_prepared ;;
--sign) sign ;;
--finish) finish ;;
*) die 1 "unknown command $CMD (--prepare, --check-prepared, --sign, --finish, --cleanup)" ;;
esac
