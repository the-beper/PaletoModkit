#!/bin/sh
# Builds the Flatpak, unsigned, with flatpak-builder in the Flathub build image (podman, or docker for local
# use), as the release workflow does. Signing is a separate step with the key (packaging/flatpak-sign.sh): no
# key ever enters the build container, and the script refuses to run with GPG_KEY_ID or GNUPGHOME set.
#   WORK=dir        builder cache, Flatpak installation (WORK/flatpak) and repository (default ~/.cache/paleto-flatpak)
#   OUT=dir         where the results are copied (default dist/)
#   CPUS=n          cores the build may use, to leave the rest of the machine free (default: all)
#   BUNDLE=1|0      make the unsigned bundle PaletoModkit-x86_64.flatpak in OUT (default 1, for local use)
#   EXPORT_REPO=1   release mode: start from an empty repository, write flatpak-unsigned-repo.tar and
#                   flatpak-build.txt to OUT (what packaging/release-verify.sh checks), and refuse (exit 2) if a
#                   pin of packaging/release.conf (image, SDK and extension commits) is empty
#   RELEASE_CONF    another pins file
#   ALLOW_ROOTFUL=1 accept docker or rootful podman (local use; the container is privileged, which is only
#                   acceptable inside a rootless user namespace)
#   FLATPAK_CONTAINER_MODE=minimal  instead of --privileged, only the seccomp and fuse options flatpak-builder
#                   needs (untested on a runner: bwrap may fail to create a namespace)
# The image is used by digest, and inside it the SDK and its extensions are updated to the commits in
# release.conf and checked with flatpak info; the installation is in WORK, not in the image's layers (see below).
# Needs the network (crates, NuGet packages, the Flathub runtime) and about 15 GB.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
# shellcheck source=/dev/null
. "$ROOT/packaging/release-lib.sh"
WORK=${WORK:-${XDG_CACHE_HOME:-$HOME/.cache}/paleto-flatpak}
OUT=${OUT:-$ROOT/dist}
BUNDLE=${BUNDLE:-1}
EXPORT_REPO=${EXPORT_REPO:-0}
BUNDLE_FILE=PaletoModkit-x86_64.flatpak

[ -z "${GPG_KEY_ID:-}" ] && [ -z "${GNUPGHOME:-}" ] || die 1 "GPG_KEY_ID or GNUPGHOME is set: the build is unsigned and never sees a key (see packaging/flatpak-sign.sh)"
ID=$(conf APP_ID)
IMAGE=$(conf FLATPAK_IMAGE)
SDK_COMMIT=$(conf SDK_COMMIT)
RUST_EXT_COMMIT=$(conf RUST_EXT_COMMIT)
DOTNET_EXT_COMMIT=$(conf DOTNET_EXT_COMMIT)
[ -n "$IMAGE" ] || die 2 "FLATPAK_IMAGE is empty in $RELEASE_CONF"
for p in SDK_COMMIT RUST_EXT_COMMIT DOTNET_EXT_COMMIT; do
    eval "v=\$$p"
    if [ -n "$v" ]; then
        is_hex "$v" 64 || die 2 "$p in $RELEASE_CONF is not a commit"
    elif [ "$EXPORT_REPO" = 1 ]; then
        die 2 "$p is empty in $RELEASE_CONF: a release build needs every pin"
    else
        echo "warning: $p is empty in $RELEASE_CONF, building with the latest version" >&2
    fi
done
if [ "$EXPORT_REPO" = 1 ]; then
    case $IMAGE in *@sha256:*) ;; *) die 2 "FLATPAK_IMAGE is not pinned by digest" ;; esac
fi
export SDK_COMMIT RUST_EXT_COMMIT DOTNET_EXT_COMMIT

# The container engine and what it may do. Rootless podman is the only place --privileged is fine: root in
# the container is the runner user outside it.
choose_engine() {
    ENGINE=${CONTAINER_ENGINE:-$(command -v podman || command -v docker)} || die 1 "podman or docker is needed"
    rootless=false
    case $(basename "$ENGINE") in
        podman) rootless=$("$ENGINE" info --format '{{.Host.Security.Rootless}}' 2> /dev/null || echo false) ;;
    esac
    if [ "$rootless" != true ] && [ "${ALLOW_ROOTFUL:-}" != 1 ]; then
        die 1 "$ENGINE is not rootless podman: a privileged container would be root on this machine (ALLOW_ROOTFUL=1 to accept, locally)"
    fi
    if [ "${FLATPAK_CONTAINER_MODE:-}" = minimal ]; then
        PRIV="--security-opt seccomp=unconfined --security-opt unmask=ALL --device /dev/fuse"
    else
        PRIV=--privileged
    fi
}

