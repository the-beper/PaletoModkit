#!/bin/sh
# Offline (loopback only) integration test of the signed Flatpak repository: the real packaging/release-verify.sh and
# packaging/flatpak-sign.sh sign a synthetic app with throwaway keys, and the real flatpak client, in an
# installation of its own, installs it, updates it and refuses tampered copies. No network, no D-Bus, no
# compositor, and nothing of the real home or the real Flatpak installation (checked at the end).
#   sh packaging/tests/flatpak-repo-test.sh
#   FLATPAK_TEST_BASE=dir   where the scratch tree goes (default $TMPDIR or /tmp; a disk is kinder than a tmpfs)
#   KEEP=1                  keep the scratch tree; VERBOSE=1 print the log of a failed step
# Needs flatpak, ostree, gpg, tar, python3; exits 77 (skipped) if one is missing. Exit 1 if a check fails.
# What it proves: signing, summary and parent chain (there are no static deltas: see flatpak-sign.sh); install and update by a client that checks
# signatures; the refusal of a corrupted object, an unsigned summary, a commit signed by another key, an unsigned
# commit under a signed summary, an older repository and a corrupted bundle; the anti-downgrade rule of the
# signing script. What it cannot: the real build with the Flathub SDK, runtime dependencies (--no-deps), the HTTP
# serving of GitHub Pages (a throwaway python http.server on 127.0.0.1 stands in: libostree trusts the objects of a
# file:// remote and does not check their checksums, so only HTTP shows that a client does), the software centre.
set -eu

for t in flatpak ostree gpg tar sha256sum python3; do
    command -v "$t" > /dev/null || { echo "SKIP: $t is needed"; exit 77; }
done
HERE=$(cd "$(dirname "$0")" && pwd)
PKG=$(cd "$HERE/.." && pwd)

# Start again with an empty environment, so that nothing of the caller's Flatpak, GnuPG, D-Bus or display
# configuration is inherited.
if [ -z "${FRT_ISOLATED:-}" ]; then
    exec env -i PATH="$PATH" LANG=C LC_ALL=C FRT_ISOLATED=1 FRT_REAL_HOME="${HOME:-/nonexistent}" \
        TMPDIR="${TMPDIR:-/tmp}" FLATPAK_TEST_BASE="${FLATPAK_TEST_BASE:-}" KEEP="${KEEP:-}" VERBOSE="${VERBOSE:-}" sh "$0" "$@"
fi
# shellcheck source=/dev/null
. "$HERE/lib-fixture.sh"

