#!/bin/sh
# Writes the files users add the Flatpak repository with, next to the signed OSTree repository made by
# packaging/flatpak-sign.sh, for hosting on GitHub Pages.
# Usage: packaging/publish-flatpak-repo.sh <site-dir> <base-url> <pubkey-file>
#   <site-dir>/repo      the OSTree repository
#   <base-url>           where <site-dir> is served, e.g. https://the-beper.github.io/PaletoModkit
#   <pubkey-file>        the public key (binary) the repository is signed with, that goes in the files; it takes
#                        no keyring and no secret
set -eu
site=$(readlink -f "$1")
url=${2%/}
pubkey=${3:?usage: publish-flatpak-repo.sh <site-dir> <base-url> <pubkey-file>}
id=io.github.the_beper.PaletoModkit

# OSTree needs these directories and they are empty, so git does not keep them.
mkdir -p "$site/repo/refs/heads" "$site/repo/refs/mirrors" "$site/repo/refs/remotes"
[ -s "$pubkey" ] || { echo "no public key in $pubkey" >&2; exit 1; }
key=$(base64 -w0 "$pubkey")

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
