#!/bin/sh
# Simulates the release pipeline (the jobs of .github/workflows/release.yml) with the real scripts of
# packaging/, a throwaway git remote, the gh stub, and throwaway signing keys: no network, no GitHub, and nothing
# of the real home or Flatpak installation. Prints "PASS name" or "FAIL name: why" per scenario.
#   sh packaging/tests/release-sim.sh [scenario...]     (no argument: all of them; -l lists them)
#   SIM_BASE=dir   where the scratch tree goes (default $TMPDIR or /tmp; a disk is kinder than a tmpfs)
#   KEEP=1         keep the scratch tree; VERBOSE=1 print the logs of a failed scenario
# Needs flatpak, ostree, gpg, git, jq, tar, curl; exits 77 (skipped) if one is missing. Exit 1 if a scenario fails.
# What it cannot do: run the container builds (podman) and the real appimagetool: the build outputs are
# synthetic, structurally valid files (lib-fixture.sh); the scripts that check them are the real ones.
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
PKG=$(cd "$HERE/.." && pwd)
for t in flatpak ostree gpg git jq tar curl base64; do
    command -v "$t" > /dev/null || { echo "SKIP: $t is needed"; exit 77; }
done
# shellcheck source=/dev/null
. "$HERE/lib-fixture.sh"