BASE=$(mktemp -d "${FLATPAK_TEST_BASE:-${TMPDIR:-/tmp}}/frt.XXXXXX")
SERVER_PID=""
cleanup() {
    [ -z "$SERVER_PID" ] || kill "$SERVER_PID" > /dev/null 2>&1 || true
    for h in "$BASE"/keys/*.home "$BASE"/rt/gnupg.* "$BASE"/ahome; do
        [ -d "$h" ] && gpgconf --homedir "$h" --kill all > /dev/null 2>&1 || true
    done
    if [ "${KEEP:-}" = 1 ]; then echo "kept $BASE"; else rm -rf "$BASE"; fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

real_listing() { # a read-only picture of the real installation of the user
    d=$FRT_REAL_HOME/.local/share/flatpak
    if [ -d "$d" ]; then find "$d" -printf '%p %y %s %T@\n' 2> /dev/null | LC_ALL=C sort | sha256sum; else echo none; fi
}
REAL_BEFORE=$(real_listing)

fx_env "$BASE/env"
KEYS=$BASE/keys
WEB=$BASE/web
mkdir -p "$KEYS" "$BASE/rt" "$BASE/clients" "$WEB"
chmod 700 "$KEYS"
RUNNER_TEMP="$BASE/rt"
export RUNNER_TEMP
ID=io.github.the_beper.PaletoModkit
APP_REF=app/$ID/x86_64/master
FPR_K=$(fx_key "$KEYS" K)
FPR_A=$(fx_key "$KEYS" A)
COMMIT=$(printf '1%.0s' $(seq 40))
CONF=$BASE/release.conf
cat > "$CONF" <<CONF
APP_ID=$ID
FLATPAK_BRANCH=master
SITE_URL=file://$BASE/site
GITHUB_REPO=test/paleto
FLATPAK_KEY_FPR=$FPR_K
CONF
RELEASE_CONF="$CONF"
REMOTE="file://$BASE/none.git"
GH_REPO=test/paleto
export RELEASE_CONF REMOTE GH_REPO

FAILED=0 PASSED=0
LOG=$BASE/step.log
ok() { echo "PASS $1"; PASSED=$((PASSED + 1)); }
no() {
    echo "FAIL $1"
    FAILED=$((FAILED + 1))
    if [ "${VERBOSE:-}" = 1 ]; then tail -n 15 "$LOG" 2> /dev/null | sed 's/^/    | /'; fi
}
# check NAME CMD...: the command succeeds.   refuse NAME CMD...: the command fails.
check() { n=$1; shift; if "$@" > "$LOG" 2>&1; then ok "$n"; else no "$n"; fi; }
refuse() { n=$1; shift; if "$@" > "$LOG" 2>&1; then no "$n"; else ok "$n"; fi; }
same() { [ "$1" = "$2" ]; } # the arguments are equal

# ---- builds and signing -----------------------------------------------------------------------------------------

# mkbuild VERSION: BASE/b/VERSION/{flatpak-unsigned-repo.tar,flatpak-build.txt} and the manifest BASE/m/VERSION/.
mkbuild() {
    d=$BASE/b/$1
    mkdir -p "$d" "$BASE/m"
    fx_metainfo "$d/metainfo.xml" "$ID" "$1" 2026-10-29
    t=$(mktemp -d "$BASE/rt/mk.XXXXXX")
    fx_unsigned_repo "$t" "$d/metainfo.xml" "$ID"
    tar -C "$t" --owner=0 --group=0 --numeric-owner -cf "$d/flatpak-unsigned-repo.tar" repo
    {
        echo "format=1"
        echo "version=$1"
        echo "commit=$COMMIT"
        echo "image=fake"
        echo "app_commit=$(ostree --repo="$t/repo" rev-parse "$APP_REF")"
        echo "debug_commit=$(ostree --repo="$t/repo" rev-parse "runtime/$ID.Debug/x86_64/master")"
    } > "$d/flatpak-build.txt"
    rm -rf "$t"
    mkdir -p "$BASE/fp/$1"
    cp "$d/flatpak-unsigned-repo.tar" "$d/flatpak-build.txt" "$BASE/fp/$1/"
    sh "$PKG/release-verify.sh" builds --tag "v$1" --commit "$COMMIT" --flatpak "$BASE/fp/$1" --out "$BASE/m/$1"
}

# sign VERSION SITE OUT [KEYFILE] [flatpak-sign.sh options]: sign the build of VERSION on top of the site SITE, in
# the steps of the signing jobs (prepare and check-prepared without the secret, sign with it, finish without it); the
# exit code is the one of the first step that fails. Work folders: OUT.pwork, OUT.prep, OUT.work, OUT.res, OUT.fwork.
sign() {
    v=$1 site=$2 out=$3 keyf=${4:-$KEYS/K.sec}
    shift 4
    man=$BASE/m/$v/release-manifest.txt
    sh "$PKG/flatpak-sign.sh" --prepare --tag "v$v" --commit "$COMMIT" --manifest "$man" \
        --unsigned "$BASE/b/$v/flatpak-unsigned-repo.tar" --site "$site" --work "$out.pwork" --out "$out.prep" "$@" || return $?
    sh "$PKG/flatpak-sign.sh" --check-prepared --tag "v$v" --commit "$COMMIT" --manifest "$man" --in "$out.prep" --work "$out.work" --site "$site" "$@" || return $?
    FLATPAK_GPG_PRIVATE_KEY=$(cat "$keyf") sh "$PKG/flatpak-sign.sh" --sign --tag "v$v" --commit "$COMMIT" --work "$out.work" --out "$out.res" || return $?
    sh "$PKG/flatpak-sign.sh" --finish --tag "v$v" --commit "$COMMIT" --manifest "$man" --in "$out.res" --work "$out.fwork" --out "$out" || return $?
    sh "$PKG/flatpak-sign.sh" --cleanup --work "$out.work"
    sh "$PKG/flatpak-sign.sh" --cleanup --work "$out.pwork"
    sh "$PKG/flatpak-sign.sh" --cleanup --work "$out.fwork"
}

# mksite SITE: an empty published repository (the first publication).
mksite_empty() { rm -rf "$1" "$1.base"; mkdir -p "$1"; printf 'gh_pages=\ngh_pages_previous=\nsubject=\n' > "$1.base"; }
# site_meta SITE VERSION PUBKEY: the files around the repository in SITE/repo, as release-publish.sh writes them.
site_meta() {
    sh "$PKG/publish-flatpak-repo.sh" "$1" "file://$1" "$3" > /dev/null
    {
        echo "format=1"
        echo "tag=v$2"
        echo "version=$2"
        echo "source_commit=$COMMIT"
        echo "ostree_app_commit=$(ostree --repo="$1/repo" rev-parse "$APP_REF")"
        echo "ostree_debug_commit=$(ostree --repo="$1/repo" rev-parse "runtime/$ID.Debug/x86_64/master")"
        echo "key_fpr=$FPR_K"
    } > "$1/release.txt"
    printf 'gh_pages=%s\ngh_pages_previous=\nsubject=Flatpak repository for v%s\n' "$(printf '2%.0s' $(seq 40))" "$2" > "$1.base"
}
# site_from SIGNED_DIR VERSION SITE: what release-publish.sh puts on Pages from the output of the finish step.
site_from() {
    rm -rf "$3" "$3.base"
    mkdir -p "$3"
    tar -C "$3" -xf "$1/flatpak-signed-repo.tar"
    site_meta "$3" "$2" "$1/flatpak-pubkey.gpg"
}
# legacy_sign VERSION BASESITE OUTDIR: the old way, one step with the key, static deltas included, like the
# repository that is published today (BASESITE empty: a repository of its own).
legacy_sign() {
    rm -rf "$3" "$3.u"
    mkdir -p "$3" "$3.u"
    tar -C "$3.u" -xf "$BASE/b/$1/flatpak-unsigned-repo.tar"
    # the config of the published repository, byte for byte (the scripts refuse any other)
    if [ -n "$2" ]; then cp -a "$2/repo" "$3/repo"; else ostree --repo="$3/repo" init --mode=archive-z2 && printf '[core]\nrepo_version=1\nmode=archive-z2\nindexed-deltas=true\n' > "$3/repo/config"; fi
    flatpak build-commit-from --untrusted --src-repo="$3.u/repo" --gpg-sign="$FPR_K" --gpg-homedir="$BASE/ahome" --no-update-summary \
        "$3/repo" "$APP_REF" "runtime/$ID.Debug/x86_64/master" > /dev/null 2>&1
    flatpak build-update-repo --gpg-sign="$FPR_K" --gpg-homedir="$BASE/ahome" --generate-static-deltas --prune --prune-depth=3 "$3/repo" > /dev/null 2>&1
}
head_of() { ostree --repo="$1/repo" rev-parse "$APP_REF"; }

# ---- the client ---------------------------------------------------------------------------------------------------

# A throwaway web server on a loopback port stands in for GitHub Pages (WEB is its root).
python3 -I -u -m http.server 0 --bind 127.0.0.1 --directory "$WEB" > "$BASE/http.log" 2>&1 &
SERVER_PID=$!
PORT=""
for _ in $(seq 1 100); do
    PORT=$(sed -n 's/.* port \([0-9][0-9]*\) .*/\1/p' "$BASE/http.log" | head -n1)
    [ -z "$PORT" ] || break
    sleep 0.1
done
[ -n "$PORT" ] || { echo "the web server did not start"; exit 1; }

# fp CLIENT COMMAND ARGS...: the flatpak client in an installation of its own (a --user one under BASE/clients).
fp() {
    c=$1 cmd=$2
    shift 2
    mkdir -p "$BASE/clients/$c"
    FLATPAK_USER_DIR=$BASE/clients/$c flatpak "$cmd" --user "$@"
}
# remote CLIENT NAME: the client has WEB/NAME as the remote t, with the public key K and nothing else.
remote() { fp "$1" remote-add --gpg-import="$KEYS/K.pub" t "http://127.0.0.1:$PORT/$2/repo"; }
install_app() { fp "$1" install --noninteractive --no-deps --no-related t "$ID//master"; }
installed() { fp "$1" info --show-commit "$ID//master" 2> /dev/null || echo none; }
nothing_installed() { [ "$(installed "$1")" = none ]; }
# publish NAME SITE: the repository of SITE is what the server serves as /NAME
publish() { rm -rf "${WEB:?}/$1"; cp -a "$2" "$WEB/$1"; }
# forget_summary CLIENT: the client must fetch the summary again
forget_summary() { rm -rf "$BASE/clients/$1/repo/tmp/cache"; }

# ---- the test -------------------------------------------------------------------------------------------------

echo "# 1-3: sign 1.0.0 into an empty repository"
mkdir -p "$BASE/ahome"
chmod 700 "$BASE/ahome"
gpg --homedir "$BASE/ahome" --batch --quiet --import "$KEYS/A.sec" "$KEYS/K.sec" > /dev/null 2>&1
check "build 0.9.0 and verify it" mkbuild 0.9.0
check "build 1.0.0 and verify it" mkbuild 1.0.0
# commit times have a resolution of one second, and a client accepts a repository that is not older than what it has:
# real releases are further apart than the builds of this test
sleep 2
check "build 1.1.0 and verify it" mkbuild 1.1.0
check "build 1.2.0 and verify it" mkbuild 1.2.0
mksite_empty "$BASE/s0"
check "sign 1.0.0 (bootstrap)" sign 1.0.0 "$BASE/s0" "$BASE/o1" "$KEYS/K.sec" --bootstrap
site_from "$BASE/o1" 1.0.0 "$BASE/P1"
C1=$(head_of "$BASE/P1")
check "the key of the repository file is K" same "$FPR_K" "$(sed -n 's/^key_fpr=//p' "$BASE/o1/flatpak-signed.txt")"

echo "# 4: a client with the public key installs 1.0.0"
publish P "$BASE/P1"
check "remote-add with the public key" remote client P
check "install 1.0.0" install_app client
check "the installed commit is C1" same "$C1" "$(installed client)"

echo "# 5: sign 1.1.0 on top of 1.0.0, the client updates"
check "sign 1.1.0 on the published 1.0.0" sign 1.1.0 "$BASE/P1" "$BASE/o2" "$KEYS/K.sec"
site_from "$BASE/o2" 1.1.0 "$BASE/P2"
C2=$(head_of "$BASE/P2")
check "the new commit differs" test "$C1" != "$C2"
check "the parent of C2 is C1" same "$C1" "$(ostree --repo="$BASE/P2/repo" rev-parse "$APP_REF^")"
# no static delta is made (they would have to be made with the key present or left out of the signed summary)
check "no static delta was generated" sh -c "ostree --repo='$BASE/P2/repo' static-delta list | grep -q 'No static deltas'"
check "the history is kept (two commits)" same 2 "$(ostree --repo="$BASE/P2/repo" log "$APP_REF" | grep -c '^commit ')"
publish P "$BASE/P2"
check "update" fp client update --noninteractive --no-deps --no-related "$ID//master"
check "the installed commit is C2" same "$C2" "$(installed client)"
check "the client's repository has C1 as the parent of C2" same "$C1" "$(ostree --repo="$BASE/clients/client/repo" rev-parse "t:$APP_REF^")"
check "a signed bundle was made" test -s "$BASE/o2/PaletoModkit-x86_64.flatpak"

echo "# 5b: an update from a repository that has static deltas (the published one has), by the pipeline that makes none"
legacy_sign 0.9.0 "" "$BASE/L0"
legacy_sign 1.0.0 "$BASE/L0" "$BASE/L1"
site_meta "$BASE/L1" 1.0.0 "$KEYS/K.pub"
check "the base repository has static deltas" sh -c "! ostree --repo='$BASE/L1/repo' static-delta list | grep -q 'No static deltas'"
publish PD "$BASE/L1"
check "remote-add (repository with deltas)" remote clientd PD
check "install from the repository with deltas" install_app clientd
CD1=$(installed clientd)
check "sign 1.2.0 on the repository with deltas" sign 1.2.0 "$BASE/L1" "$BASE/o6" "$KEYS/K.sec"
site_from "$BASE/o6" 1.2.0 "$BASE/P3"
CD2=$(head_of "$BASE/P3")
check "the parent of the new commit is the one the client has" same "$CD1" "$(ostree --repo="$BASE/P3/repo" rev-parse "$APP_REF^")"
publish PD "$BASE/P3"
check "update from the repository with deltas" fp clientd update --noninteractive --no-deps --no-related "$ID//master"
check "the client has the new commit" same "$CD2" "$(installed clientd)"
echo "# (the old deltas in the new repository: $(ostree --repo="$BASE/P3/repo" static-delta list | wc -l) lines)"

echo "# 6: the bundle"
check "install the bundle (own installation)" fp bundle install --noninteractive --no-deps --no-related --bundle "$BASE/o2/PaletoModkit-x86_64.flatpak"
check "the bundle installs C2" same "$C2" "$(installed bundle)"
# flip a byte in the middle of the bundle (one in the header would only test the parser)
size=$(stat -c %s "$BASE/o2/PaletoModkit-x86_64.flatpak")
cp "$BASE/o2/PaletoModkit-x86_64.flatpak" "$BASE/bad.flatpak"
printf '\377' | dd of="$BASE/bad.flatpak" bs=1 seek=$((size / 2)) conv=notrunc 2> /dev/null
refuse "a bundle with a flipped byte is refused" fp badbundle install --noninteractive --no-deps --no-related --bundle "$BASE/bad.flatpak"
check "nothing was installed from it" nothing_installed badbundle

echo "# 7: tampering (each client starts with a good remote, then the server serves something else)"
# tamper NAME: a client with the good 1.1.0 repository as its remote, nothing installed; WEB/T is then changed.
tamper() {
    publish T "$BASE/P2"
    remote "$1" T > "$LOG" 2>&1 || return 1
    forget_summary "$1"
}
# A file object replaced by another valid one.
swap_object() { # REPO: the object of /files/bin/hello gets the content of /metadata
    h=$(ostree --repo="$1" ls -C "$APP_REF" /files/bin/hello | awk '{ print $5 }')
    m=$(ostree --repo="$1" ls -C "$APP_REF" /metadata | awk '{ print $5 }')
    cp "$1/objects/$(printf '%s' "$m" | cut -c1-2)/$(printf '%s' "$m" | cut -c3-).filez" "$1/objects/$(printf '%s' "$h" | cut -c1-2)/$(printf '%s' "$h" | cut -c3-).filez"
}

# (a) a corrupted object (the client must hash it)
check "client for a corrupted object" tamper tamper_a
swap_object "$WEB/T/repo"
refuse "a corrupted object is refused" install_app tamper_a
check "nothing is installed (corrupted object)" nothing_installed tamper_a

# (b) the summary regenerated without a signature
check "client for an unsigned summary" tamper tamper_b
rm -f "$WEB/T/repo/summary.sig" "$WEB/T/repo/summary.idx.sig"
flatpak build-update-repo "$WEB/T/repo" > /dev/null 2>&1
check "the summary has no signature now" test ! -e "$WEB/T/repo/summary.sig"
refuse "an unsigned summary is refused" install_app tamper_b
check "nothing is installed (unsigned summary)" nothing_installed tamper_b

# (c) a commit and summary signed by another key (A)
mkdir -p "$BASE/u2"
tar -C "$BASE/u2" -xf "$BASE/b/1.2.0/flatpak-unsigned-repo.tar"
check "client for a commit signed by A" tamper tamper_c
rm -rf "$WEB/T"
mkdir -p "$WEB/T"
ostree --repo="$WEB/T/repo" init --mode=archive-z2
flatpak build-commit-from --untrusted --src-repo="$BASE/u2/repo" --gpg-sign="$FPR_A" --gpg-homedir="$BASE/ahome" --no-update-summary "$WEB/T/repo" "$APP_REF" > /dev/null 2>&1
flatpak build-update-repo --gpg-sign="$FPR_A" --gpg-homedir="$BASE/ahome" "$WEB/T/repo" > /dev/null 2>&1
refuse "a commit signed by another key is refused" install_app tamper_c
check "nothing is installed (key A)" nothing_installed tamper_c

# (d) an unsigned commit under a summary signed by K
check "client for an unsigned commit" tamper tamper_d
rm -rf "$WEB/T"
mkdir -p "$WEB/T"
ostree --repo="$WEB/T/repo" init --mode=archive-z2
flatpak build-commit-from --untrusted --src-repo="$BASE/u2/repo" --no-update-summary "$WEB/T/repo" "$APP_REF" > /dev/null 2>&1
flatpak build-update-repo --gpg-sign="$FPR_K" --gpg-homedir="$BASE/ahome" "$WEB/T/repo" > /dev/null 2>&1
check "the summary is signed (K)" test -s "$WEB/T/repo/summary.sig"
refuse "an unsigned commit under a signed summary is refused" install_app tamper_d
check "nothing is installed (unsigned commit)" nothing_installed tamper_d

# (e) a replay: the valid, signed 1.0.0 is offered to the client that has 1.1.0
publish P "$BASE/P1"
forget_summary client
fp client update --noninteractive --no-deps --no-related "$ID//master" > "$LOG" 2>&1 || true
check "the older repository does not replace the installed commit" same "$C2" "$(installed client)"

echo "# 8: the signing script's own rules"
# 1.0.0 on top of the published 1.1.0: refused with exit 3
rc=0
sign 1.0.0 "$BASE/P2" "$BASE/o3" "$KEYS/K.sec" > "$LOG" 2>&1 || rc=$?
check "signing 1.0.0 over 1.1.0 exits 3" same 3 "$rc"
# the same tag and commit again: nothing is signed
rc=0
sign 1.1.0 "$BASE/P2" "$BASE/o4" "$KEYS/K.sec" > "$LOG" 2>&1 || rc=$?
check "signing 1.1.0 again is a no-op (exit 0, skip=1)" sh -c "[ $rc = 0 ] && grep -qx skip=1 '$BASE/o4/flatpak-signed.txt'"
# a key that is not the pinned one: exit 4
rc=0
sign 1.2.0 "$BASE/P2" "$BASE/o5" "$KEYS/A.sec" > "$LOG" 2>&1 || rc=$?
check "signing with another key exits 4" same 4 "$rc"
check "no keyring is left behind" test -z "$(find "$BASE/rt" -maxdepth 1 -name 'gnupg.*' -o -maxdepth 1 -name 'flatpak-sign.*'; find /dev/shm -maxdepth 1 -name 'paleto-gnupg.*' -user "$(id -u)")"

echo "# the real installation"
REAL_AFTER=$(real_listing)
check "the real ~/.local/share/flatpak is untouched" same "$REAL_BEFORE" "$REAL_AFTER"

echo "$PASSED passed, $FAILED failed"
[ "$FAILED" = 0 ]
