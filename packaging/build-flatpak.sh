#!/bin/sh
# Builds the Flatpak bundle, PaletoModkit-<arch>.flatpak, with flatpak-builder in
# the Flathub build image (podman or docker), as the release workflow does.
#   WORK=dir   builder cache, repository and bundle (default ~/.cache/paleto-flatpak)
#   OUT=dir    where the bundle is copied (default dist/)
#   CPUS=n     cores the build may use, to leave the rest of the machine free (default: all)
#   GPG_KEY_ID=key  (exported, with GNUPGHOME set) sign the repository commit and summary with this key from
#              that keyring, which the container may write to; WORK/repo is reused if present, so the new
#              commit is added to it
# Needs the network (crates, NuGet packages, the Flathub runtime) and about 15 GB.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${WORK:-${XDG_CACHE_HOME:-$HOME/.cache}/paleto-flatpak}
OUT=${OUT:-$ROOT/dist}
ENGINE=$(command -v podman || command -v docker) || { echo "podman or docker is needed" >&2; exit 1; }
IMAGE=ghcr.io/flathub-infra/flatpak-github-actions:freedesktop-26.08
BUNDLE=PaletoModkit-x86_64.flatpak
mkdir -p "$WORK" "$OUT"

"$ENGINE" run --rm --privileged ${CPUS:+--cpus=$CPUS -e JOBS=$CPUS} ${GPG_KEY_ID:+-e GPG_KEY_ID -e GNUPGHOME=/gnupg -v "${GNUPGHOME:?set GNUPGHOME to the keyring holding GPG_KEY_ID}":/gnupg} --security-opt label=disable -v "$ROOT":/src -v "$WORK":/work -w /src "$IMAGE" sh -c '
set -e
git config --global --add safe.directory "*"
./packaging/flatpak-cargo-sources.sh
flatpak remote-add --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
flatpak install -y --noninteractive flathub org.freedesktop.Sdk//26.08 org.freedesktop.Sdk.Extension.rust-stable//26.08 org.freedesktop.Sdk.Extension.dotnet10//26.08
./packaging/flatpak-nuget-sources.sh
flatpak-builder --jobs=${JOBS:-$(nproc)} --repo=/work/repo --disable-rofiles-fuse --force-clean --default-branch=master --arch=x86_64 --state-dir=/work/state ${GPG_KEY_ID:+--gpg-sign=$GPG_KEY_ID} /work/build packaging/io.github.the_beper.PaletoModkit.yml
[ -z "${GPG_KEY_ID:-}" ] || flatpak build-update-repo --gpg-sign="$GPG_KEY_ID" --generate-static-deltas --prune --prune-depth=3 /work/repo
flatpak build-bundle /work/repo /work/'"$BUNDLE"' io.github.the_beper.PaletoModkit master --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo
'
cp "$WORK/$BUNDLE" "$OUT/$BUNDLE"
ls -l "$OUT/$BUNDLE"
