#!/bin/sh
# First step of every release job: checks that the checked-out tag is a release that may be built and published,
# and says what to do. Run from a checkout of refs/tags/<tag> (fetch-depth 0), with the scripts of that checkout.
#   release-validate.sh --tag vX.Y.Z --mode build-only|release|publish-flatpak --event push|workflow_dispatch
#       writes tag, version, commit, prerelease, submodule, build_linux, publish and plan (build|publish|noop)
#       to $GITHUB_OUTPUT (stdout when it is not set)
#   release-validate.sh --check-head SHA
#       a later job: HEAD is that commit and the submodule is at the commit the tag records
# Environment: GITHUB_REPOSITORY, GITHUB_REF, GITHUB_SHA (the event), GH_TOKEN and GH_REPO (the gh calls),
# BOOTSTRAP=true (the first publication, when gh-pages does not exist yet), REMOTE, RELEASE_CONF.
# The rules (tag format, versions in the manifests, submodule, pins, no downgrade) are in docs/release.md.
# Exit codes: see release-lib.sh.
set -eu
# shellcheck source=/dev/null
. "$(dirname "$0")/release-lib.sh"

TAG="" MODE="" EVENT="" CHECK_HEAD=""
while [ $# -gt 0 ]; do
    case $1 in
        --tag) TAG=${2:?}; shift 2 ;;
        --mode) MODE=${2:?}; shift 2 ;;
        --event) EVENT=${2:?}; shift 2 ;;
        --check-head) CHECK_HEAD=${2:?}; shift 2 ;;
        *) die 1 "unknown argument $1" ;;
    esac
done

cd "$PKG_DIR/.."
HEAD_SHA=$(git rev-parse HEAD)
GITLINK=$(git ls-tree HEAD third_party/codewalker | awk '$1 == "160000" { print $3 }')

if [ -n "$CHECK_HEAD" ]; then
    [ "$HEAD_SHA" = "$CHECK_HEAD" ] || die 2 "the checkout is $HEAD_SHA, not the validated $CHECK_HEAD"
    [ -n "$GITLINK" ] || die 2 "no third_party/codewalker gitlink"
    sub=$(git -C third_party/codewalker rev-parse HEAD 2> /dev/null) || die 2 "the submodule is not checked out"
    [ "$sub" = "$GITLINK" ] || die 2 "the submodule is at $sub, the tag records $GITLINK"
    exit 0
fi

[ -n "$TAG" ] && [ -n "$MODE" ] && [ -n "$EVENT" ] || die 1 "usage: $0 --tag T --mode M --event E | --check-head SHA"
release_init
case $MODE in build-only | release | publish-flatpak) ;; *) die 2 "unknown mode $MODE" ;; esac
case $EVENT in push | workflow_dispatch) ;; *) die 2 "unknown event $EVENT" ;; esac
semver_valid "$TAG" || die 2 "$TAG is not a release tag (vMAJOR.MINOR.PATCH or vMAJOR.MINOR.PATCH-rc.N)"
VERSION=${TAG#v}
PRERELEASE=false
if is_rc "$TAG"; then PRERELEASE=true; fi
[ "$EVENT" != push ] || [ "$MODE" = release ] || die 2 "a tag push is always a release, not $MODE"
[ "$PRERELEASE" = false ] || [ "$MODE" != publish-flatpak ] || die 2 "a release candidate is never published to the Flatpak repository"

# ---- the ref and the commit -----------------------------------------------------------------------------------
TAG_COMMIT=$(git rev-parse --verify -q "refs/tags/$TAG^{commit}") || die 2 "the tag $TAG is not in this checkout"
[ "$HEAD_SHA" = "$TAG_COMMIT" ] || die 2 "HEAD is $HEAD_SHA, not the commit of $TAG ($TAG_COMMIT)"
if [ "$EVENT" = push ]; then
    [ "${GITHUB_SHA:-}" = "$TAG_COMMIT" ] || die 2 "GITHUB_SHA ${GITHUB_SHA:-unset} is not the commit of $TAG"
else
    [ "${GITHUB_REF:-}" = refs/heads/main ] || die 2 "a manual run must come from refs/heads/main, not ${GITHUB_REF:-unset}"
fi
PUBLISHING=false
[ "$MODE" = build-only ] || PUBLISHING=true
if [ "$PUBLISHING" = true ]; then
    [ "${GITHUB_REPOSITORY:-}" = "$(conf GITHUB_REPO)" ] || die 2 "GITHUB_REPOSITORY ${GITHUB_REPOSITORY:-unset} is not $(conf GITHUB_REPO)"
    remote_commit=$(remote_tag "$TAG")
    [ -n "$remote_commit" ] || die 2 "the tag $TAG does not exist on the remote"
    [ "$remote_commit" = "$HEAD_SHA" ] || die 5 "the tag $TAG moved on the remote: $remote_commit, this run has $HEAD_SHA"
    main=$(remote_head main)
    [ -n "$main" ] || die 2 "the remote has no main branch"
    git cat-file -e "$main^{commit}" 2> /dev/null || die 2 "main ($main) is not in this checkout: fetch the whole history"
    git merge-base --is-ancestor "$HEAD_SHA" "$main" || die 2 "$TAG is not on main"
fi

# ---- identity and pins ----------------------------------------------------------------------------------------
[ "$APP_ID" = io.github.the_beper.PaletoModkit ] || die 3 "the Flatpak id must stay io.github.the_beper.PaletoModkit, not $APP_ID"
[ "$BRANCH" = master ] || die 3 "the Flatpak branch must stay master, not $BRANCH"
conf_hex FLATPAK_KEY_FPR 40 > /dev/null
for k in APPIMAGETOOL_SHA256 APPIMAGE_RUNTIME_SHA256 SDK_COMMIT RUST_EXT_COMMIT DOTNET_EXT_COMMIT; do conf_hex $k 64 > /dev/null; done
conf FLATPAK_IMAGE | grep -Eq '@sha256:[0-9a-f]{64}$' || die 2 "FLATPAK_IMAGE in $RELEASE_CONF is not pinned by digest"
for k in APPIMAGETOOL_URL APPIMAGE_RUNTIME_URL SITE_URL GITHUB_REPO CODEWALKER_URL RELEASE_FILES; do
    [ -n "$(conf $k)" ] || die 2 "$k is empty in $RELEASE_CONF"
done

# ---- versions in the sources ----------------------------------------------------------------------------------
ws=$(awk '/^\[/ { s = ($0 == "[workspace.package]"); next } s && /^version *= *"/ { v = $0; sub(/^version *= *"/, "", v); sub(/".*/, "", v); print v; exit }' Cargo.toml)
[ "$ws" = "$VERSION" ] || die 2 "Cargo.toml has version ${ws:-none}, the tag is $VERSION"
for f in crates/*/Cargo.toml; do
    grep -Eq '^version\.workspace *= *true' "$f" || die 2 "$f does not use version.workspace = true"
done
locked=$(awk '/^\[\[package\]\]/ { n = ""; next } /^name = "paleto-/ { n = $0; sub(/^name = "/, "", n); sub(/".*/, "", n) }
    /^version = / && n != "" { v = $0; sub(/^version = "/, "", v); sub(/".*/, "", v); print n, v; n = "" }' Cargo.lock)