SIM=$(mktemp -d "${SIM_BASE:-${TMPDIR:-/tmp}}/rs.XXXXXX")
cleanup() {
    for h in "$SIM"/rt/*/gnupg.* "$SIM"/keys/*.home; do
        [ -d "$h" ] && gpgconf --homedir "$h" --kill all > /dev/null 2>&1 || true
    done
    if [ "${KEEP:-}" = 1 ]; then echo "kept $SIM"; else rm -rf "$SIM"; fi
}
trap cleanup EXIT
fx_env "$SIM"
LIVE=$SIM/live
TPL=$SIM/tpl
KEYS=$SIM/keys
CW=$SIM/cw.git
BIN=$SIM/bin
mkdir -p "$KEYS" "$BIN" "$SIM/rt" "$SIM/tools"
chmod 700 "$KEYS"
ln -s "$HERE/gh" "$BIN/gh"
PATH=$BIN:$PATH
APP_ID=io.github.the_beper.PaletoModkit
APP_REF=app/$APP_ID/x86_64/master

# Everything below talks to a local remote and the stub; the scripts check it again (RELEASE_SIM).
unset GITHUB_TOKEN GH_ENTERPRISE_TOKEN
GH_TOKEN=sim-token-not-a-secret
GH_REPO=sim/paleto
GITHUB_REPOSITORY=sim/paleto
REMOTE=file://$LIVE/origin.git
GH_STUB_DIR=$LIVE/stub
GH_STUB_REMOTE=$REMOTE
RELEASE_SIM=1
RELEASE_SIM_GH=$BIN/gh
export GH_TOKEN GH_REPO GITHUB_REPOSITORY REMOTE GH_STUB_DIR GH_STUB_REMOTE RELEASE_SIM RELEASE_SIM_GH PATH
case $REMOTE$GH_REPO in *github.com* | *the-beper*) echo "the simulation must not point at GitHub" >&2; exit 1 ;; esac

kv() { sed -n "s/^$2=//p" "$1" | head -n1; }
in_dir() { (cd "$1" && shift && "$@"); }
# run LOG CMD...: runs, logs, sets RC; never fails
run() { RUNLOG=$1; shift; RC=0; "$@" > "$RUNLOG" 2>&1 || RC=$?; }

# ---- the world ------------------------------------------------------------------------------------------------

write_conf() { # FILE
    cat > "$1" <<CONF
APP_ID=io.github.the_beper.PaletoModkit
FLATPAK_BRANCH=master
SITE_URL=file://$LIVE/site
GITHUB_REPO=sim/paleto
FLATPAK_KEY_FPR=$FPR_K
FLATPAK_IMAGE=example.invalid/image:1@sha256:$(printf 'a%.0s' $(seq 64))
SDK_COMMIT=$(printf 'b%.0s' $(seq 64))
RUST_EXT_COMMIT=$(printf 'c%.0s' $(seq 64))
DOTNET_EXT_COMMIT=$(printf 'd%.0s' $(seq 64))
APPIMAGETOOL_URL=file://$SIM/tools/appimagetool
APPIMAGETOOL_SHA256=$(sha256sum "$SIM/tools/appimagetool" | cut -d' ' -f1)
APPIMAGE_RUNTIME_URL=file://$SIM/tools/runtime
APPIMAGE_RUNTIME_SHA256=$(sha256sum "$SIM/tools/runtime" | cut -d' ' -f1)
CODEWALKER_URL=file://$CW
RELEASE_FILES=PaletoModkit-x86_64.tar.gz PaletoModkit-x86_64.AppImage PaletoModkit-x86_64.AppImage.zsync PaletoModkit-x86_64.flatpak
CONF
}

install_hooks() {
    cat > "$LIVE/origin.git/hooks/pre-receive" <<HOOK
#!/bin/sh
while read -r old new ref; do
    if [ "\$ref" = refs/heads/gh-pages ] && [ -e "$LIVE/reject-once" ]; then
        rm -f "$LIVE/reject-once"
        echo "pre-receive: rejected on purpose" >&2
        exit 1
    fi
done
HOOK
    # The "Pages deployment": the site folder follows gh-pages, unless a test says it does not.
    cat > "$LIVE/origin.git/hooks/post-receive" <<HOOK
#!/bin/sh
[ ! -e "$LIVE/nodeploy" ] || exit 0
git rev-parse -q --verify refs/heads/gh-pages > /dev/null || exit 0
rm -rf "$LIVE/site"
mkdir -p "$LIVE/site"
git archive refs/heads/gh-pages | tar -x -C "$LIVE/site"
HOOK
    chmod +x "$LIVE/origin.git/hooks/pre-receive" "$LIVE/origin.git/hooks/post-receive"
}

checkout() { # DIR REF: a fresh clone at REF, with the submodule
    rm -rf "$1"
    git clone -q "$REMOTE" "$1"
    git -C "$1" checkout -q --detach "$2"
    git -C "$1" submodule update -q --init
}

# commit_release VERSION [MUTATOR]: a commit on main (or $ON_BRANCH) that makes the project VERSION.
commit_release() {
    w=$LIVE/work
    git -C "$w" fetch -q origin
    git -C "$w" checkout -q -B "${ON_BRANCH:-main}" "origin/${ON_BRANCH:-main}" 2> /dev/null || git -C "$w" checkout -q -B "${ON_BRANCH:-main}" origin/main
    fx_set_version "$w" "$1"
    echo "marker-$(fx_nonce)" > "$w/marker.txt"
    if [ -n "${2:-}" ]; then "$2" "$w"; fi
    git -C "$w" add -A
    git -C "$w" commit -q -m "Release $1"
    git -C "$w" push -q origin "HEAD:refs/heads/${ON_BRANCH:-main}"
}
tag_release() { # TAG [COMMIT]
    git -C "$LIVE/work" tag -f -a "$1" -m "$1" "${2:-HEAD}" > /dev/null
    git -C "$LIVE/work" push -q -f origin "refs/tags/$1"
}

# ---- the jobs ---------------------------------------------------------------------------------------------------

RC=0 PIPE_FAIL="" PIPE_PLAN="" RUNLOG=/dev/null
BOOTSTRAP=false
FAKE_FAIL=
FAKE_CWCONV=
SECRET_FILE=

job_validate() { # RUN TAG MODE [EVENT]
    d=$LIVE/runs/$1
    rm -rf "$d"
    mkdir -p "$d"
    event=${4:-push}
    checkout "$d/src" origin/main
    if git -C "$d/src" rev-parse -q --verify "refs/tags/$2^{commit}" > /dev/null; then git -C "$d/src" checkout -q --detach "refs/tags/$2"; fi
    if [ "$event" = push ]; then
        sha=$(git -C "$d/src" rev-parse HEAD)
        ref=refs/tags/$2
    else
        sha=$(git -C "$d/src" rev-parse origin/main)
        ref=refs/heads/main
    fi
    : > "$d/validate.out"
    run "$d/validate.log" in_dir "$d/src" env GITHUB_OUTPUT="$d/validate.out" GITHUB_SHA="$sha" GITHUB_REF="$ref" BOOTSTRAP="$BOOTSTRAP" \
        sh packaging/release-validate.sh --tag "$2" --mode "$3" --event "$event"
}

job_build() { # RUN
    d=$LIVE/runs/$1
    commit=$(kv "$d/validate.out" commit)
    checkout "$d/build" "$commit"
    run "$d/check-head.log" in_dir "$d/build" sh packaging/release-validate.sh --check-head "$commit"
    [ "$RC" = 0 ] || return 0
    RC=0
    if [ "$(kv "$d/validate.out" build_linux)" = true ]; then
        [ "${FAKE_FAIL:-}" != linux ] || { RC=1; echo "the Linux build failed" > "$d/build-linux.log"; return 0; }
        fx_linux "$d/build" "$d/linux" "${FAKE_CWCONV:-}"
    fi
    [ "${FAKE_FAIL:-}" != flatpak ] || { RC=1; echo "the Flatpak build failed" > "$d/build-flatpak.log"; return 0; }
    fx_flatpak "$d/build" "$d/fp"
}

job_verify() { # RUN
    d=$LIVE/runs/$1
    linux=""
    [ ! -d "$d/linux" ] || linux=$d/linux
    : > "$d/verify.gho"
    run "$d/verify.log" in_dir "$d/build" env GITHUB_OUTPUT="$d/verify.gho" sh packaging/release-verify.sh builds --tag "$(kv "$d/validate.out" tag)" --commit "$(kv "$d/validate.out" commit)" \
        ${linux:+--linux "$linux"} --flatpak "$d/fp" --out "$d/verified"
    # the job output: the later jobs check the manifest they download against it
    MANIFEST_SHA256=$(kv "$d/verify.gho" manifest_sha256)
    export MANIFEST_SHA256
}

# job_sign RUN: fetch Pages (stable) and sign; SECRET_FILE says which key.
job_sign() {
    d=$LIVE/runs/$1
    tag=$(kv "$d/validate.out" tag)
    commit=$(kv "$d/validate.out" commit)
    rt=$SIM/rt/$1
    rm -rf "$rt"
    mkdir -p "$rt"
    secret=$(cat "${SECRET_FILE:-$KEYS/K.sec}")
    case $tag in
        *-rc.*) site="--fresh" csite="--fresh" ;;
        *)
            boot=""
            [ "$BOOTSTRAP" != true ] || boot=--bootstrap
            run "$d/pages.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/release-pages.sh fetch "$rt/pages" $boot
            [ "$RC" = 0 ] || return 0
            site="--site $rt/pages $boot"
            # sign-flatpak fetches gh-pages again, on a machine of its own
            run "$d/pages2.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/release-pages.sh fetch "$rt/pages2" $boot
            [ "$RC" = 0 ] || return 0
            csite="--site $rt/pages2 $boot"
            ;;
    esac
    # The steps of the jobs: prepare (no secret), check-prepared (none), sign (the secret, nothing else), finish (none).
    sign_args="--tag $tag --commit $commit"
    m=$d/verified/release-manifest.txt
    MANIFEST_SHA256=$(kv "$d/verify.gho" manifest_sha256)
    export MANIFEST_SHA256
    rm -f "$d/sign-prepare.log" "$d/sign-check.log" "$d/sign-sign.log" "$d/sign-finish.log"
    rm -rf "$d/prepared" "$d/signres"
    : > "$d/finish.gho"
    : > "$d/sign.gho"
    # shellcheck disable=SC2086
    run "$d/sign-prepare.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/flatpak-sign.sh --prepare $sign_args --manifest "$m" \
        --unsigned "$d/fp/flatpak-unsigned-repo.tar" $site --work "$rt/prepwork" --out "$d/prepared"
    rc=$RC
    # PREPARED_HOOK DIR: a function that edits the output of --prepare, as a compromised prepare job would
    if [ "$rc" = 0 ] && [ -n "${PREPARED_HOOK:-}" ]; then "$PREPARED_HOOK" "$d/prepared"; fi
    if [ "$rc" = 0 ]; then
        # shellcheck disable=SC2086
        run "$d/sign-check.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/flatpak-sign.sh --check-prepared $sign_args --manifest "$m" --in "$d/prepared" --work "$rt/signwork" $csite --check-remote
        rc=$RC
    fi
    if [ "$rc" = 0 ]; then
        # shellcheck disable=SC2086
        run "$d/sign-sign.log" in_dir "$d/build" env RUNNER_TEMP="$rt" GITHUB_OUTPUT="$d/sign.gho" FLATPAK_GPG_PRIVATE_KEY="$secret" sh packaging/flatpak-sign.sh --sign $sign_args --work "$rt/signwork" --out "$d/signres"
        rc=$RC
    fi
    if [ "$rc" = 0 ]; then
        # shellcheck disable=SC2086
        run "$d/sign-finish.log" in_dir "$d/build" env RUNNER_TEMP="$rt" RESULT_SHA256="$(kv "$d/sign.gho" result_sha256)" GITHUB_OUTPUT="$d/finish.gho" sh packaging/flatpak-sign.sh --finish $sign_args --manifest "$m" --in "$d/signres" --work "$rt/finwork" --out "$d/signed"
        rc=$RC
    fi
    cat "$d"/sign-prepare.log "$d"/sign-check.log "$d"/sign-sign.log "$d"/sign-finish.log > "$d/sign.log" 2> /dev/null || true
    in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/flatpak-sign.sh --cleanup --work "$rt/signwork" > "$d/cleanup.log" 2>&1 || true
    RC=$rc
}

job_publish() { # RUN MODE
    d=$LIVE/runs/$1
    mode=${2:-release}
    rm -rf "$d/in"
    mkdir -p "$d/in"
    [ ! -d "$d/linux" ] || cp "$d"/linux/PaletoModkit-x86_64.* "$d/in/"
    cp "$d/verified/release-manifest.txt" "$d/in/"
    cp "$d"/signed/* "$d/in/"
    boot=""
    [ "$BOOTSTRAP" != true ] || boot=--bootstrap
    run "$d/publish.log" in_dir "$d/build" env RUNNER_TEMP="$SIM/rt/$1" MANIFEST_SHA256="$(kv "$d/verify.gho" manifest_sha256)" SIGNED_SHA256="$(kv "$d/finish.gho" signed_sha256)" PAGES_WAIT=3 PAGES_POLL=1 RELEASE_WAIT=6 RELEASE_POLL=1 GITHUB_RUN_ID="$1" GITHUB_RUN_ATTEMPT=1 \
        sh packaging/release-publish.sh --mode "$mode" --tag "$(kv "$d/validate.out" tag)" --commit "$(kv "$d/validate.out" commit)" --in "$d/in" $boot
}

# pipeline RUN TAG MODE [EVENT [UNTIL]]: the jobs in order, stopping at the first that fails (like "needs").
# Sets RC and PIPE_FAIL (the job that failed, empty if none), PIPE_PLAN.
pipeline() {
    n=$1 tag=$2 mode=$3 event=${4:-push} until=${5:-publish}
    PIPE_FAIL="" PIPE_PLAN=""
    job_validate "$n" "$tag" "$mode" "$event"
    [ "$RC" = 0 ] || { PIPE_FAIL=validate; return 0; }
    PIPE_PLAN=$(kv "$LIVE/runs/$n/validate.out" plan)
    [ "$PIPE_PLAN" != noop ] || return 0
    [ "$until" != validate ] || return 0
    job_build "$n"
    [ "$RC" = 0 ] || { PIPE_FAIL=build; return 0; }
    job_verify "$n"
    [ "$RC" = 0 ] || { PIPE_FAIL=verify; return 0; }
    [ "$(kv "$LIVE/runs/$n/validate.out" publish)" = true ] || return 0
    [ "$until" != verify ] || return 0
    job_sign "$n"
    [ "$RC" = 0 ] || { PIPE_FAIL=sign; return 0; }
    [ "$until" != sign ] || return 0
    job_publish "$n" "$mode"
    [ "$RC" = 0 ] || { PIPE_FAIL=publish; return 0; }
}

# ---- assertions and state ---------------------------------------------------------------------------------------

fail() { echo "$*" >> "$FAILFILE"; }
assert_eq() { [ "$2" = "$3" ] || fail "$1: expected '$2', got '$3'"; }
assert_rc() { assert_eq "$1 exit code" "$2" "$RC"; }
assert_file() { [ -e "$2" ] || fail "$1: $2 is missing"; }
assert_log() { # LOGFILE PATTERN
    grep -q -- "$2" "$1" 2> /dev/null || fail "the log $(basename "$1") does not say '$2'"
}
# the id of the release of a tag in the stub
stub_rel() { for x in "$GH_STUB_DIR"/rel/*; do [ -d "$x" ] && [ "$(cat "$x/tag")" = "$1" ] && basename "$x"; done; return 0; }
stub_field() { cat "$GH_STUB_DIR/rel/$(stub_rel "$1")/$2" 2> /dev/null || echo none; }
stub_assets() { find "$GH_STUB_DIR/rel/$(stub_rel "$1")/assets" -type f -not -name '*.meta' 2> /dev/null | wc -l; }
# a picture of everything a publish could change
snapshot() {
    git ls-remote "$REMOTE"
    (cd "$GH_STUB_DIR" 2> /dev/null && find . -type f -not -name log | LC_ALL=C sort | xargs -r sha256sum)
    (cd "$LIVE/site" 2> /dev/null && find . -type f -not -path './repo/*' | LC_ALL=C sort | xargs -r sha256sum)
}
pages_count() { git -C "$LIVE/origin.git" rev-list --count refs/heads/gh-pages; }
record() { git -C "$LIVE/origin.git" show "$1:release.txt" 2> /dev/null; }
leftover_gnupg() { { find "$SIM/rt" -maxdepth 2 -name 'gnupg.*' 2> /dev/null; find /dev/shm -maxdepth 1 -name 'paleto-gnupg.*' -user "$(id -u)" 2> /dev/null; } | wc -l; }

# ---- scenarios --------------------------------------------------------------------------------------------------

sc_valid_stable() {
    before=$(git -C "$LIVE/origin.git" rev-parse gh-pages)
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 1 v1.6.0 release
    assert_rc "pipeline" 0
    assert_eq "failed job" "" "$PIPE_FAIL"
    assert_eq "the release is a draft" false "$(stub_field v1.6.0 draft)"
    assert_eq "the release assets (4 files and SHA256SUMS)" 5 "$(stub_assets v1.6.0)"
    assert_eq "latest release" v1.6.0 "$(cat "$GH_STUB_DIR/latest")"
    # the notes are the list of the metainfo release, not the merged pull requests
    assert_eq "release notes" "$(printf '%s\n\n%s\n\n%s' "## What's Changed" "- A change of \`paleto\`." "**Full Changelog**: https://github.com/$GH_REPO/compare/v1.0.0...v1.6.0")" "$(stub_field v1.6.0 notes)"
    assert_eq "gh-pages tag" v1.6.0 "$(kv "$LIVE/site/release.txt" tag)"
    assert_eq "gh-pages version" 1.6.0 "$(kv "$LIVE/site/release.txt" version)"
    assert_eq "gh-pages commits" 1 "$(pages_count)"
    assert_eq "gh-pages-previous is the old gh-pages" "$before" "$(git -C "$LIVE/origin.git" rev-parse gh-pages-previous)"
    prev_app=$(record gh-pages-previous | sed -n 's/^ostree_app_commit=//p')
    assert_eq "app commit parent" "$prev_app" "$(ostree --repo="$LIVE/site/repo" rev-parse "$APP_REF^")"
    assert_eq "app commit" "$(kv "$LIVE/site/release.txt" ostree_app_commit)" "$(ostree --repo="$LIVE/site/repo" rev-parse "$APP_REF")"
    cmp -s "$TPL/site/paleto-modkit.flatpakrepo" "$LIVE/site/paleto-modkit.flatpakrepo" || fail "the .flatpakrepo changed"
    cmp -s "$TPL/site/paleto-modkit.flatpakref" "$LIVE/site/paleto-modkit.flatpakref" || fail ".flatpakref changed"
    # the release assets are exactly the verified files
    assert_eq "SHA256SUMS lines" 4 "$(wc -l < "$GH_STUB_DIR/rel/$(stub_rel v1.6.0)/assets/SHA256SUMS")"
    (cd "$GH_STUB_DIR/rel/$(stub_rel v1.6.0)/assets" && sha256sum --strict -c SHA256SUMS > /dev/null) || fail "SHA256SUMS does not check against the release assets"
}

sc_invalid_tag() {
    for t in v1.6 v01.6.0 v1.6.0-beta.1 1.6.0 v1.6.0-rc.0 v1.6.0-rc.01; do
        job_validate 1 "$t" release
        assert_rc "validate $t" 2
        assert_log "$LIVE/runs/1/validate.log" "not a release tag"
    done
}

mut_cargo() { sed -i 's/^version = "1.6.0"/version = "1.5.1"/' "$1/Cargo.toml"; }
mut_lock() { sed -i '/^name = "paleto-gui"/{n;s/version = "1.6.0"/version = "1.5.1"/}' "$1/Cargo.lock"; }
mut_metainfo() { fx_metainfo "$1/packaging/$APP_ID.metainfo.xml" "$APP_ID" 1.5.1 2026-10-29; }
mut_date() { sed -i 's|date="2026-10-29"|date="29/10/2026"|' "$1/packaging/$APP_ID.metainfo.xml"; }
mut_submodule() { git -C "$1" config -f .gitmodules submodule.third_party/codewalker.url https://example.invalid/CodeWalker.git; }
mut_extra_submodule() { git -C "$1" config -f .gitmodules submodule.extra.path extra; git -C "$1" config -f .gitmodules submodule.extra.url "file://$CW"; }
mut_rc_type() { sed -i 's/ type="development"//' "$1/packaging/$APP_ID.metainfo.xml"; }
mut_stable_dev() { sed -i 's/date="2026-10-29"/date="2026-10-29" type="development"/' "$1/packaging/$APP_ID.metainfo.xml"; }
mut_pin() { sed -i 's/^SDK_COMMIT=.*/SDK_COMMIT=/' "$1/packaging/release.conf"; }
mut_identity() { sed -i 's/^APP_ID=.*/APP_ID=io.github.other.App/' "$1/packaging/release.conf"; }
mutated() { # NAME TAG MUTATOR
    commit_release "${2#v}" "$3"
    tag_release "$2"
    job_validate 1 "$2" release
    assert_rc "$1: validate" "${4:-2}"
}
sc_mismatch_cargo() { mutated cargo v1.6.0 mut_cargo; }
sc_mismatch_lock() { mutated lock v1.6.0 mut_lock; }
sc_mismatch_metainfo() { mutated metainfo v1.6.0 mut_metainfo; }
sc_mismatch_date() { mutated date v1.6.0 mut_date; }
sc_mismatch_submodule() { mutated submodule v1.6.0 mut_submodule; }
sc_mismatch_extra_submodule() { mutated "extra submodule" v1.6.0 mut_extra_submodule; }
sc_mismatch_rc_type() { mutated "rc without development type" v1.6.0-rc.1 mut_rc_type; }
sc_mismatch_stable_dev() { mutated "stable with development type" v1.6.0 mut_stable_dev; }
sc_empty_pin() { mutated "empty pin" v1.6.0 mut_pin; }
sc_identity_change() { mutated "identity change" v1.6.0 mut_identity 3; }

sc_prerelease() {
    before=$(git -C "$LIVE/origin.git" rev-parse gh-pages)
    commit_release 1.7.0-rc.1
    tag_release v1.7.0-rc.1
    pipeline 1 v1.7.0-rc.1 release
    assert_rc "pipeline" 0
    assert_eq "failed job" "" "$PIPE_FAIL"
    assert_eq "prerelease flag" true "$(stub_field v1.7.0-rc.1 prerelease)"
    assert_eq "draft flag" false "$(stub_field v1.7.0-rc.1 draft)"
    assert_eq "assets" 5 "$(stub_assets v1.7.0-rc.1)"
    assert_eq "gh-pages unchanged" "$before" "$(git -C "$LIVE/origin.git" rev-parse gh-pages)"
    assert_eq "latest unchanged" v1.5.1 "$(cat "$GH_STUB_DIR/latest")"
    # an rc cannot go to Pages
    job_validate 2 v1.7.0-rc.1 publish-flatpak
    assert_rc "publish-flatpak of an rc" 2
}

sc_main_moved() {
    commit_release 1.6.0
    tag_release v1.6.0
    tagged_marker=$(git -C "$LIVE/work" show v1.6.0:marker.txt)
    commit_release 1.6.1
    pipeline 1 v1.6.0 release
    assert_rc "pipeline" 0
    assert_eq "failed job" "" "$PIPE_FAIL"
    d=$LIVE/runs/1
    assert_eq "marker in the tarball" "$tagged_marker" "$(tar -xzOf "$d/linux/PaletoModkit-x86_64.tar.gz" paleto-modkit-1.6.0/MARKER)"
    assert_eq "manifest version" 1.6.0 "$(kv "$d/verified/release-manifest.txt" version)"
    assert_eq "release.txt version" 1.6.0 "$(kv "$LIVE/site/release.txt" version)"
}

sc_tag_not_on_main() {
    ON_BRANCH=side
    git -C "$LIVE/work" checkout -q -B side origin/main
    git -C "$LIVE/work" push -q origin side
    commit_release 1.6.0
    tag_release v1.6.0
    job_validate 1 v1.6.0 release
    assert_rc "validate" 2
    assert_log "$LIVE/runs/1/validate.log" "not on main"
    # a dry run (build-only) does not need main
    job_validate 2 v1.6.0 build-only workflow_dispatch
    assert_rc "validate build-only" 0
}

sc_build_failure() {
    commit_release 1.6.0
    tag_release v1.6.0
    snapshot > "$SIM/snap.before"
    FAKE_FAIL=linux
    pipeline 1 v1.6.0 release
    FAKE_FAIL=
    assert_eq "failed job" build "$PIPE_FAIL"
    [ "$RC" != 0 ] || fail "the build did not fail"
    [ ! -d "$LIVE/runs/1/verified" ] || fail "verify ran after a failed build"
    [ ! -d "$LIVE/runs/1/signed" ] || fail "signing ran after a failed build"
    snapshot > "$SIM/snap.after"
    cmp -s "$SIM/snap.before" "$SIM/snap.after" || fail "the remote changed"
}

sc_converter_failure() {
    commit_release 1.6.0
    tag_release v1.6.0
    FAKE_CWCONV=nocwconv
    pipeline 1 v1.6.0 release
    FAKE_CWCONV=
    assert_eq "failed job" verify "$PIPE_FAIL"
    assert_rc "verify" 2
    assert_log "$LIVE/runs/1/verify.log" "paleto-cwconv"
}

sc_missing_zsync() {
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 1 v1.6.0 release push validate
    job_build 1
    rm "$LIVE/runs/1/linux/PaletoModkit-x86_64.AppImage.zsync"
    job_verify 1
    assert_rc "verify" 2
    assert_log "$LIVE/runs/1/verify.log" "zsync"
}

sc_zsync_length() {
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 1 v1.6.0 release push validate
    job_build 1
    sed -i 's/^Length: .*/Length: 1/' "$LIVE/runs/1/linux/PaletoModkit-x86_64.AppImage.zsync"
    job_verify 1
    assert_rc "verify" 2
    assert_log "$LIVE/runs/1/verify.log" "Length"
}

sc_flipped_appimage() {
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 1 v1.6.0 release push sign
    assert_eq "failed job" "" "$PIPE_FAIL"
    snapshot > "$SIM/snap.before"
    f=$LIVE/runs/1/linux/PaletoModkit-x86_64.AppImage
    printf 'X' | dd of="$f" bs=1 seek=100 conv=notrunc 2> /dev/null
    job_publish 1 release
    assert_rc "publish" 2
    assert_log "$LIVE/runs/1/publish.log" "AppImage is not the verified build"
    snapshot > "$SIM/snap.after"
    cmp -s "$SIM/snap.before" "$SIM/snap.after" || fail "the remote changed"
}

sc_tool_pin() {
    # fetch-pinned.sh by itself
    printf 'tool' > "$SIM/tools/x"
    good=$(sha256sum "$SIM/tools/x" | cut -d' ' -f1)
    bad=$(printf 'e%.0s' $(seq 64))
    run "$SIM/fp.log" sh "$PKG/fetch-pinned.sh" "file://$SIM/tools/x" "$bad" "$SIM/dl/tool" executable
    assert_rc "fetch with a wrong hash" 2
    [ ! -e "$SIM/dl/tool" ] && [ ! -e "$SIM/dl/tool.part" ] || fail "an unverified file was left behind"
    run "$SIM/fp.log" sh "$PKG/fetch-pinned.sh" "file://$SIM/tools/x" "$good" "$SIM/dl/tool" executable
    assert_rc "fetch with the right hash" 0
    [ -x "$SIM/dl/tool" ] || fail "the verified tool is not executable"
    # a cached file that no longer matches is replaced, a bad one is refused
    echo tampered > "$SIM/dl/tool"
    run "$SIM/fp.log" sh "$PKG/fetch-pinned.sh" "file://$SIM/tools/x" "$good" "$SIM/dl/tool" executable
    assert_rc "fetch over a tampered cache" 0
    assert_eq "cache restored" tool "$(cat "$SIM/dl/tool")"
    run "$SIM/fp.log" sh "$PKG/fetch-pinned.sh" "http://example.invalid/x" "$good" "$SIM/dl/tool2"
    assert_rc "plain http" 1
    # build-release.sh: a bad pin stops it before anything is compiled or run
    [ "$(uname -m)" = x86_64 ] || { echo "skipped the build-release.sh part: not x86_64" >&2; return 0; }
    w=$SIM/br
    rm -rf "$w"
    git clone -q "$REMOTE" "$w"
    mkdir -p "$SIM/fakebin"
    printf '#!/bin/sh\ntouch "%s/cargo-ran"\nexit 99\n' "$SIM" > "$SIM/fakebin/cargo"
    printf '#!/bin/sh\nexit 0\n' > "$SIM/fakebin/zsyncmake"
    chmod +x "$SIM/fakebin/cargo" "$SIM/fakebin/zsyncmake"
    rm -f "$SIM/cargo-ran"
    sed "s/^APPIMAGETOOL_SHA256=.*/APPIMAGETOOL_SHA256=$bad/" "$w/packaging/release.conf" > "$SIM/bad.conf"
    run "$SIM/br.log" in_dir "$w" env RELEASE_CONF="$SIM/bad.conf" PATH="$SIM/fakebin:$PATH" sh packaging/build-release.sh
    assert_rc "build-release.sh with a wrong appimagetool hash" 2
    [ ! -e "$SIM/cargo-ran" ] || fail "the build ran with an unverified tool"
    [ ! -e "$w/target/tools/appimagetool-$bad" ] || fail "the unverified appimagetool is in place"
    sed "s/^APPIMAGE_RUNTIME_SHA256=.*/APPIMAGE_RUNTIME_SHA256=/" "$w/packaging/release.conf" > "$SIM/empty.conf"
    run "$SIM/br.log" in_dir "$w" env RELEASE_CONF="$SIM/empty.conf" PATH="$SIM/fakebin:$PATH" sh packaging/build-release.sh
    assert_rc "build-release.sh with an empty pin" 2
    run "$SIM/br.log" in_dir "$w" env EXPECT_VERSION=9.9.9 PATH="$SIM/fakebin:$PATH" sh packaging/build-release.sh
    assert_rc "build-release.sh with another version than Cargo.toml" 2
    # with the right pins it gets as far as the build (the fake cargo stops it)
    run "$SIM/br.log" in_dir "$w" env PATH="$SIM/fakebin:$PATH" sh packaging/build-release.sh
    assert_rc "build-release.sh with the right pins reaches cargo" 99
    [ -x "$w/target/tools/appimagetool-$(sha256sum "$SIM/tools/appimagetool" | cut -d' ' -f1)" ] || fail "the verified appimagetool is not in place"
}

sc_build_flatpak_script() {
    # a fake container engine: the script must be unsigned-only, pinned, and must not give the container a key
    w=$SIM/bf
    rm -rf "$w"
    git clone -q "$REMOTE" "$w"
    mkdir -p "$SIM/fakeeng"
    cat > "$SIM/fakeeng/podman" <<'ENGINE'
#!/bin/sh
case $1 in
    info) echo "${FAKE_ROOTLESS:-true}"; exit 0 ;;
    run) ;;
    *) exit 1 ;;
