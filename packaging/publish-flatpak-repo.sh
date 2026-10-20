#!/bin/sh
# Writes the files users add the Flatpak repository with, next to the signed OSTree repository built by
# build-flatpak.sh (GPG_KEY_ID set), for hosting on GitHub Pages.
# Usage: GPG_KEY_ID=<key> packaging/publish-flatpak-repo.sh <site-dir> <base-url>
#   <site-dir>/repo      the OSTree repository
#   <base-url>           where <site-dir> is served, e.g. https://the-beper.github.io/PaletoModkit
# Needs gpg and the signing key in the keyring ($GNUPGHOME).
set -eu
site=$(readlink -f "$1")
url=${2%/}
id=io.github.the_beper.PaletoModkit
: "${GPG_KEY_ID:?set GPG_KEY_ID to the signing key}"

# OSTree needs these directories and they are empty, so git does not keep them.
mkdir -p "$site/repo/refs/heads" "$site/repo/refs/mirrors" "$site/repo/refs/remotes"
key=$(gpg --export "$GPG_KEY_ID" | base64 -w0)
[ -n "$key" ] || { echo "no public key for $GPG_KEY_ID in the keyring" >&2; exit 1; }

cat > "$site/paleto-modkit.flatpakrepo" <<REPO
[Flatpak Repo]
Title=Paleto Modkit
Url=$url/repo/
Homepage=https://github.com/the-beper/PaletoModkit
Comment=Mod manager for GTA V on Linux
Description=A Linux-native replacement for OpenIV: browse and edit GTA V archives, install mods.
Icon=$url/icon.svg
GPGKey=$key
REPO

cat > "$site/paleto-modkit.flatpakref" <<REF
[Flatpak Ref]
Title=Paleto Modkit
Name=$id
Branch=master
Url=$url/repo/
SuggestRemoteName=paletomodkit
IsRuntime=false
RuntimeRepo=https://dl.flathub.org/repo/flathub.flatpakrepo
GPGKey=$key
REF

cp "$(dirname "$0")/icons/hicolor/scalable/apps/$id.svg" "$site/icon.svg"
# GitHub Pages runs Jekyll unless told not to, and Jekyll skips the dot-files OSTree uses.
touch "$site/.nojekyll"