[ -n "$locked" ] || die 2 "Cargo.lock lists no paleto-* package"
bad=$(printf '%s\n' "$locked" | awk -v v="$VERSION" '$2 != v { print $1 " " $2 }')
[ -z "$bad" ] || die 2 "Cargo.lock is not at $VERSION for: $bad"
meta=packaging/$APP_ID.metainfo.xml
rel=$(xml_first_release "$meta")
[ -n "$rel" ] || die 2 "$meta has no <release>"
# shellcheck disable=SC2086
set -- $rel
[ "$1" = "$VERSION" ] || die 2 "the first <release> of $meta is $1, the tag is $VERSION"
printf '%s\n' "$2" | grep -Eq '^[0-9]{4}-[0-9]{2}-[0-9]{2}$' && [ "$(date -u -d "$2" +%F 2> /dev/null)" = "$2" ] || die 2 "the date of the first <release> of $meta is not YYYY-MM-DD: $2"
if [ "$PRERELEASE" = true ]; then
    [ "$3" = development ] || die 2 "the <release> of a release candidate needs type=\"development\" in $meta"
else
    [ "$3" = stable ] || die 2 "the <release> of $VERSION has type=\"development\" in $meta"
fi

# ---- the submodule --------------------------------------------------------------------------------------------
[ -f .gitmodules ] || die 2 "no .gitmodules"
paths=$(git config -f .gitmodules --get-regexp '^submodule\..*\.path$' | awk '{ print $2 }')
[ "$paths" = third_party/codewalker ] || die 2 ".gitmodules must have exactly third_party/codewalker, it has: $paths"
url=$(git config -f .gitmodules --get submodule.third_party/codewalker.url)
[ "$url" = "$(conf CODEWALKER_URL)" ] || die 2 "the CodeWalker URL is $url, not $(conf CODEWALKER_URL)"
[ -n "$GITLINK" ] || die 2 "third_party/codewalker is not a submodule commit"
[ "$(git ls-tree -r HEAD | awk '$1 == "160000"' | wc -l)" = 1 ] || die 2 "the tree has another gitlink than third_party/codewalker"

# ---- what to do -----------------------------------------------------------------------------------------------
PLAN=build
PUBLISH=false
BUILD_LINUX=true
if [ "$PUBLISHING" = true ]; then
    : "${GH_TOKEN:?set GH_TOKEN}"
    state=$(mktemp -d "${TMPDIR:-/tmp}/release-state.XXXXXX")
    trap 'rm -rf "$state"' EXIT
    release_state "$state"
    if [ "$PRERELEASE" = false ]; then
        if [ -z "$PG_HEAD" ]; then
            [ "${BOOTSTRAP:-}" = true ] || die 5 "gh-pages does not exist: run once with BOOTSTRAP=true to create it"
        else
            [ "${BOOTSTRAP:-}" != true ] || die 5 "gh-pages exists: BOOTSTRAP=true is only for the very first publication"
        fi
    fi
    PLAN=$(release_plan "$state" "$TAG" "$HEAD_SHA" "$MODE")
    case $PLAN in
        publish) PUBLISH=true ;;
        noop) PUBLISH=false; BUILD_LINUX=false ;;
        *) die 1 "bad plan $PLAN" ;;
    esac
    [ "$MODE" != publish-flatpak ] || BUILD_LINUX=false
fi

out=${GITHUB_OUTPUT:-/dev/stdout}
{
    echo "tag=$TAG"
    echo "version=$VERSION"
    echo "commit=$HEAD_SHA"
    echo "prerelease=$PRERELEASE"
    echo "submodule=$GITLINK"
    echo "build_linux=$BUILD_LINUX"
    echo "publish=$PUBLISH"
    echo "plan=$PLAN"
} >> "$out"
echo "$TAG: plan $PLAN (mode $MODE, commit $HEAD_SHA)"