esac
echo "$*" > "$FAKE_LOG"
shift
work=""
while [ $# -gt 0 ]; do
    case $1 in
        -v) case $2 in *:/work) work=${2%:/work} ;; esac; shift 2 ;;
        *) shift ;;
    esac
done
cp -a "$FAKE_REPO/repo" "$work/repo"
printf 'sdk_commit=1\napp_commit=%s\n' "$(ostree --repo="$work/repo" rev-parse app/io.github.the_beper.PaletoModkit/x86_64/master)" > "$work/build-info.txt"
echo bundle > "$work/PaletoModkit-x86_64.flatpak"
ENGINE
    chmod +x "$SIM/fakeeng/podman"
    fx_unsigned_repo "$SIM/fakerepo" "$w/packaging/$APP_ID.metainfo.xml" "$APP_ID"
    FAKE_LOG=$SIM/engine.log
    FAKE_REPO=$SIM/fakerepo
    export FAKE_LOG FAKE_REPO
    # shellcheck disable=SC2329
    bf() { in_dir "$w" env PATH="$SIM/fakeeng:$PATH" WORK="$SIM/bfwork" OUT="$SIM/bfout" "$@"; }
    run "$SIM/bf.log" bf GPG_KEY_ID=ABCD sh packaging/build-flatpak.sh
    assert_rc "with GPG_KEY_ID" 1
    run "$SIM/bf.log" bf GNUPGHOME="$SIM/keys" sh packaging/build-flatpak.sh
    assert_rc "with GNUPGHOME" 1
    run "$SIM/bf.log" bf FAKE_ROOTLESS=false sh packaging/build-flatpak.sh
    assert_rc "rootful engine" 1
    run "$SIM/bf.log" bf EXPORT_REPO=1 BUNDLE=0 sh packaging/build-flatpak.sh
    assert_rc "release build" 0
    assert_file "unsigned repository tar" "$SIM/bfout/flatpak-unsigned-repo.tar"
    assert_file "build description" "$SIM/bfout/flatpak-build.txt"
    [ ! -e "$SIM/bfout/PaletoModkit-x86_64.flatpak" ] || fail "BUNDLE=0 made a bundle"
    assert_eq "commit in flatpak-build.txt" "$(git -C "$w" rev-parse HEAD)" "$(kv "$SIM/bfout/flatpak-build.txt" commit)"
    assert_log "$SIM/engine.log" "--privileged"
    assert_log "$SIM/engine.log" "@sha256:"
    assert_log "$SIM/engine.log" "export FLATPAK_SYSTEM_DIR=/work/flatpak"
    if grep -q -i 'gnupg\|GPG' "$SIM/engine.log"; then fail "the container was given something about GPG"; fi
    # what a release verify makes of the exported repository
    run "$SIM/bf.log" in_dir "$w" sh packaging/release-verify.sh builds --tag v1.5.1 --commit "$(git -C "$w" rev-parse HEAD)" --flatpak "$SIM/bfout" --out "$SIM/bfverified"
    assert_rc "verify of the exported repository" 0
    # the default (local) build still leaves the bundle where .claude/flatpak-bg.sh expects it
    run "$SIM/bf.log" bf sh packaging/build-flatpak.sh
    assert_rc "local build" 0
    assert_file "local bundle" "$SIM/bfout/PaletoModkit-x86_64.flatpak"
    # an empty SDK pin refuses a release build
    sed 's/^SDK_COMMIT=.*/SDK_COMMIT=/' "$w/packaging/release.conf" > "$SIM/nosdk.conf"
    run "$SIM/bf.log" bf RELEASE_CONF="$SIM/nosdk.conf" EXPORT_REPO=1 sh packaging/build-flatpak.sh
    assert_rc "release build with an empty pin" 2
}

sc_sim_guard() {
    # the scripts refuse a real remote while RELEASE_SIM is set
    commit_release 1.6.0
    tag_release v1.6.0
    job_validate 1 v1.6.0 release
    assert_rc "baseline validate" 0
    d=$LIVE/runs/1
    run "$SIM/g.log" in_dir "$d/src" env REMOTE=https://github.com/the-beper/PaletoModkit.git GITHUB_OUTPUT=/dev/null sh packaging/release-validate.sh --tag v1.6.0 --mode release --event push
    assert_rc "github remote" 1
    run "$SIM/g.log" in_dir "$d/src" env GH_REPO=the-beper/PaletoModkit GITHUB_OUTPUT=/dev/null sh packaging/release-validate.sh --tag v1.6.0 --mode release --event push
    assert_rc "real repository name" 1
    run "$SIM/g.log" in_dir "$d/src" env PATH=/usr/bin:/bin GITHUB_OUTPUT=/dev/null sh packaging/release-validate.sh --tag v1.6.0 --mode release --event push
    assert_rc "no gh stub" 1
    printf '#!/bin/sh\nexit 0\n' > "$SIM/gh"
    chmod +x "$SIM/gh"
    run "$SIM/g.log" in_dir "$d/src" env GH_REPO=sim/x sh -c "PATH=$SIM:\$PATH; export PATH; sh packaging/release-validate.sh --tag v1.6.0 --mode release --event push"
    assert_rc "a gh that is not the stub" 1
    # and the stub itself refuses a real repository
    run "$SIM/g.log" env GH_REPO=the-beper/PaletoModkit gh api repos/the-beper/PaletoModkit/releases
    assert_rc "stub with the real repository" 1
}

sc_gpg_missing() {
    commit_release 1.6.0
    tag_release v1.6.0
    snapshot > "$SIM/snap.before"
    : > "$SIM/empty.sec"
    SECRET_FILE=$SIM/empty.sec
    pipeline 1 v1.6.0 release
    assert_eq "failed job" sign "$PIPE_FAIL"
    assert_rc "sign (empty secret)" 4
    assert_eq "keyrings left" 0 "$(leftover_gnupg)"
    snapshot > "$SIM/snap.after"
    cmp -s "$SIM/snap.before" "$SIM/snap.after" || fail "the remote changed"
}
sc_gpg_wrong_key() {
    commit_release 1.6.0
    tag_release v1.6.0
    SECRET_FILE=$KEYS/A.sec
    pipeline 1 v1.6.0 release
    assert_eq "failed job" sign "$PIPE_FAIL"
    assert_rc "sign (other key)" 4
    assert_log "$LIVE/runs/1/sign.log" "not $FPR_K"
    assert_eq "keyrings left" 0 "$(leftover_gnupg)"
    assert_eq "the real key is not in the log" 0 "$(grep -c 'BEGIN PGP' "$LIVE/runs/1/sign.log" || true)"
}
sc_gpg_corrupt() {
    commit_release 1.6.0
    tag_release v1.6.0
    head -c 200 "$KEYS/K.sec" > "$SIM/corrupt.sec"
    SECRET_FILE=$SIM/corrupt.sec
    pipeline 1 v1.6.0 release
    assert_eq "failed job" sign "$PIPE_FAIL"
    assert_rc "sign (corrupted secret)" 4
    assert_eq "keyrings left" 0 "$(leftover_gnupg)"
    assert_eq "work folders left" 0 "$(find "$SIM/rt" -maxdepth 2 -name 'flatpak-sign.*' 2> /dev/null | wc -l)"
}

sc_tampered_signed() {
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 1 v1.6.0 release push sign
    assert_eq "failed job" "" "$PIPE_FAIL"
    d=$LIVE/runs/1
    t=$SIM/tamper
    rm -rf "$t"
    mkdir -p "$t"
    tar -C "$t" -xf "$d/signed/flatpak-signed-repo.tar"
    # an object of the app commit: the file with the build id
    sum=$(ostree --repo="$t/repo" ls -C "$APP_REF" /files/share/build-id | awk '{ print $5 }')
    f=$t/repo/objects/$(printf '%s' "$sum" | cut -c1-2)/$(printf '%s' "$sum" | cut -c3-).filez
    [ -n "$f" ] || { fail "no file object to tamper with"; return 0; }
    [ -f "$f" ] || { fail "the object $f does not exist"; return 0; }
    chmod u+w "$f"
    printf Z | dd of="$f" bs=1 seek=$(($(stat -c %s "$f") - 3)) conv=notrunc 2> /dev/null
    # a client without the static deltas reads the objects: make it so (the deltas carry the content otherwise)
    rm -rf "$t/repo/deltas" "$t/repo/delta-indexes"
    tar -C "$t" -cf "$d/signed/flatpak-signed-repo.tar" repo
    snapshot > "$SIM/snap.before"
    job_publish 1 release
    assert_rc "publish with a changed signed tar" 2
    assert_log "$d/publish.log" "recorded hash"
    # an attacker who also fixes the recorded hash is stopped by the pin of flatpak-signed.txt (a job output)
    sed -i "s/^repo_tar_sha256=.*/repo_tar_sha256=$(sha256sum "$d/signed/flatpak-signed-repo.tar" | cut -d' ' -f1)/" "$d/signed/flatpak-signed.txt"
    job_publish 1 release
    assert_rc "publish with the recorded hash fixed in the file" 2
    assert_log "$d/publish.log" "pinned"
    # and, if he could change the pin as well, by the pull
    sed -i "s/^signed_sha256=.*/signed_sha256=$(sha256sum "$d/signed/flatpak-signed.txt" | cut -d' ' -f1)/" "$d/finish.gho"
    job_publish 1 release
    assert_rc "publish with the hash fixed" 2
    assert_log "$d/publish.log" "does not verify"
    snapshot > "$SIM/snap.after"
    cmp -s "$SIM/snap.before" "$SIM/snap.after" || fail "the remote changed"
}

sc_interrupted_upload() {
    commit_release 1.6.0
    tag_release v1.6.0
    before=$(git -C "$LIVE/origin.git" rev-parse gh-pages)
    echo upload:2 > "$GH_STUB_DIR/fail"
    pipeline 1 v1.6.0 release
    assert_eq "failed job" publish "$PIPE_FAIL"
    [ "$RC" != 0 ] || fail "the interrupted upload did not fail"
    assert_eq "still a draft" true "$(stub_field v1.6.0 draft)"
    assert_eq "gh-pages untouched" "$before" "$(git -C "$LIVE/origin.git" rev-parse gh-pages)"
    job_publish 1 release
    assert_rc "rerun of publish" 0
    assert_eq "assets" 5 "$(stub_assets v1.6.0)"
    assert_eq "draft" false "$(stub_field v1.6.0 draft)"
    assert_eq "gh-pages commits" 1 "$(pages_count)"
    assert_eq "one release" 1 "$(grep -lx v1.6.0 "$GH_STUB_DIR"/rel/*/tag | wc -l)"
}
# The list of releases is seconds behind a create and a delete: the job waits for it (run 1 gets as far as the
# upload, run 2 replaces that draft because it is another build) and ends with one published release.
sc_listing_lag() {
    commit_release 1.6.0
    tag_release v1.6.0
    echo 2 > "$GH_STUB_DIR/lag"
    echo upload:2 > "$GH_STUB_DIR/fail"
    pipeline 1 v1.6.0 release
    [ "$RC" != 0 ] || fail "the interrupted upload did not fail"
    [ ! -s "$GH_STUB_DIR/fail" ] || fail "run 1 did not get as far as the upload"
    pipeline 2 v1.6.0 release
    assert_rc "run 2" 0
    assert_log "$LIVE/runs/2/publish.log" 'replacing it'
    assert_eq "one release" 1 "$(grep -lx v1.6.0 "$GH_STUB_DIR"/rel/*/tag | wc -l)"
    assert_eq "draft" false "$(stub_field v1.6.0 draft)"
    assert_eq "assets" 5 "$(stub_assets v1.6.0)"
}
sc_interrupted_edit() {
    commit_release 1.6.0
    tag_release v1.6.0
    echo edit > "$GH_STUB_DIR/fail"
    pipeline 1 v1.6.0 release
    assert_eq "failed job" publish "$PIPE_FAIL"
    [ "$RC" != 0 ] || fail "the failed edit did not fail"
    assert_eq "gh-pages already at the tag" v1.6.0 "$(kv "$LIVE/site/release.txt" tag)"
    assert_eq "still a draft" true "$(stub_field v1.6.0 draft)"
    job_publish 1 release
    assert_rc "rerun of publish" 0
    assert_eq "draft" false "$(stub_field v1.6.0 draft)"
    assert_eq "gh-pages commits" 1 "$(pages_count)"
}
sc_interrupted_push() {
    commit_release 1.6.0
    tag_release v1.6.0
    touch "$LIVE/reject-once"
    pipeline 1 v1.6.0 release
    assert_eq "failed job" publish "$PIPE_FAIL"
    assert_rc "publish (push rejected)" 5
    assert_eq "gh-pages still at the old tag" v1.5.1 "$(kv "$LIVE/site/release.txt" tag)"
    job_publish 1 release
    assert_rc "rerun of publish" 0
    assert_eq "gh-pages tag" v1.6.0 "$(kv "$LIVE/site/release.txt" tag)"
    assert_eq "gh-pages commits" 1 "$(pages_count)"
    assert_eq "draft" false "$(stub_field v1.6.0 draft)"
}
sc_pages_wait() {
    commit_release 1.6.0
    tag_release v1.6.0
    touch "$LIVE/nodeploy"
    pipeline 1 v1.6.0 release
    assert_eq "failed job" publish "$PIPE_FAIL"
    assert_rc "publish (Pages never deploys)" 5
    assert_eq "still a draft" true "$(stub_field v1.6.0 draft)"
    rm "$LIVE/nodeploy"
    git -C "$LIVE/origin.git" update-ref refs/heads/gh-pages "$(git -C "$LIVE/origin.git" rev-parse gh-pages)"
    rm -rf "$LIVE/site"
    mkdir -p "$LIVE/site"
    git -C "$LIVE/origin.git" archive refs/heads/gh-pages | tar -x -C "$LIVE/site"
    job_publish 1 release
    assert_rc "rerun of publish" 0
    assert_eq "draft" false "$(stub_field v1.6.0 draft)"
}