# What runs in the container (the only part this machine cannot test without podman). Variables come in with -e.
container_script() {
    cat <<'SCRIPT'
set -e
git config --global --add safe.directory /src
git config --global --add safe.directory /src/third_party/codewalker
./packaging/flatpak-cargo-sources.sh
# The Flatpak system installation lives in the bind-mounted work folder: the image ships the SDK in its overlay
# layers, and updating it there fails with EXDEV (rename across layers). libflatpak, so flatpak and
# flatpak-builder, read FLATPAK_SYSTEM_DIR; the image's own installation is not used.
mkdir -p /work/flatpak
export FLATPAK_SYSTEM_DIR=/work/flatpak
flatpak remote-add --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
SDK=org.freedesktop.Sdk//26.08
RUST=org.freedesktop.Sdk.Extension.rust-stable//26.08
DOTNET=org.freedesktop.Sdk.Extension.dotnet10//26.08
flatpak install -y --noninteractive flathub "$SDK" "$RUST" "$DOTNET"
# The build tools at the commits in release.conf, checked, not trusted.
pin() {
    [ -z "$2" ] || flatpak update -y --noninteractive --commit="$2" "$1"
    [ -z "$2" ] || [ "$(flatpak info --show-commit "$1")" = "$2" ] || { echo "$1 is not at the pinned commit $2" >&2; exit 2; }
}
pin "$SDK" "$SDK_COMMIT"
pin "$RUST" "$RUST_EXT_COMMIT"
pin "$DOTNET" "$DOTNET_EXT_COMMIT"
./packaging/flatpak-nuget-sources.sh
flatpak-builder --jobs="${JOBS:-$(nproc)}" --repo=/work/repo --disable-rofiles-fuse --force-clean --default-branch=master --arch=x86_64 --state-dir=/work/state /work/build packaging/io.github.the_beper.PaletoModkit.yml
flatpak build-update-repo --generate-static-deltas --prune --prune-depth=3 /work/repo
{
    echo "sdk_commit=$(flatpak info --show-commit "$SDK")"
    echo "rust_ext_commit=$(flatpak info --show-commit "$RUST")"
    echo "dotnet_ext_commit=$(flatpak info --show-commit "$DOTNET")"
    echo "app_commit=$(ostree --repo=/work/repo rev-parse "app/$APP_ID/x86_64/master")"
    echo "debug_commit=$(ostree --repo=/work/repo rev-parse "runtime/$APP_ID.Debug/x86_64/master")"
} > /work/build-info.txt
[ "$BUNDLE" != 1 ] || flatpak build-bundle /work/repo "/work/$BUNDLE_FILE" "$APP_ID" master --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo
SCRIPT
}

run_container() {
    choose_engine
    # shellcheck disable=SC2086
    set -- run --rm $PRIV ${CPUS:+--cpus=$CPUS -e JOBS=$CPUS} -e SDK_COMMIT -e RUST_EXT_COMMIT -e DOTNET_EXT_COMMIT \
        -e "BUNDLE=$BUNDLE" -e "BUNDLE_FILE=$BUNDLE_FILE" -e "APP_ID=$ID" --security-opt label=disable \
        -v "$ROOT":/src -v "$WORK":/work -w /src "$IMAGE" sh -c "$(container_script)"
    "$ENGINE" "$@"
}

mkdir -p "$WORK" "$OUT"
if [ "$EXPORT_REPO" = 1 ]; then rm -rf "${WORK:?}/repo" "${WORK:?}/build-info.txt"; fi
run_container

if [ "$BUNDLE" = 1 ]; then
    cp "$WORK/$BUNDLE_FILE" "$OUT/$BUNDLE_FILE"
    ls -l "$OUT/$BUNDLE_FILE"
fi
if [ "$EXPORT_REPO" = 1 ]; then
    [ -f "$WORK/build-info.txt" ] || die 1 "the build left no build-info.txt"
    rm -rf "$WORK/repo/tmp"
    mkdir "$WORK/repo/tmp"
    tar -C "$WORK" --owner=0 --group=0 --numeric-owner -cf "$OUT/flatpak-unsigned-repo.tar" repo
    {
        echo "format=1"
        echo "version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)"
        echo "commit=$(git -C "$ROOT" rev-parse HEAD)"
        echo "image=$IMAGE"
        cat "$WORK/build-info.txt"
    } > "$OUT/flatpak-build.txt"
    ls -l "$OUT/flatpak-unsigned-repo.tar" "$OUT/flatpak-build.txt"
fi