sc_rerun_same_tag() {
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 1 v1.6.0 release
    assert_rc "first run" 0
    snapshot > "$SIM/snap.before"
    pipeline 2 v1.6.0 release
    assert_rc "second run" 0
    assert_eq "plan" noop "$PIPE_PLAN"
    [ ! -d "$LIVE/runs/2/build" ] || fail "the second run built something"
    snapshot > "$SIM/snap.after"
    cmp -s "$SIM/snap.before" "$SIM/snap.after" || fail "refs or assets changed"
    # manual run of the same tag
    pipeline 3 v1.6.0 release workflow_dispatch
    assert_eq "plan (dispatch)" noop "$PIPE_PLAN"
}

sc_same_tag_new_content() {
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 1 v1.6.0 release push sign
    assert_eq "run 1 failed job" "" "$PIPE_FAIL"
    pipeline 2 v1.6.0 release
    assert_rc "run 2" 0
    snapshot > "$SIM/snap.before"
    job_publish 1 release
    assert_rc "publish of the first run, after the second published" 3
    snapshot > "$SIM/snap.after"
    cmp -s "$SIM/snap.before" "$SIM/snap.after" || fail "the remote changed"
    # and a tag that points to a new commit with the same version
    git -C "$LIVE/work" commit -q --allow-empty -m "same version, other commit"
    git -C "$LIVE/work" push -q origin HEAD:main
    tag_release v1.6.0
    job_validate 3 v1.6.0 release
    assert_rc "validate of the moved tag" 3
}

sc_downgrade() {
    commit_release 1.5.2
    tag_release v1.5.2
    pipeline 1 v1.5.2 release push sign
    assert_eq "v1.5.2 before 1.6.0" "" "$PIPE_FAIL"
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 2 v1.6.0 release
    assert_rc "1.6.0" 0
    job_validate 3 v1.5.2 release
    assert_rc "validate v1.5.2 after 1.6.0" 3
    snapshot > "$SIM/snap.before"
    job_publish 1 release
    assert_rc "publish v1.5.2 after 1.6.0" 3
    snapshot > "$SIM/snap.after"
    cmp -s "$SIM/snap.before" "$SIM/snap.after" || fail "the remote changed"
    job_validate 4 v1.5.1 release workflow_dispatch
    assert_rc "dispatch of v1.5.1" 3
}

sc_concurrent() {
    commit_release 1.6.0
    tag_release v1.6.0
    commit_release 1.6.1
    tag_release v1.6.1
    pipeline 1 v1.6.0 release push sign
    assert_eq "run 1" "" "$PIPE_FAIL"
    pipeline 2 v1.6.1 release push sign
    assert_eq "run 2" "" "$PIPE_FAIL"
    job_publish 1 release
    assert_rc "publish 1.6.0" 0
    snapshot > "$SIM/snap.before"
    job_publish 2 release
    assert_rc "publish 1.6.1 on the old base" 5
    snapshot > "$SIM/snap.after"
    cmp -s "$SIM/snap.before" "$SIM/snap.after" || fail "the remote changed"
    pipeline 3 v1.6.1 release
    assert_rc "rerun of 1.6.1" 0
    assert_eq "failed job" "" "$PIPE_FAIL"
    assert_eq "pages tag" v1.6.1 "$(kv "$LIVE/site/release.txt" tag)"
    assert_eq "app parent is 1.6.0" "$(record gh-pages-previous | sed -n 's/^ostree_app_commit=//p')" "$(ostree --repo="$LIVE/site/repo" rev-parse "$APP_REF^")"
    assert_eq "gh-pages commits" 1 "$(pages_count)"
    pipeline 4 v1.6.0 release
    assert_rc "rerun of 1.6.0 after 1.6.1" 3
}

sc_gh_pages_missing() {
    git -C "$LIVE/origin.git" update-ref -d refs/heads/gh-pages
    git -C "$LIVE/origin.git" update-ref -d refs/heads/gh-pages-previous
    # the very first publication: no release yet
    rm -rf "$GH_STUB_DIR/rel"
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 1 v1.6.0 release
    assert_eq "failed job" validate "$PIPE_FAIL"
    assert_rc "validate without gh-pages" 5
    BOOTSTRAP=true
    pipeline 2 v1.6.0 release
    BOOTSTRAP=false
    assert_rc "bootstrap run" 0
    assert_eq "failed job" "" "$PIPE_FAIL"
    assert_eq "gh-pages tag" v1.6.0 "$(kv "$LIVE/site/release.txt" tag)"
    assert_eq "gh-pages commits" 1 "$(pages_count)"
    # bootstrap with a gh-pages that exists is refused
    commit_release 1.6.1
    tag_release v1.6.1
    BOOTSTRAP=true
    pipeline 3 v1.6.1 release
    BOOTSTRAP=false
    assert_rc "bootstrap on an existing gh-pages" 5
    # both branches gone with a release published: a new version does not start a new chain, the highest release rebuilds gh-pages
    git -C "$LIVE/origin.git" update-ref -d refs/heads/gh-pages
    git -C "$LIVE/origin.git" update-ref -d refs/heads/gh-pages-previous
    BOOTSTRAP=true
    pipeline 4 v1.6.1 release
    assert_rc "bootstrap of a new version when 1.6.0 is published" 5
    assert_eq "failed job" validate "$PIPE_FAIL"
    assert_log "$LIVE/runs/4/validate.log" "v1.6.0 is published"
    pipeline 5 v1.6.0 publish-flatpak workflow_dispatch
    BOOTSTRAP=false
    assert_rc "bootstrap by publish-flatpak of the highest release" 0
    assert_eq "failed job" "" "$PIPE_FAIL"
    assert_eq "gh-pages tag" v1.6.0 "$(kv "$LIVE/site/release.txt" tag)"
    assert_eq "gh-pages commits" 1 "$(pages_count)"
}

sc_another_writer() {
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 1 v1.6.0 release push sign
    assert_eq "run 1" "" "$PIPE_FAIL"
    g=$SIM/other
    rm -rf "$g"
    git clone -q --branch gh-pages "$REMOTE" "$g"
    echo hello > "$g/other.txt"
    git -C "$g" add other.txt
    git -C "$g" commit -q -m "someone else"
    git -C "$g" push -q origin gh-pages
    snapshot > "$SIM/snap.before"
    job_publish 1 release
    assert_rc "publish after another writer" 5
    snapshot > "$SIM/snap.after"
    cmp -s "$SIM/snap.before" "$SIM/snap.after" || fail "something was written"
}

sc_tag_moved() {
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 1 v1.6.0 release push sign
    assert_eq "run 1" "" "$PIPE_FAIL"
    git -C "$LIVE/work" commit -q --allow-empty -m "another commit"
    git -C "$LIVE/work" push -q origin HEAD:main
    tag_release v1.6.0
    snapshot > "$SIM/snap.before"
    job_publish 1 release
    assert_rc "publish after the tag moved" 5
    snapshot > "$SIM/snap.after"
    cmp -s "$SIM/snap.before" "$SIM/snap.after" || fail "something was written"
}

sc_legacy_record() {
    g=$SIM/legacy
    rm -rf "$g"
    git clone -q --branch gh-pages "$REMOTE" "$g"
    git -C "$g" rm -q release.txt
    git -C "$g" commit -q --amend -m "Flatpak repository for v1.5.1"
    git -C "$g" push -q -f origin gh-pages
    git -C "$LIVE/origin.git" show gh-pages:release.txt > /dev/null 2>&1 && fail "the legacy gh-pages still has release.txt"
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 1 v1.6.0 release
    assert_rc "pipeline over a legacy gh-pages" 0
    assert_eq "failed job" "" "$PIPE_FAIL"
    assert_eq "gh-pages tag" v1.6.0 "$(kv "$LIVE/site/release.txt" tag)"
    assert_eq "gh-pages commits" 1 "$(pages_count)"
    # the legacy state is also where the same tag is refused: v1.5.1 has no known source commit
    git -C "$g" fetch -q origin
    job_validate 2 v1.5.1 release workflow_dispatch
    assert_rc "dispatch of v1.5.1" 3
}

sc_publish_flatpak() {
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 1 v1.6.0 release
    assert_rc "release" 0
    # not the highest: refused
    job_validate 2 v1.5.1 publish-flatpak workflow_dispatch
    assert_rc "publish-flatpak of v1.5.1" 3
    # the highest, Pages already there: nothing to do
    job_validate 3 v1.6.0 publish-flatpak workflow_dispatch
    assert_rc "publish-flatpak of v1.6.0 (up to date)" 0
    assert_eq "plan" noop "$(kv "$LIVE/runs/3/validate.out" plan)"
    # recovery: Pages back at 1.5.1, then publish-flatpak puts 1.6.0 on it again, the release untouched
    git -C "$LIVE/origin.git" update-ref refs/heads/gh-pages "$(git -C "$LIVE/origin.git" rev-parse gh-pages-previous)"
    git -C "$LIVE/origin.git" update-ref -d refs/heads/gh-pages-previous
    rm -rf "$LIVE/site"
    mkdir -p "$LIVE/site"
    git -C "$LIVE/origin.git" archive refs/heads/gh-pages | tar -x -C "$LIVE/site"
    sums=$(sha256sum "$GH_STUB_DIR/rel/$(stub_rel v1.6.0)/assets/SHA256SUMS" | cut -d' ' -f1)
    snapshot_stub=$(cd "$GH_STUB_DIR" && find . -type f -not -name log | LC_ALL=C sort | xargs sha256sum)
    pipeline 4 v1.6.0 publish-flatpak workflow_dispatch
    assert_rc "publish-flatpak run" 0
    assert_eq "failed job" "" "$PIPE_FAIL"
    assert_eq "gh-pages tag" v1.6.0 "$(kv "$LIVE/site/release.txt" tag)"
    assert_eq "recorded sums are the release's" "$sums" "$(kv "$LIVE/site/release.txt" sums_sha256)"
    assert_eq "the release was not touched" "$snapshot_stub" "$(cd "$GH_STUB_DIR" && find . -type f -not -name log | LC_ALL=C sort | xargs sha256sum)"
}

# rollback_pages: gh-pages is its previous state again (the version before the last release), the site follows
rollback_pages() {
    git -C "$LIVE/origin.git" update-ref refs/heads/gh-pages "$(git -C "$LIVE/origin.git" rev-parse gh-pages-previous)"
    git -C "$LIVE/origin.git" update-ref -d refs/heads/gh-pages-previous
    rm -rf "$LIVE/site"
    mkdir -p "$LIVE/site"
    git -C "$LIVE/origin.git" archive refs/heads/gh-pages | tar -x -C "$LIVE/site"
}

# The record of a skip is made by jobs that read untrusted data. With gh-pages not at the tag, publish-flatpak would say "nothing
# to do" and end green: both the signing job (--check-prepared) and the publish job check the record against gh-pages.
forge_skip() { sed -i 's/^skip=.*/skip=1/' "$1/flatpak-prepared.txt"; }
sc_forged_skip() {
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 1 v1.6.0 release
    assert_rc "release" 0
    rollback_pages
    pipeline 2 v1.6.0 publish-flatpak workflow_dispatch sign
    assert_eq "failed job" "" "$PIPE_FAIL"
    d=$LIVE/runs/2
    assert_eq "skip of the honest run" 0 "$(kv "$d/signed/flatpak-signed.txt" skip)"
    # the publish job: a record of a skip that fits its pin
    sed -i 's/^skip=.*/skip=1/' "$d/signed/flatpak-signed.txt"
    sed -i "s/^signed_sha256=.*/signed_sha256=$(sha256sum "$d/signed/flatpak-signed.txt" | cut -d' ' -f1)/" "$d/finish.gho"
    snapshot > "$SIM/snap.before"
    job_publish 2 publish-flatpak
    assert_rc "publish of a forged skip" 5
    assert_log "$d/publish.log" "nothing was published"
    snapshot > "$SIM/snap.after"
    cmp -s "$SIM/snap.before" "$SIM/snap.after" || fail "something was written"
    assert_eq "gh-pages tag" v1.5.1 "$(kv "$LIVE/site/release.txt" tag)"
    # the signing job: the description of the preparation says skip, gh-pages does not
    PREPARED_HOOK=forge_skip
    job_sign 2
    PREPARED_HOOK=
    assert_rc "check of a forged skip" 5
    assert_log "$d/sign.log" "gh-pages has"
}

# gh-pages rolled back to an older state, signed and valid: a new version is not built on it. The release list says which
# version gh-pages must at least have; the repair is publish-flatpak for the highest published tag.
sc_replay_pages() {
    commit_release 1.6.0
    tag_release v1.6.0
    commit_release 1.6.1
    tag_release v1.6.1
    pipeline 1 v1.6.0 release
    assert_rc "release 1.6.0" 0
    pipeline 2 v1.6.1 release push sign
    assert_eq "failed job" "" "$PIPE_FAIL"
    rollback_pages
    assert_eq "gh-pages tag" v1.5.1 "$(kv "$LIVE/site/release.txt" tag)"
    job_validate 3 v1.6.1 release
    assert_rc "validate of 1.6.1 over a gh-pages with 1.5.1 when 1.6.0 is published" 5
    assert_log "$LIVE/runs/3/validate.log" "publish-flatpak for v1.6.0"
    snapshot > "$SIM/snap.before"
    job_publish 2 release
    assert_rc "publish of 1.6.1 over a gh-pages with 1.5.1" 5
    assert_log "$LIVE/runs/2/publish.log" "publish-flatpak for v1.6.0"
    snapshot > "$SIM/snap.after"
    cmp -s "$SIM/snap.before" "$SIM/snap.after" || fail "something was written"
    # a release candidate has no Pages
    commit_release 1.7.0-rc.1
    tag_release v1.7.0-rc.1
    job_validate 4 v1.7.0-rc.1 release
    assert_rc "validate of a release candidate" 0
    # the repair, then the release goes on
    pipeline 5 v1.6.0 publish-flatpak workflow_dispatch
    assert_rc "publish-flatpak of 1.6.0" 0
    assert_eq "failed job" "" "$PIPE_FAIL"
    assert_eq "gh-pages tag" v1.6.0 "$(kv "$LIVE/site/release.txt" tag)"
    job_validate 6 v1.6.1 release
    assert_rc "validate of 1.6.1 after the repair" 0
    # a release made by hand (published, without the files) was never on Pages: it is no reason to refuse the next version...
    d=$GH_STUB_DIR/rel/9001
    mkdir -p "$d"
    echo v1.6.1 > "$d/tag"
    echo false > "$d/draft"
    echo false > "$d/prerelease"
    commit_release 1.6.2
    tag_release v1.6.2
    job_validate 7 v1.6.2 release
    assert_rc "validate of 1.6.2 when the highest release has no files and gh-pages has 1.6.0" 0
    # ...and no way to lift the check: the anchor is the highest release that has them (1.6.0), not the highest
    rollback_pages
    job_validate 8 v1.6.2 release
    assert_rc "validate of 1.6.2 over a gh-pages with 1.5.1, 1.6.0 published and a release of 1.6.1 without files" 5
    assert_log "$LIVE/runs/8/validate.log" "publish-flatpak for v1.6.0"
}

sc_sign_fresh_and_verify() {
    # the signing script on its own: a fresh repository, then the bundle
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 1 v1.6.0 release push sign
    d=$LIVE/runs/1
    for f in flatpak-signed-repo.tar PaletoModkit-x86_64.flatpak flatpak-pubkey.gpg flatpak-signed.txt; do assert_file "signed output" "$d/signed/$f"; done
    assert_eq "key fingerprint" "$FPR_K" "$(kv "$d/signed/flatpak-signed.txt" key_fpr)"
    # the same tag and commit again, signed on the published state: skip, nothing signed
    pipeline 2 v1.6.0 release
    assert_rc "publish" 0
    job_validate 3 v1.6.0 release
    assert_eq "plan" noop "$(kv "$LIVE/runs/3/validate.out" plan)"
    # a skipped signing: run 4 was validated before the release, signs after it
    d=$LIVE/runs/1
    rm -rf "$d/signed"
    job_sign 1
    assert_rc "sign again on the published state" 0
    assert_eq "skip" 1 "$(kv "$d/signed/flatpak-signed.txt" skip)"
    [ ! -e "$d/signed/flatpak-signed-repo.tar" ] || fail "something was signed on skip"
}

# ---- hardening of the signing and of the checks of gh-pages -----------------------------------------------------

# edit_pages MESSAGE CMD...: a commit on gh-pages by someone else; CMD runs in a clone of it.
edit_pages() {
    msg=$1
    shift
    g=$SIM/other
    rm -rf "$g"
    git clone -q --branch gh-pages "$REMOTE" "$g"
    (cd "$g" && "$@")
    git -C "$g" add -A
    git -C "$g" commit -q -m "$msg"
    git -C "$g" push -q origin gh-pages
}
mut_two_keys() { sed -i "s|^GPGKey=.*|GPGKey=$(cat "$KEYS/K.pub" "$KEYS/A.pub" | base64 -w0)|" paleto-modkit.flatpakrepo; }
mut_other_key() { sed -i "s|^GPGKey=.*|GPGKey=$(base64 -w0 "$KEYS/A.pub")|" paleto-modkit.flatpakrepo; }
mut_record_no_tag() { echo "format=1" > release.txt; }
mut_record_dir() { rm -f release.txt; mkdir release.txt; echo x > release.txt/a; }
mut_symlink() { ln -s /etc/passwd evil-link; }

# a second key in the published .flatpakrepo (or another one instead): the signing job refuses, nothing is signed
sc_pages_second_key() {
    commit_release 1.6.0
    tag_release v1.6.0
    edit_pages "second key" mut_two_keys
    pipeline 1 v1.6.0 release
    assert_eq "failed job" sign "$PIPE_FAIL"
    assert_rc "sign with two keys in the published file" 3
    assert_log "$LIVE/runs/1/sign.log" "exactly the key"
    [ ! -e "$LIVE/runs/1/signed/PaletoModkit-x86_64.flatpak" ] || fail "a bundle was made"
    assert_eq "keyrings left" 0 "$(leftover_gnupg)"
}
sc_pages_other_key() {
    commit_release 1.6.0
    tag_release v1.6.0
    edit_pages "another key" mut_other_key
    pipeline 1 v1.6.0 release
    assert_eq "failed job" sign "$PIPE_FAIL"
    assert_rc "sign with another published key" 3
}

# a release.txt that is there and says nothing usable, or is not a file: exit 5, never "no limit"
sc_pages_record_unreadable() {
    commit_release 1.6.0
    tag_release v1.6.0
    edit_pages "record without a tag" mut_record_no_tag
    job_validate 1 v1.6.0 release
    assert_rc "validate with a record without a tag" 5
    edit_pages "record is a folder" mut_record_dir
    job_validate 1 v1.6.0 release
    assert_rc "validate with a record that is a folder" 5
}

# a symbolic link in gh-pages: nothing is read or copied
sc_pages_symlink() {
    commit_release 1.6.0
    tag_release v1.6.0
    edit_pages "a link" mut_symlink
    pipeline 1 v1.6.0 release push verify
    assert_eq "failed job" "" "$PIPE_FAIL"
    job_sign 1
    assert_rc "fetch of gh-pages with a link" 2
    assert_log "$LIVE/runs/1/pages.log" "symbolic link"
    [ ! -e "$SIM/rt/1/pages" ] || fail "the clone was left behind"
}

# the secret in the environment of the signing job: only --sign accepts it, and none of its child processes sees it
sc_sign_secret_env() {
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 1 v1.6.0 release push verify
    assert_eq "failed job" "" "$PIPE_FAIL"
    d=$LIVE/runs/1
    tag=v1.6.0
    commit=$(kv "$d/validate.out" commit)
    rt=$SIM/rt/iso
    mkdir -p "$rt/shim"
    for t in flatpak gpg ostree; do
        real=$(command -v "$t")
        printf '#!/bin/sh\necho "%s $*" >> "%s/calls"\nprintenv FLATPAK_GPG_PRIVATE_KEY >> "%s/seen" || true\nexec %s "$@"\n' "$t" "$rt" "$rt" "$real" > "$rt/shim/$t"
        chmod +x "$rt/shim/$t"
    done
    : > "$rt/seen"
    : > "$rt/calls"
    run "$d/pages.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/release-pages.sh fetch "$rt/pages"
    assert_rc "fetch" 0
    run "$d/pages2.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/release-pages.sh fetch "$rt/pages2"
    assert_rc "fetch again" 0
    m=$d/verified/release-manifest.txt
    args="--tag $tag --commit $commit --manifest $m --unsigned $d/fp/flatpak-unsigned-repo.tar --site $rt/pages --work $rt/pw --out $d/prepared"
    cargs="--tag $tag --commit $commit --manifest $m --in $d/prepared --work $rt/w --site $rt/pages2 --check-remote"
    sargs="--tag $tag --commit $commit --work $rt/w --out $d/signres"
    fargs="--tag $tag --commit $commit --manifest $m --in $d/signres --work $rt/fw --out $d/signed"
    secret=$(cat "$KEYS/K.sec")
    # shellcheck disable=SC2086
    run "$d/p0.log" in_dir "$d/build" env RUNNER_TEMP="$rt" FLATPAK_GPG_PRIVATE_KEY="$secret" sh packaging/flatpak-sign.sh --prepare $args
    assert_rc "prepare with the secret" 1
    assert_log "$d/p0.log" "without the secret"
    # shellcheck disable=SC2086
    run "$d/p1.log" in_dir "$d/build" env RUNNER_TEMP="$rt" PATH="$rt/shim:$PATH" sh packaging/flatpak-sign.sh --prepare $args
    assert_rc "prepare" 0
    # shellcheck disable=SC2086
    run "$d/p1b.log" in_dir "$d/build" env RUNNER_TEMP="$rt" FLATPAK_GPG_PRIVATE_KEY="$secret" sh packaging/flatpak-sign.sh --check-prepared $cargs
    assert_rc "check-prepared with the secret" 1
    # shellcheck disable=SC2086
    run "$d/p1c.log" in_dir "$d/build" env RUNNER_TEMP="$rt" PATH="$rt/shim:$PATH" sh packaging/flatpak-sign.sh --check-prepared $cargs
    assert_rc "check-prepared" 0
    : > "$rt/seen"
    : > "$rt/calls"
    # shellcheck disable=SC2086
    run "$d/p2.log" in_dir "$d/build" env RUNNER_TEMP="$rt" GITHUB_OUTPUT="$d/p2.gho" PATH="$rt/shim:$PATH" FLATPAK_GPG_PRIVATE_KEY="$secret" sh packaging/flatpak-sign.sh --sign $sargs
    assert_rc "sign" 0
    assert_eq "the secret seen by a child process of --sign" 0 "$(wc -c < "$rt/seen")"
    # the sign step unpacks nothing and makes no commit, no appstream data and no delta: it signs
    if grep -q -e build-commit-from -e 'untrusted' "$rt/calls"; then fail "the sign step made commits from the build output"; fi
    grep 'build-update-repo' "$rt/calls" | grep -q -e '--no-update-appstream' || fail "the sign step generated appstream data"
    if grep -q -e 'generate-static-deltas' "$rt/calls"; then fail "the sign step generated static deltas"; fi
    grep -q 'gpg-sign' "$rt/calls" || fail "the sign step did not sign with ostree gpg-sign"
    for t in flatpak gpg ostree; do
        [ "$(grep -c "^$t " "$rt/calls")" -gt 0 ] || fail "the shim of $t was never called: the check proves nothing"
    done
    assert_eq "keyrings left" 0 "$(leftover_gnupg)"
    # shellcheck disable=SC2086
    run "$d/p3.log" in_dir "$d/build" env RUNNER_TEMP="$rt" FLATPAK_GPG_PRIVATE_KEY="$secret" sh packaging/flatpak-sign.sh --finish $fargs
    assert_rc "finish with the secret" 1
    # shellcheck disable=SC2086
    run "$d/p4.log" in_dir "$d/build" env RUNNER_TEMP="$rt" RESULT_SHA256="$(kv "$d/p2.gho" result_sha256)" PATH="$rt/shim:$PATH" sh packaging/flatpak-sign.sh --finish $fargs
    assert_rc "finish" 0
    assert_eq "keyrings left" 0 "$(leftover_gnupg)"
}

# two keys in the secret: not the pinned one alone
sc_gpg_two_keys() {
    commit_release 1.6.0
    tag_release v1.6.0
    cat "$KEYS/K.sec" "$KEYS/A.sec" > "$SIM/two.sec"
    SECRET_FILE=$SIM/two.sec
    pipeline 1 v1.6.0 release
    assert_eq "failed job" sign "$PIPE_FAIL"
    assert_rc "sign (two keys in the secret)" 4
    assert_eq "keyrings left" 0 "$(leftover_gnupg)"
}

# the published key file holds the same key in other bytes (an added user id): the files users have are kept
# byte for byte, and the release goes through
sc_pages_key_bytes() {
    h=$SIM/keyvar
    rm -rf "$h"
    mkdir -p "$h"
    chmod 700 "$h"
    gpg --homedir "$h" --batch --quiet --import "$KEYS/K.sec" > /dev/null 2>&1
    gpg --homedir "$h" --batch --quiet --quick-add-uid "$FPR_K" "Extra user id" > /dev/null 2>&1
    gpg --homedir "$h" --batch --export "$FPR_K" > "$SIM/K2.pub" 2> /dev/null
    gpgconf --homedir "$h" --kill all > /dev/null 2>&1 || true
    if cmp -s "$KEYS/K.pub" "$SIM/K2.pub"; then fail "the variant of the key has the same bytes"; fi
    commit_release 1.6.0
    tag_release v1.6.0
    edit_pages "same key, other bytes" mut_variant_key
    pipeline 1 v1.6.0 release
    assert_rc "pipeline" 0
    assert_eq "failed job" "" "$PIPE_FAIL"
    cmp -s "$SIM/other/paleto-modkit.flatpakrepo" "$LIVE/site/paleto-modkit.flatpakrepo" || fail "the .flatpakrepo has other bytes than the published one"
    cmp -s "$SIM/other/paleto-modkit.flatpakref" "$LIVE/site/paleto-modkit.flatpakref" || fail "the .flatpakref has other bytes than the published one"
    assert_eq "gh-pages tag" v1.6.0 "$(kv "$LIVE/site/release.txt" tag)"
}
mut_variant_key() { k=$(base64 -w0 "$SIM/K2.pub"); sed -i "s|^GPGKey=.*|GPGKey=$k|" paleto-modkit.flatpakrepo paleto-modkit.flatpakref; }
mut_other_file() { echo hello > other.txt; }

# what the signing job checks before it signs: the hash of the prepared repository, the tag, the base of gh-pages, and
# the content against the manifest of the verify job (a prepare job that lies about everything it wrote)
sc_sign_prepared_checks() {
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 1 v1.6.0 release push verify
    assert_eq "failed job" "" "$PIPE_FAIL"
    d=$LIVE/runs/1
    tag=v1.6.0
    commit=$(kv "$d/validate.out" commit)
    rt=$SIM/rt/pc
    mkdir -p "$rt"
    secret=$(cat "$KEYS/K.sec")
    m=$d/verified/release-manifest.txt
    run "$d/pages.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/release-pages.sh fetch "$rt/pages"
    assert_rc "fetch" 0
    run "$d/pages2.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/release-pages.sh fetch "$rt/pages2"
    assert_rc "fetch again" 0
    # shellcheck disable=SC2086
    run "$d/q1.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/flatpak-sign.sh --prepare --tag $tag --commit $commit --manifest "$m" \
        --unsigned "$d/fp/flatpak-unsigned-repo.tar" --site "$rt/pages" --work "$rt/pw" --out "$d/prepared"
    assert_rc "prepare" 0
    for f in flatpak-prepared-repo.tar flatpak-prepared.txt; do assert_file "prepared output" "$d/prepared/$f"; done
    # shellcheck disable=SC2329
    chk_it() { # IN TAG [OPTION]
        in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/flatpak-sign.sh --check-prepared --tag "$2" --commit "$commit" --manifest "$m" --in "$1" --work "$rt/w" --site "$rt/pages2" ${3:+"$3"}
    }
    # shellcheck disable=SC2329
    sign_it() {
        in_dir "$d/build" env RUNNER_TEMP="$rt" FLATPAK_GPG_PRIVATE_KEY="$secret" sh packaging/flatpak-sign.sh --sign --tag "$tag" --commit "$commit" --work "$rt/w" --out "$d/signres"
    }
    # a prepared repository that changed after it was described
    rm -rf "$d/prepared.bad"
    cp -a "$d/prepared" "$d/prepared.bad"
    printf Z >> "$d/prepared.bad/flatpak-prepared-repo.tar"
    run "$d/q2.log" chk_it "$d/prepared.bad" "$tag"
    assert_rc "check with a changed prepared repository" 2
    assert_log "$d/q2.log" "not the one that was described"
    # another tag than the one prepared
    run "$d/q3.log" chk_it "$d/prepared" v1.6.1
    assert_rc "check for another tag" 2
    # a prepare job that lies about everything: another build of the same commit, prepared consistently, its
    # description edited to name the verified build. Only the content checksum against the manifest can tell.
    fx_flatpak "$d/build" "$d/fp2"
    run "$d/q5.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/release-verify.sh builds --tag "$tag" --commit "$commit" --flatpak "$d/fp2" --out "$d/verified2"
    assert_rc "verify the other build" 0
    # shellcheck disable=SC2086
    run "$d/q6.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/flatpak-sign.sh --prepare --tag $tag --commit $commit --manifest "$d/verified2/release-manifest.txt" \
        --unsigned "$d/fp2/flatpak-unsigned-repo.tar" --site "$rt/pages" --work "$rt/pw2" --out "$d/prepared2"
    assert_rc "prepare the other build" 0
    sed -i "s/^unsigned_tar_sha256=.*/unsigned_tar_sha256=$(kv "$m" flatpak_unsigned_tar_sha256)/" "$d/prepared2/flatpak-prepared.txt"
    run "$d/q7.log" chk_it "$d/prepared2" "$tag"
    assert_rc "check of a prepared repository of another build" 2
    assert_log "$d/q7.log" "not the verified build"
    # the sign step does not accept a work folder that --check-prepared did not make
    rm -rf "$rt/w"
    mkdir -p "$rt/w"
    run "$d/q8.log" sign_it
    assert_rc "sign without a check" 1
    # gh-pages moved since the preparation
    edit_pages "someone else" mut_other_file
    run "$d/q4.log" chk_it "$d/prepared" "$tag" --check-remote
    assert_rc "check after gh-pages moved" 5
    # a ref that points to another commit after the check: not signed
    run "$d/q9.log" chk_it "$d/prepared" "$tag"
    assert_rc "check" 0
    par=$(ostree --repo="$rt/w/repo" rev-parse "$APP_REF^")
    printf '%s\n' "$par" > "$rt/w/repo/refs/heads/$APP_REF"
    assert_eq "the ref was moved" "$par" "$(ostree --repo="$rt/w/repo" rev-parse "$APP_REF")"
    run "$d/q10.log" sign_it
    assert_rc "sign after a ref changed" 2
    assert_log "$d/q10.log" "not the commit that was checked"
    assert_eq "keyrings left" 0 "$(leftover_gnupg)"
}

# the config of the published repository is not signed: [flatpak] keys in it (redirect-url, gpg-keys, title) would end up in
# the summary the key signs, and move every client to another URL and another key. The preparation refuses it.
mut_hostile_config() { printf '[flatpak]\nredirect-url=https://evil.example/repo\ngpg-keys=%s\ntitle=Evil\n' "$(base64 -w0 "$KEYS/A.pub")" >> repo/config; }
sc_pages_hostile_config() {
    commit_release 1.6.0
    tag_release v1.6.0
    edit_pages "a hostile repository config" mut_hostile_config
    pipeline 1 v1.6.0 release
    assert_eq "failed job" sign "$PIPE_FAIL"
    assert_rc "sign over a repository with a hostile config" 2
    assert_log "$LIVE/runs/1/sign.log" "config"
    [ ! -e "$LIVE/runs/1/signed/PaletoModkit-x86_64.flatpak" ] || fail "a bundle was made"
    assert_eq "keyrings left" 0 "$(leftover_gnupg)"
}

# a compromised prepare job makes any repository it likes and describes it consistently (hashes, commits). The signing job
# fetches gh-pages again and compares, before the key exists: mode, config, parents, bindings, metadata keys, dates.
sc_sign_hostile_prepared() {
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 1 v1.6.0 release push verify
    assert_eq "failed job" "" "$PIPE_FAIL"
    d=$LIVE/runs/1
    tag=v1.6.0
    commit=$(kv "$d/validate.out" commit)
    rt=$SIM/rt/hx
    mkdir -p "$rt"
    m=$d/verified/release-manifest.txt
    run "$d/pages.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/release-pages.sh fetch "$rt/pages"
    assert_rc "fetch" 0
    run "$d/pages2.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/release-pages.sh fetch "$rt/pages2"
    assert_rc "fetch again" 0
    run "$d/h1.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/flatpak-sign.sh --prepare --tag "$tag" --commit "$commit" --manifest "$m" \
        --unsigned "$d/fp/flatpak-unsigned-repo.tar" --site "$rt/pages" --work "$rt/pw" --out "$d/prepared"
    assert_rc "prepare" 0
    run "$d/h2.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/flatpak-sign.sh --prepare --tag "$tag" --commit "$commit" --manifest "$m" \
        --unsigned "$d/fp/flatpak-unsigned-repo.tar" --fresh --work "$rt/pwf" --out "$d/preparedf"
    assert_rc "prepare --fresh" 0
    base_app=$(ostree --repo="$rt/pages/repo" rev-parse "$APP_REF")
    prep_app=$(kv "$d/prepared/flatpak-prepared.txt" prepared_app_commit)
    hx_obj() { printf '%s/%s' "$(printf '%s' "$1" | cut -c1-2)" "$(printf '%s' "$1" | cut -c3-)"; }
    # shellcheck disable=SC2329
    hx_check() { # IN OPTION...: --check-prepared of a prepared directory
        i=$1
        shift
        in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/flatpak-sign.sh --check-prepared --tag "$tag" --commit "$commit" --manifest "$m" --in "$i" --work "$rt/w" "$@" --check-remote
    }
    # shellcheck disable=SC2329
    hx_make() { # NAME MUTATOR: $d/hx.NAME is the prepared directory with the repository MUTATOR changed, described as a lying prepare job would
        rm -rf "$d/hx.$1" "$rt/hx.$1"
        cp -a "$d/prepared" "$d/hx.$1"
        mkdir -p "$rt/hx.$1"
        tar -C "$rt/hx.$1" -xf "$d/prepared/flatpak-prepared-repo.tar"
        "$2" "$rt/hx.$1/repo"
        tar -C "$rt/hx.$1" --owner=0 --group=0 --numeric-owner ${HX_TRANSFORM:+"--transform=$HX_TRANSFORM"} -cf "$d/hx.$1/flatpak-prepared-repo.tar" repo
        sed -i -e "s/^prepared_tar_sha256=.*/prepared_tar_sha256=$(sha256sum "$d/hx.$1/flatpak-prepared-repo.tar" | cut -d' ' -f1)/" \
            -e "s/^prepared_app_commit=.*/prepared_app_commit=$(ostree --repo="$rt/hx.$1/repo" rev-parse "$APP_REF")/" "$d/hx.$1/flatpak-prepared.txt"
    }
    # shellcheck disable=SC2329
    hx_u64() { # N: the text of a uint64 that ostree prints as N (it byte-swaps what it prints)
        printf 'uint64 0x%s' "$(printf '%016x' "$1" | sed 's/\(..\)\(..\)\(..\)\(..\)\(..\)\(..\)\(..\)\(..\)/\8\7\6\5\4\3\2\1/')"
    }
    # a new app commit on the published one, with the metadata of the prepared one (the keys named in $HX_DROP are left out)
    # and what the mutator adds
    HX_DROP="" HX_SUBJECT="" HX_BODY=""
    # shellcheck disable=SC2329
    hx_commit() { # REPO OPTION...
        r=$1
        shift
        h=$(ostree --repo="$r" rev-parse "$APP_REF")
        for k in xa.ref xa.metadata xa.from_commit xa.download-size xa.installed-size; do
            case " $HX_DROP " in *" $k "*) continue ;; esac
            v=$(ostree --repo="$r" show --print-metadata-key="$k" "$h")
            case $v in "uint64 "*) v=$(hx_u64 "${v#uint64 }") ;; esac
            set -- "$@" "--add-metadata=$k=$v"
        done
        case " $HX_DROP " in *" ostree.collection-binding "*) ;; *) set -- "$@" "--add-metadata=ostree.collection-binding=''" ;; esac
        ostree --repo="$r" commit -b "$APP_REF" --tree=ref="$APP_REF" --parent="$base_app" -s "${HX_SUBJECT:-Paleto Modkit ${tag#v} ($tag)}" -m "${HX_BODY:-source $commit}" "$@" > /dev/null
    }
    # shellcheck disable=SC2329
    hx_refuse() { # NAME MUTATOR PATTERN [CODE]: the check exits with CODE (2) and names the reason
        hx_make "$1" "$2"
        [ "$(kv "$d/hx.$1/flatpak-prepared.txt" prepared_app_commit)" != "$prep_app" ] || case $1 in config | appstreamkey | appstreamsubject | tamper | tar*) ;; *) fail "$1: the mutation did not change the app commit" ;; esac
        run "$d/hx.$1.log" hx_check "$d/hx.$1" --site "$rt/pages2"
        assert_rc "check of a prepared repository with $1" "${4:-2}"
        assert_log "$d/hx.$1.log" "$3"
        [ -e "$rt/w/signlist" ] && fail "$1: a list of commits to sign was written"
        rm -rf "$rt/w"
    }
    # shellcheck disable=SC2329
    mut_config() { printf '[flatpak]\nredirect-url=https://evil.example/repo\ntitle=Evil\n' >> "$1/config"; }
    # shellcheck disable=SC2329
    mut_parent() { ostree --repo="$1" commit -b "$APP_REF" --tree=ref="$APP_REF" -s "on the prepared commit, not the published one" > /dev/null; }
    # shellcheck disable=SC2329
    mut_nobinding() { hx_commit "$1" --no-bindings; }
    # shellcheck disable=SC2329
    mut_badbinding() { hx_commit "$1" --no-bindings --add-metadata="ostree.ref-binding=['app/org.example.Other/x86_64/master']"; }
    # shellcheck disable=SC2329
    mut_collection() { HX_DROP=ostree.collection-binding; hx_commit "$1" --add-metadata='ostree.collection-binding="org.example.X"'; HX_DROP=""; }
    # shellcheck disable=SC2329
    mut_nosize() { HX_DROP=xa.installed-size; hx_commit "$1"; HX_DROP=""; }
    # a key with a newline in it is two lines of the list, and the second one is there twice
    # shellcheck disable=SC2329
    mut_newlinekey() { hx_commit "$1" --add-metadata-string="xa.ref
xa.metadata=x"; }
    # shellcheck disable=SC2329
    mut_badxaref() { HX_DROP=xa.ref; hx_commit "$1" --add-metadata="xa.ref='app/org.example.Other/x86_64/master'"; HX_DROP=""; }
    # shellcheck disable=SC2329
    mut_badmeta() { HX_DROP=xa.metadata; hx_commit "$1" --add-metadata='xa.metadata="[Application]\nname=org.example.Evil\ncommand=evil\n"'; HX_DROP=""; }
    # shellcheck disable=SC2329
    mut_badsize() { HX_DROP=xa.download-size; hx_commit "$1" "--add-metadata=xa.download-size=$(hx_u64 1)"; HX_DROP=""; }
    # shellcheck disable=SC2329
    mut_badfrom() { HX_DROP=xa.from_commit; hx_commit "$1" "--add-metadata=xa.from_commit='$(printf 'f%.0s' $(seq 64))'"; HX_DROP=""; }
    # a delta planted in the prepared repository, which the summary would list: deleted, not refused
    # shellcheck disable=SC2329
    mut_deltas() { mkdir -p "$1/deltas/ab" "$1/delta-indexes/ab"; echo x > "$1/deltas/ab/cdef"; echo x > "$1/delta-indexes/ab/idx"; }
    # a published head (it is signed already, and not on the sign list) whose commit file is not the published one
    # shellcheck disable=SC2329
    mut_tamper() { f="$1/objects/$(hx_obj "$(ostree --repo="$1" rev-parse appstream2/x86_64)").commit"; chmod u+w "$f"; printf Z >> "$f"; }
    # shellcheck disable=SC2329
    mut_extrakey() { hx_commit "$1" --add-metadata-string=ostree.endoflife-rebase=org.example.Evil; }
    # shellcheck disable=SC2329
    mut_commitmeta() { hx_commit "$1"; cp "$rt/pages/repo/objects/$(hx_obj "$base_app").commitmeta" "$1/objects/$(hx_obj "$(ostree --repo="$1" rev-parse "$APP_REF")").commitmeta"; }
    # shellcheck disable=SC2329
    mut_future() { hx_commit "$1" --timestamp="$(date -u -d '+1 day' '+%Y-%m-%d %H:%M:%S')"; }
    # shellcheck disable=SC2329
    mut_past() { hx_commit "$1" --timestamp='2001-01-01 00:00:00'; }
    # shellcheck disable=SC2329
    mut_honest() { hx_commit "$1"; }
    # a new appstream commit on the published one, like build-update-repo makes it when the app data changed
    # shellcheck disable=SC2329
    mut_appstream() { ostree --repo="$1" commit -b appstream2/x86_64 --tree=ref=appstream2/x86_64 --parent="$(ostree --repo="$rt/pages/repo" rev-parse appstream2/x86_64)" -s "${HX_SUBJECT:-Update}" --add-metadata="ostree.collection-binding=''" "$@" > /dev/null; }
    # shellcheck disable=SC2329
    mut_subject() { HX_SUBJECT="Visit https://evil.example/paleto"; hx_commit "$1"; HX_SUBJECT=""; }
    # shellcheck disable=SC2329
    mut_body() { HX_BODY="source $commit, and a link to https://evil.example/paleto"; hx_commit "$1"; HX_BODY=""; }
    # shellcheck disable=SC2329
    mut_appstream_subject() { HX_SUBJECT="Evil"; mut_appstream "$1"; HX_SUBJECT=""; }
    # shellcheck disable=SC2329
    mut_appstream_key() { mut_appstream "$1" --add-metadata-string=xa.title=Evil; }

    # the control: the honest prepared repository passes, and its old summary files are gone before the key is used (the
    # signed summary is built from checked inputs only)
    tar -tf "$d/prepared/flatpak-prepared-repo.tar" | grep -qx 'repo/summary' || fail "the prepared repository has no summary: the test proves nothing"
    run "$d/hx.ok.log" hx_check "$d/prepared" --site "$rt/pages2"
    assert_rc "check of the honest prepared repository" 0
    for f in summary summary.sig summary.idx summary.idx.sig summaries deltas delta-indexes; do [ ! -e "$rt/w/repo/$f" ] || fail "$f of the prepared repository is still there after the check"; done
    rm -rf "$rt/w"
    hx_make honest mut_honest
    run "$d/hx.honest.log" hx_check "$d/hx.honest" --site "$rt/pages2"
    assert_rc "check of a new commit made like the honest one" 0
    grep -q "^$APP_REF $(ostree --repo="$rt/hx.honest/repo" rev-parse "$APP_REF")\$" "$rt/w/signlist" || fail "the new app commit is not on the sign list"
    rm -rf "$rt/w"

    hx_make appstream mut_appstream
    run "$d/hx.appstream.log" hx_check "$d/hx.appstream" --site "$rt/pages2"
    assert_rc "check of a new appstream commit" 0
    grep -q '^appstream2/x86_64 ' "$rt/w/signlist" || fail "the new appstream commit is not on the sign list"
    rm -rf "$rt/w"
    hx_make deltas mut_deltas
    run "$d/hx.deltas.log" hx_check "$d/hx.deltas" --site "$rt/pages2"
    assert_rc "check of a prepared repository with planted deltas" 0
    for f in deltas delta-indexes; do [ ! -e "$rt/w/repo/$f" ] || fail "the planted $f are still there after the check"; done
    [ -d "$rt/w/repo/tmp" ] || fail "the checked repository has no tmp folder"
    rm -rf "$rt/w"
    hx_refuse config mut_config "config"
    hx_refuse appstreamkey mut_appstream_key "other metadata keys than"
    hx_refuse appstreamsubject mut_appstream_subject "subject or the body"
    hx_refuse parent mut_parent "does not continue"
    hx_refuse nobinding mut_nobinding "not bound to its own ref"
    hx_refuse badbinding mut_badbinding "not bound to its own ref"
    hx_refuse collection mut_collection "bound to a collection"
    hx_refuse extrakey mut_extrakey "other metadata keys than"
    hx_refuse nosize mut_nosize "other metadata keys than"
    hx_refuse newlinekey mut_newlinekey "other metadata keys than"
    hx_refuse badxaref mut_badxaref "another xa.ref"
    hx_refuse badmeta mut_badmeta "xa.metadata or the sizes"
    hx_refuse badsize mut_badsize "xa.metadata or the sizes"
    hx_refuse badfrom mut_badfrom "verified build's commit"
    hx_refuse tamper mut_tamper "differs"
    hx_refuse subject mut_subject "subject or the body"
    hx_refuse body mut_body "subject or the body"
    hx_refuse commitmeta mut_commitmeta "commitmeta"
    hx_refuse future mut_future "in the future"
    hx_refuse past mut_past "older than"
    # what is in the archive and not in the repository: files that no honest run has, in places ostree ignores
    # shellcheck disable=SC2329
    mut_tarremotes() { mkdir -p "$1/refs/remotes/evil"; printf '%s\n' "$base_app" > "$1/refs/remotes/evil/x"; }
    # shellcheck disable=SC2329
    mut_tarmirrors() { printf '%s\n' "$base_app" > "$1/refs/mirrors/x"; }
    # shellcheck disable=SC2329
    mut_tartop() { echo x > "$1/evil"; }
    # shellcheck disable=SC2329
    mut_tarname() { echo x > "$1/objects/odd name"; }
    hx_refuse tarremotes mut_tarremotes "files in refs/"
    hx_refuse tarmirrors mut_tarmirrors "files in refs/"
    hx_refuse tartop mut_tartop "top of repo/"
    hx_refuse tarname mut_tarname "characters"
    # a member "repo//evil" has an empty component: it matches no check on the names, and tar unpacks it as repo/evil
    HX_TRANSFORM='s|^repo/evil$|repo//evil|'
    hx_refuse tartopslash mut_tartop "unsafe path"
    HX_TRANSFORM='s|^repo/refs/mirrors/x$|repo//refs/mirrors/x|'
    hx_refuse tarmirrorslash mut_tarmirrors "unsafe path"
    HX_TRANSFORM=""

    # the mode and the base: the flags say what the preparation had to be, the description is no proof
    run "$d/hx.fresh1.log" hx_check "$d/preparedf" --site "$rt/pages2"
    assert_rc "check of a repository prepared without a base, when gh-pages has one" 2
    assert_log "$d/hx.fresh1.log" "mode"
    run "$d/hx.fresh2.log" hx_check "$d/prepared" --fresh
    assert_rc "check of a stable release as a fresh repository" 3
    assert_log "$d/hx.fresh2.log" "release candidates"
    rm -rf "$d/hx.lie1" "$d/hx.lie2"
    cp -a "$d/preparedf" "$d/hx.lie1"
    sed -i 's/^mode=.*/mode=bootstrap/' "$d/hx.lie1/flatpak-prepared.txt"
    run "$d/hx.lie1.log" hx_check "$d/hx.lie1" --site "$rt/pages2"
    assert_rc "check of a repository that says it is a bootstrap, when gh-pages has one" 2
    cp -a "$d/preparedf" "$d/hx.lie2"
    sed -i -e 's/^mode=.*/mode=site/' -e "s/^base_gh_pages=.*/base_gh_pages=$(kv "$rt/pages2.base" gh_pages)/" "$d/hx.lie2/flatpak-prepared.txt"
    run "$d/hx.lie2.log" hx_check "$d/hx.lie2" --site "$rt/pages2"
    assert_rc "check of a repository without history that says it continues gh-pages" 2
    assert_log "$d/hx.lie2.log" "does not continue"
    # the honest fresh one is fine as a fresh one
    run "$d/hx.fresh3.log" hx_check "$d/preparedf" --fresh
    assert_rc "check of a stable release prepared as a fresh repository" 3
    assert_eq "keyrings left" 0 "$(leftover_gnupg)"
}

# the same for the result of the signing job: finish takes the mode and the parents from the record the signing job made,
# and refuses a summary with keys that flatpak build-update-repo does not write on its own
sc_sign_hostile_result() {
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 1 v1.6.0 release push sign
    assert_eq "failed job" "" "$PIPE_FAIL"
    d=$LIVE/runs/1
    tag=v1.6.0
    commit=$(kv "$d/validate.out" commit)
    rt=$SIM/rt/hr
    mkdir -p "$rt"
    m=$d/verified/release-manifest.txt
    # shellcheck disable=SC2329
    hr_finish() { # IN: the record of the signing job is made to fit the files of IN and pinned, as a forger who could also change the
        # job output would do it (the pin itself is tested in sign_pins), so that the checks of finish itself are reached
        for x in flatpak-prepared.txt:prepared_sha256 flatpak-base.txt:base_sha256 flatpak-pubkey.gpg:pubkey_sha256 flatpak-sign-result-repo.tar:signed_tar_sha256; do
            [ ! -f "$1/${x%%:*}" ] || sed -i "s/^${x##*:}=.*/${x##*:}=$(sha256sum "$1/${x%%:*}" | cut -d' ' -f1)/" "$1/flatpak-sign-result.txt"
        done
        in_dir "$d/build" env RUNNER_TEMP="$rt" RESULT_SHA256="$(sha256sum "$1/flatpak-sign-result.txt" | cut -d' ' -f1)" sh packaging/flatpak-sign.sh --finish --tag "$tag" --commit "$commit" --manifest "$m" --in "$1" --work "$rt/fw" --out "$rt/out"
    }
    # shellcheck disable=SC2329
    hr_copy() { rm -rf "$d/hr.$1"; cp -a "$d/signres" "$d/hr.$1"; }
    # shellcheck disable=SC2329
    hr_retar() { # NAME: the repository in $rt/hr.NAME is the signed one
        tar -C "$rt/hr.$1" --owner=0 --group=0 --numeric-owner -cf "$d/hr.$1/flatpak-sign-result-repo.tar" repo
        sed -i "s/^signed_tar_sha256=.*/signed_tar_sha256=$(sha256sum "$d/hr.$1/flatpak-sign-result-repo.tar" | cut -d' ' -f1)/" "$d/hr.$1/flatpak-sign-result.txt"
    }
    # shellcheck disable=SC2329
    hr_unpack() { rm -rf "$rt/hr.$1"; mkdir -p "$rt/hr.$1"; tar -C "$rt/hr.$1" -xf "$d/signres/flatpak-sign-result-repo.tar"; }
    run "$d/r0.log" hr_finish "$d/signres"
    assert_rc "finish of the honest result" 0
    assert_file "the record of the base" "$d/signres/flatpak-base.txt"
    assert_eq "mode in the signed record" site "$(kv "$rt/out/flatpak-signed.txt" mode)"
    assert_eq "the base commit in the signed record" "$(ostree --repo="$LIVE/site/repo" rev-parse "$APP_REF")" "$(kv "$rt/out/flatpak-signed.txt" base_app_commit)"
    # without the record of the base
    hr_copy nobase
    rm -f "$d/hr.nobase/flatpak-base.txt"
    run "$d/r1.log" hr_finish "$d/hr.nobase"
    assert_rc "finish without the record of the base" 2
    assert_log "$d/r1.log" "flatpak-base.txt is missing"
    # a record that names another parent than the one the signed commit has
    hr_copy parent
    sed -i "s|^\(head $APP_REF \).*|\1$(printf 'e%.0s' $(seq 64))|" "$d/hr.parent/flatpak-base.txt"
    run "$d/r2.log" hr_finish "$d/hr.parent"
    assert_rc "finish with a record of another base" 4
    assert_log "$d/r2.log" "does not continue"
    # a record without heads for a site
    hr_copy noheads
    sed -i '/^head /d' "$d/hr.noheads/flatpak-base.txt"
    run "$d/r3.log" hr_finish "$d/hr.noheads"
    assert_rc "finish with a record of a site without heads" 4
    # the config of the signed repository
    hr_copy config
    hr_unpack config
    printf '[flatpak]\nredirect-url=https://evil.example/repo\n' >> "$rt/hr.config/repo/config"
    hr_retar config
    run "$d/r4.log" hr_finish "$d/hr.config"
    assert_rc "finish with a hostile config" 4
    assert_log "$d/r4.log" "config"
    # a summary with a title and a redirect, signed by the real key, and the config put back as it was
    hr_copy summary
    hr_unpack summary
    cp "$rt/hr.summary/repo/config" "$rt/hr.summary/config.orig"
    mkdir -p "$KEYS/hr.home"
    chmod 700 "$KEYS/hr.home"
    gpg --homedir "$KEYS/hr.home" --batch --quiet --import "$KEYS/K.sec" > /dev/null 2>&1
    flatpak build-update-repo --no-update-appstream --title=Evil --redirect-url=https://evil.example/repo --gpg-sign="$FPR_K" --gpg-homedir="$KEYS/hr.home" "$rt/hr.summary/repo" > "$d/r5.build.log" 2>&1 || fail "could not make the hostile summary"
    gpgconf --homedir "$KEYS/hr.home" --kill all > /dev/null 2>&1 || true
    cp "$rt/hr.summary/config.orig" "$rt/hr.summary/repo/config"
    ostree --repo="$rt/hr.summary/repo" summary --list-metadata-keys | grep -qx xa.redirect-url || fail "the hostile summary has no redirect: the test proves nothing"
    hr_retar summary
    run "$d/r5.log" hr_finish "$d/hr.summary"
    assert_rc "finish with a summary that redirects" 4
    assert_log "$d/r5.log" "summary"
    # static deltas in the signed repository: the signing job makes none, and --check-prepared deleted those of the preparation
    hr_copy deltas
    hr_unpack deltas
    mkdir -p "$rt/hr.deltas/repo/deltas/ab"
    echo x > "$rt/hr.deltas/repo/deltas/ab/cdef"
    hr_retar deltas
    run "$d/r6.log" hr_finish "$d/hr.deltas"
    assert_rc "finish with deltas in the signed repository" 2
    assert_log "$d/r6.log" "top of repo/"
    assert_eq "keyrings left" 0 "$(leftover_gnupg)"
}

# the manifest and the signed result go from job to job as run artifacts, which a compromised job can replace; the hashes are job
# outputs, which it cannot (the emulation: verify.gho and finish.gho). Forged files that tell a consistent story are refused.
sc_sign_pins() {
    commit_release 1.6.0
    tag_release v1.6.0
    pipeline 1 v1.6.0 release push verify
    assert_eq "failed job" "" "$PIPE_FAIL"
    d=$LIVE/runs/1
    tag=v1.6.0
    commit=$(kv "$d/validate.out" commit)
    rt=$SIM/rt/pin
    mkdir -p "$rt"
    m=$d/verified/release-manifest.txt
    good=$(kv "$d/verify.gho" manifest_sha256)
    assert_eq "the pin is the hash of the manifest" "$(sha256sum "$m" | cut -d' ' -f1)" "$good"
    run "$d/pages.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/release-pages.sh fetch "$rt/pages"
    assert_rc "fetch" 0
    run "$d/pages2.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/release-pages.sh fetch "$rt/pages2"
    assert_rc "fetch again" 0
    run "$d/n1.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/flatpak-sign.sh --prepare --tag "$tag" --commit "$commit" --manifest "$m" \
        --unsigned "$d/fp/flatpak-unsigned-repo.tar" --site "$rt/pages" --work "$rt/pw" --out "$d/prepared"
    assert_rc "prepare" 0
    # a forged manifest with a prepared repository that fits it: another build of the same commit, verified and prepared consistently
    fx_flatpak "$d/build" "$d/fp2"
    run "$d/n2.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/release-verify.sh builds --tag "$tag" --commit "$commit" --flatpak "$d/fp2" --out "$d/verified2"
    assert_rc "verify the other build" 0
    m2=$d/verified2/release-manifest.txt
    run "$d/n3.log" in_dir "$d/build" env RUNNER_TEMP="$rt" sh packaging/flatpak-sign.sh --prepare --tag "$tag" --commit "$commit" --manifest "$m2" \
        --unsigned "$d/fp2/flatpak-unsigned-repo.tar" --site "$rt/pages" --work "$rt/pw2" --out "$d/prepared2"
    assert_rc "prepare the other build" 0
    # shellcheck disable=SC2329
    pin_check() { # PIN MANIFEST PREPARED
        in_dir "$d/build" env RUNNER_TEMP="$rt" MANIFEST_SHA256="$1" sh packaging/flatpak-sign.sh --check-prepared --tag "$tag" --commit "$commit" \
            --manifest "$2" --in "$3" --work "$rt/w" --site "$rt/pages2" --check-remote
    }
    run "$d/n4.log" pin_check "$(sha256sum "$m2" | cut -d' ' -f1)" "$m2" "$d/prepared2"
    assert_rc "check of the forged pair with the hash of the forged manifest (the control: only the pin can stop it)" 0
    run "$d/n5.log" pin_check "$good" "$m2" "$d/prepared2"
    assert_rc "check of a forged manifest and its prepared repository" 2
    assert_log "$d/n5.log" "pinned"
    run "$d/n6.log" pin_check "" "$m" "$d/prepared"
    assert_rc "check with an empty pin" 2
    assert_log "$d/n6.log" "pinned"
    run "$d/n7.log" pin_check "not-a-hash" "$m" "$d/prepared"
    assert_rc "check with a pin that is not a hash" 2
    run "$d/n8.log" pin_check "$good" "$m" "$d/prepared"
    assert_rc "check of the honest pair" 0

    # the result of the signing job and the publish job
    pipeline 2 v1.6.0 release push sign
    assert_eq "failed job" "" "$PIPE_FAIL"
    e=$LIVE/runs/2
    m=$e/verified/release-manifest.txt
    # shellcheck disable=SC2329
    fin() { # MANIFEST_PIN RESULT_PIN IN
        in_dir "$e/build" env RUNNER_TEMP="$rt" MANIFEST_SHA256="$1" RESULT_SHA256="$2" GITHUB_OUTPUT="$rt/fin.gho" sh packaging/flatpak-sign.sh --finish --tag "$tag" \
            --commit "$(kv "$e/validate.out" commit)" --manifest "$m" --in "$3" --work "$rt/fw" --out "$rt/out"
    }
    mp=$(kv "$e/verify.gho" manifest_sha256)
    rp=$(kv "$e/sign.gho" result_sha256)
    assert_eq "the pin is the hash of the record of the sign job" "$(sha256sum "$e/signres/flatpak-sign-result.txt" | cut -d' ' -f1)" "$rp"
    run "$e/f1.log" fin "" "$rp" "$e/signres"
    assert_rc "finish with an empty pin" 2
    run "$e/f2.log" fin "$(printf 'a%.0s' $(seq 64))" "$rp" "$e/signres"
    assert_rc "finish with another hash" 2
    assert_log "$e/f2.log" "pinned"
    run "$e/f3.log" fin "$mp" "$rp" "$e/signres"
    assert_rc "finish with the pins" 0
    run "$e/f4.log" fin "$mp" "" "$e/signres"
    assert_rc "finish with an empty pin of the sign job" 2
    run "$e/f5.log" fin "$mp" "$(printf 'a%.0s' $(seq 64))" "$e/signres"
    assert_rc "finish with another pin of the sign job" 2
    assert_log "$e/f5.log" "pinned"
    # a skip record forged into the result of the sign job: nothing is signed, publish would say "nothing to do"
    rm -rf "$e/hr.skip"
    cp -a "$e/signres" "$e/hr.skip"
    sed -i 's/^skip=.*/skip=1/' "$e/hr.skip/flatpak-prepared.txt"
    run "$e/f6.log" fin "$mp" "$rp" "$e/hr.skip"
    assert_rc "finish with a forged skip" 2
    assert_log "$e/f6.log" "recorded"
    sed -i "s/^prepared_sha256=.*/prepared_sha256=$(sha256sum "$e/hr.skip/flatpak-prepared.txt" | cut -d' ' -f1)/" "$e/hr.skip/flatpak-sign-result.txt"
    run "$e/f7.log" fin "$mp" "$rp" "$e/hr.skip"
    assert_rc "finish with a forged skip and a record that fits it" 2
    assert_log "$e/f7.log" "pinned"
    # the base of gh-pages in the record changed to the head of gh-pages after it moved: the lease of publish would not see it
    rm -rf "$e/hr.base"
    cp -a "$e/signres" "$e/hr.base"
    sed -i "s/^base_gh_pages=.*/base_gh_pages=$(printf 'c%.0s' $(seq 40))/" "$e/hr.base/flatpak-base.txt"
    run "$e/f8.log" fin "$mp" "$rp" "$e/hr.base"
    assert_rc "finish with another base of gh-pages" 2
    assert_log "$e/f8.log" "recorded"
    sed -i "s/^base_sha256=.*/base_sha256=$(sha256sum "$e/hr.base/flatpak-base.txt" | cut -d' ' -f1)/" "$e/hr.base/flatpak-sign-result.txt"
    run "$e/f9.log" fin "$mp" "$rp" "$e/hr.base"
    assert_rc "finish with another base of gh-pages and a record that fits it" 2
    assert_log "$e/f9.log" "pinned"
    # the public key in another encoding: the same key, other bytes in the files users add the repository with
    rm -rf "$e/hr.key"
    cp -a "$e/signres" "$e/hr.key"
    # the same key, armored by hand (the checksum line is optional)
    { echo '-----BEGIN PGP PUBLIC KEY BLOCK-----'; echo; base64 -w64 "$e/signres/flatpak-pubkey.gpg"; echo '-----END PGP PUBLIC KEY BLOCK-----'; } > "$e/hr.key/flatpak-pubkey.gpg"
    cmp -s "$e/signres/flatpak-pubkey.gpg" "$e/hr.key/flatpak-pubkey.gpg" && fail "the other encoding of the key has the same bytes"
    run "$e/f10.log" fin "$mp" "$rp" "$e/hr.key"
    assert_rc "finish with the key in another encoding" 2
    assert_log "$e/f10.log" "recorded"
    assert_eq "finish pinned flatpak-signed.txt" "$(sha256sum "$rt/out/flatpak-signed.txt" | cut -d' ' -f1)" "$(kv "$rt/fin.gho" signed_sha256)"
    snapshot > "$SIM/snap.before"
    # the bundle replaced, and flatpak-signed.txt fitted to it: the files tell a consistent story, the pin does not
    cp -a "$e/signed" "$e/signed.good"
    printf 'not the bundle' >> "$e/signed/PaletoModkit-x86_64.flatpak"
    sed -i -e "s/^bundle_sha256=.*/bundle_sha256=$(sha256sum "$e/signed/PaletoModkit-x86_64.flatpak" | cut -d' ' -f1)/" \
        -e "s/^bundle_size=.*/bundle_size=$(stat -c %s "$e/signed/PaletoModkit-x86_64.flatpak")/" "$e/signed/flatpak-signed.txt"
    job_publish 2 release
    assert_rc "publish with a swapped bundle" 2
    assert_log "$e/publish.log" "pinned"
    rm -rf "$e/signed"
    mv "$e/signed.good" "$e/signed"
    # the manifest replaced
    cp "$e/verified/release-manifest.txt" "$e/manifest.good"
    echo "flatpak_extra=forged" >> "$e/verified/release-manifest.txt"
    job_publish 2 release
    assert_rc "publish with a swapped manifest" 2
    assert_log "$e/publish.log" "pinned"
    cp "$e/manifest.good" "$e/verified/release-manifest.txt"
    # the public key in another encoding (the same key): flatpak-signed.txt records the hash of the file
    cp "$e/signed/flatpak-pubkey.gpg" "$e/pubkey.good"
    cp "$e/hr.key/flatpak-pubkey.gpg" "$e/signed/flatpak-pubkey.gpg"
    job_publish 2 release
    assert_rc "publish with the key in another encoding" 2
    assert_log "$e/publish.log" "pubkey_sha256"
    cp "$e/pubkey.good" "$e/signed/flatpak-pubkey.gpg"
    # no pin
    cp "$e/finish.gho" "$e/finish.good"
    : > "$e/finish.gho"
    job_publish 2 release
    assert_rc "publish with no pin for the signed result" 2
    assert_log "$e/publish.log" "pinned"
    cp "$e/finish.good" "$e/finish.gho"
    snapshot > "$SIM/snap.after"
    cmp -s "$SIM/snap.before" "$SIM/snap.after" || fail "the remote changed"
    # the control: the real files and the real pins publish
    job_publish 2 release
    assert_rc "publish with the real files and pins" 0
}

# the checks of the library, on their own
lib_run() { sh -c ". \"\$0\"; $1" "$PKG/release-lib.sh"; }
# shellcheck disable=SC2016
sc_lib_checks() {
    nl=$(printf '\n_')
    nl=${nl%_}
    h40=$(printf 'a%.0s' $(seq 40))
    if X="v1.6.0${nl}v9.9.9" lib_run 'semver_valid "$X"'; then fail "a two line tag is valid"; fi
    if X="v1.6.0$(printf '\r')" lib_run 'semver_valid "$X"'; then fail "a tag with a CR is valid"; fi
    if X="${h40}${nl}zz" lib_run 'is_hex "$X" 40'; then fail "a two line hex value is valid"; fi
    X="$h40" lib_run 'is_hex "$X" 40' || fail "a hex value is not valid"
    X="v1.6.0" lib_run 'semver_valid "$X"' || fail "v1.6.0 is not valid"
    cmp_of() { lib_run "semver_cmp $1 $2"; }
    assert_eq "cmp big patch" 1 "$(cmp_of v1.2.9007199254740993 v1.2.9007199254740992)"
    assert_eq "cmp big patch reversed" -1 "$(cmp_of v1.2.9007199254740992 v1.2.9007199254740993)"
    assert_eq "cmp equal" 0 "$(cmp_of v1.2.3 v1.2.3)"
    assert_eq "cmp 10 over 9" 1 "$(cmp_of v1.10.0 v1.9.0)"
    assert_eq "cmp rc.10 over rc.9" 1 "$(cmp_of v2.0.0-rc.10 v2.0.0-rc.9)"
    assert_eq "cmp release over rc" 1 "$(cmp_of v2.0.0 v2.0.0-rc.1)"
    assert_eq "cmp rc below release" -1 "$(cmp_of v2.0.0-rc.1 v2.0.0)"
    # the public key file
    cat "$KEYS/K.pub" "$KEYS/A.pub" > "$SIM/two.pub"
    assert_eq "fingerprint of one key" "$FPR_K" "$(lib_run "pubkey_fpr \"$KEYS/K.pub\"")"
    assert_eq "fingerprint of two keys" "" "$(lib_run "pubkey_fpr \"$SIM/two.pub\"")"
    assert_eq "fingerprint of a secret key file" "" "$(lib_run "pubkey_fpr \"$KEYS/K.sec\"")"
    assert_eq "fingerprint of nothing" "" "$(lib_run "pubkey_fpr /dev/null")"
}

# --check-head: the build jobs check the submodule out, the other jobs do not and say so
sc_check_head() {
    commit_release 1.6.0
    tag_release v1.6.0
    job_validate 1 v1.6.0 release
    assert_rc "validate" 0
    commit=$(kv "$LIVE/runs/1/validate.out" commit)
    x=$LIVE/runs/1/bare
    rm -rf "$x"
    git clone -q "$REMOTE" "$x"
    git -C "$x" checkout -q --detach "$commit"
    run "$LIVE/runs/1/ch1.log" in_dir "$x" sh packaging/release-validate.sh --check-head "$commit"
    assert_rc "check-head without the submodule" 2
    run "$LIVE/runs/1/ch2.log" in_dir "$x" sh packaging/release-validate.sh --check-head "$commit" --no-submodule
    assert_rc "check-head --no-submodule" 0
    run "$LIVE/runs/1/ch3.log" in_dir "$x" sh packaging/release-validate.sh --check-head "$(printf 'f%.0s' $(seq 40))" --no-submodule
    assert_rc "check-head --no-submodule on another commit" 2
    git -C "$x" submodule update -q --init
    run "$LIVE/runs/1/ch4.log" in_dir "$x" sh packaging/release-validate.sh --check-head "$commit"
    assert_rc "check-head with the submodule" 0
}

# ---- the world, once ----------------------------------------------------------------------------------------------

make_template() {
    FPR_K=$(fx_key "$KEYS" K)
    fx_key "$KEYS" A > /dev/null
    # keep the test keys out of the real keyring: nothing is imported anywhere but the sign script's own folder
    printf 'fake appimagetool\n' > "$SIM/tools/appimagetool"
    printf 'fake runtime\n' > "$SIM/tools/runtime"
    # a CodeWalker stand-in
    git init -q "$SIM/cw-work"
    echo "codewalker" > "$SIM/cw-work/README"
    git -C "$SIM/cw-work" add README
    git -C "$SIM/cw-work" commit -q -m "CodeWalker"
    git clone -q --bare "$SIM/cw-work" "$CW"
    rm -rf "$LIVE"
    mkdir -p "$LIVE/stub"
    git init -q --bare "$LIVE/origin.git"
    git -C "$LIVE/origin.git" config uploadpack.allowFilter true
    git -C "$LIVE/origin.git" config uploadpack.allowAnySHA1InWant true
    install_hooks
    w=$LIVE/work
    mkdir -p "$w"
    fx_project "$w" 1.5.1 "file://$CW" "$PKG"
    write_conf "$w/packaging/release.conf"
    git -C "$w" init -q
    git -C "$w" submodule add -q "file://$CW" third_party/codewalker
    git -C "$w" add -A
    git -C "$w" commit -q -m "Release 1.5.1"
    git -C "$w" remote add origin "$REMOTE"
    git -C "$w" push -q origin main
    git -C "$w" tag -a v1.5.1 -m v1.5.1
    git -C "$w" push -q origin v1.5.1
    git -C "$w" fetch -q origin
    BOOTSTRAP=true
    pipeline 0 v1.5.1 release
    if [ "$RC" != 0 ] || [ -n "$PIPE_FAIL" ]; then
        echo "the baseline release failed in $PIPE_FAIL (exit $RC):" >&2
        tail -n 20 "$RUNLOG" >&2
        KEEP=1
        exit 1
    fi
    BOOTSTRAP=false
    rm -rf "$LIVE/runs"
    cp -a "$LIVE" "$TPL"
}

restore() {
    rm -rf "$LIVE" "$SIM/rt"
    mkdir -p "$SIM/rt"
    cp -a "$TPL" "$LIVE"
}

SCENARIOS="valid_stable invalid_tag mismatch_cargo mismatch_lock mismatch_metainfo mismatch_date mismatch_submodule mismatch_extra_submodule mismatch_rc_type mismatch_stable_dev
empty_pin identity_change prerelease main_moved tag_not_on_main build_failure converter_failure missing_zsync zsync_length flipped_appimage tool_pin build_flatpak_script sim_guard
gpg_missing gpg_wrong_key gpg_corrupt tampered_signed interrupted_upload listing_lag interrupted_edit interrupted_push pages_wait rerun_same_tag same_tag_new_content downgrade concurrent
gh_pages_missing another_writer tag_moved legacy_record publish_flatpak sign_fresh_and_verify
pages_second_key pages_other_key pages_record_unreadable pages_symlink sign_secret_env gpg_two_keys lib_checks check_head pages_key_bytes sign_prepared_checks
pages_hostile_config sign_hostile_prepared sign_hostile_result sign_pins forged_skip replay_pages"
if [ "${1:-}" = -l ]; then echo "$SCENARIOS" | tr '\n' ' '; echo; exit 0; fi

FAILFILE=$SIM/fails
make_template
LIST=${*:-$SCENARIOS}
FAILED=0
PASSED=0
for s in $LIST; do
    s=$(printf '%s' "$s" | tr '-' '_')
    restore
    : > "$FAILFILE"
    rc=0
    ( set -eu; "sc_$s" ) > "$SIM/scenario.log" 2>&1 || rc=$?
    if [ "$rc" != 0 ] || [ -s "$FAILFILE" ]; then
        echo "FAIL $s: $( [ "$rc" = 0 ] || echo "aborted with $rc;") $(tr '\n' ';' < "$FAILFILE")"
        if [ "${VERBOSE:-}" = 1 ]; then
            tail -n 15 "$SIM/scenario.log" | sed 's/^/    | /'
            for l in "$LIVE"/runs/*/*.log; do [ -s "$l" ] && { echo "    == $l"; tail -n 6 "$l" | sed 's/^/    | /'; }; done
        fi
        FAILED=$((FAILED + 1))
    else
        echo "PASS $s"
        PASSED=$((PASSED + 1))
    fi
done
echo "$PASSED passed, $FAILED failed"
[ "$FAILED" = 0 ]
