#!/bin/sh
# Checks what the build jobs and the signing job hand over. Everything downloaded here is untrusted: it is only
# read (listed, hashed, unpacked after a path check, pulled by ostree), never run.
#   release-verify.sh builds --tag T --commit C [--linux DIR] --flatpak DIR --out DIR
#       DIR of --linux: PaletoModkit-x86_64.tar.gz, .AppImage, .AppImage.zsync, linux-build.txt (nothing else);
#       DIR of --flatpak: flatpak-unsigned-repo.tar, flatpak-build.txt (nothing else).
#       Writes OUT/release-manifest.txt, the hashes everything later is compared with. Without --linux (the
#       publish-flatpak mode) only the Flatpak is checked.
#   release-verify.sh final --tag T --commit C --in DIR --out DIR
#       DIR of --in: release-manifest.txt, flatpak-signed.txt, flatpak-signed-repo.tar, flatpak-pubkey.gpg,
#       PaletoModkit-x86_64.flatpak and the Linux files. The signed repository is pulled like a client does,
#       with the public key only. Writes OUT with exactly the RELEASE_FILES and SHA256SUMS (only the bundle
#       when the manifest has no Linux files).
# With GITHUB_OUTPUT set, "builds" writes manifest_sha256 (the hash of release-manifest.txt) to it: the later jobs get the
# manifest as a run artifact, which a compromised job could replace, and check it against this job output.
# Needs ostree, gpg (the public key only), tar. Exit codes: see release-lib.sh.
set -eu
# shellcheck source=/dev/null
. "$(dirname "$0")/release-lib.sh"

[ $# -ge 1 ] || die 1 "usage: $0 builds|final ..."
CMD=$1
shift
TAG="" COMMIT="" LINUX="" FLATPAK="" IN="" OUT=""
while [ $# -gt 0 ]; do
    case $1 in
        --tag) TAG=${2:?}; shift 2 ;;
        --commit) COMMIT=${2:?}; shift 2 ;;
        --linux) LINUX=${2:?}; shift 2 ;;
        --flatpak) FLATPAK=${2:?}; shift 2 ;;
        --in) IN=${2:?}; shift 2 ;;
        --out) OUT=${2:?}; shift 2 ;;
        *) die 1 "unknown argument $1" ;;
    esac
done
[ -n "$TAG" ] && [ -n "$COMMIT" ] && [ -n "$OUT" ] || die 1 "--tag, --commit and --out are needed"
semver_valid "$TAG" || die 2 "$TAG is not a release tag"
is_hex "$COMMIT" 40 || die 2 "$COMMIT is not a commit"
release_init
VERSION=${TAG#v}
TARBALL=PaletoModkit-x86_64.tar.gz
APPIMAGE=PaletoModkit-x86_64.AppImage
ZSYNC=PaletoModkit-x86_64.AppImage.zsync
BUNDLE_FILE=PaletoModkit-x86_64.flatpak
SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/release-verify.XXXXXX")
trap 'rm -rf "$SCRATCH"' EXIT

# only_files DIR NAME...: DIR holds exactly these regular files.
only_files() {
    d=$1
    shift
    [ -d "$d" ] || die 2 "$d is not a directory"
    for f in "$@"; do [ -f "$d/$f" ] && [ ! -L "$d/$f" ] || die 2 "$d/$f is missing"; done
    have=$(cd "$d" && find . -mindepth 1 -maxdepth 1 | sed 's|^\./||' | LC_ALL=C sort)
    want=$(printf '%s\n' "$@" | LC_ALL=C sort)
    [ "$have" = "$want" ] || die 2 "$d holds other files than: $*"
}

check_buildfile() { # FILE: format, version and commit of a build description
    [ "$(kv_get "$1" format)" = 1 ] || die 2 "$1: unknown format"
    [ "$(kv_get "$1" version)" = "$VERSION" ] || die 2 "$1: version $(kv_get "$1" version), not $VERSION"
    [ "$(kv_get "$1" commit)" = "$COMMIT" ] || die 2 "$1: built from $(kv_get "$1" commit), not $COMMIT"
}

check_linux() { # DIR
    only_files "$1" "$TARBALL" "$APPIMAGE" "$ZSYNC" linux-build.txt
    check_buildfile "$1/linux-build.txt"
    [ "$(kv_get "$1/linux-build.txt" arch)" = x86_64 ] || die 2 "linux-build.txt: not an x86_64 build"
    p=paleto-modkit-$VERSION
    tar_safe "$1/$TARBALL" "$p"
    names=$(tar -tf "$1/$TARBALL")
    for f in bin/paleto-modkit bin/paleto lib/paleto-modkit/cwconv/paleto-cwconv install.sh "share/metainfo/$APP_ID.metainfo.xml"; do
        printf '%s\n' "$names" | grep -qxF "$p/$f" || die 2 "the tarball has no $p/$f"
    done
    top=$(tar -xzOf "$1/$TARBALL" "$p/share/metainfo/$APP_ID.metainfo.xml" | xml_first_release /dev/stdin | cut -d' ' -f1)
    [ "$top" = "$VERSION" ] || die 2 "the metainfo in the tarball has ${top:-no release} on top, not $VERSION"
    # AppImage: ELF, type 2 (AI 02 at offset 8), with the update information AppImageUpdate reads.
    [ "$(head -c 4 "$1/$APPIMAGE" | od -An -tx1 | tr -d ' \n')" = 7f454c46 ] || die 2 "the AppImage is not an ELF file"
    [ "$(dd if="$1/$APPIMAGE" bs=1 skip=8 count=3 2> /dev/null | od -An -tx1 | tr -d ' \n')" = 414902 ] || die 2 "the AppImage is not type 2"
    grep -qaF "gh-releases-zsync|the-beper|PaletoModkit|latest|$ZSYNC" "$1/$APPIMAGE" || die 2 "the AppImage has no update information"
    # zsync: describes this AppImage
    hdr=$(sed '/^$/q' "$1/$ZSYNC")
    [ "$(printf '%s\n' "$hdr" | sed -n 's/^Filename: //p')" = "$APPIMAGE" ] || die 2 "the .zsync file names another file"
    [ "$(printf '%s\n' "$hdr" | sed -n 's/^Length: //p')" = "$(stat -c %s "$1/$APPIMAGE")" ] || die 2 "the .zsync Length is not the size of the AppImage"
    [ "$(printf '%s\n' "$hdr" | sed -n 's/^SHA-1: //p')" = "$(sha1sum "$1/$APPIMAGE" | cut -d' ' -f1)" ] || die 2 "the .zsync SHA-1 is not the one of the AppImage"
}

case $CMD in
builds)
    [ -n "$FLATPAK" ] || die 1 "--flatpak is needed"
    if [ -n "$LINUX" ]; then check_linux "$LINUX"; fi
    only_files "$FLATPAK" flatpak-unsigned-repo.tar flatpak-build.txt
    check_buildfile "$FLATPAK/flatpak-build.txt"
    extract_repo_tar "$FLATPAK/flatpak-unsigned-repo.tar" "$SCRATCH/u" deltas
    check_unsigned_repo "$SCRATCH/u/repo" "$VERSION"
    [ "$(ostree --repo="$SCRATCH/u/repo" rev-parse "$APP_REF")" = "$(kv_get "$FLATPAK/flatpak-build.txt" app_commit)" ] || die 2 "flatpak-build.txt: another app commit than the repository"
    mkdir -p "$OUT"
    {
        echo "format=1"
        echo "tag=$TAG"
        echo "version=$VERSION"
        echo "commit=$COMMIT"
        if [ -n "$LINUX" ]; then
            echo "linux=1"
            echo "tarball_sha256=$(sha256_of "$LINUX/$TARBALL")"
            echo "appimage_sha256=$(sha256_of "$LINUX/$APPIMAGE")"
            echo "zsync_sha256=$(sha256_of "$LINUX/$ZSYNC")"
        else
            echo "linux=0"
        fi
        echo "flatpak_unsigned_tar_sha256=$(sha256_of "$FLATPAK/flatpak-unsigned-repo.tar")"
        echo "flatpak_app_commit=$(ostree --repo="$SCRATCH/u/repo" rev-parse "$APP_REF")"
        echo "flatpak_debug_commit=$(ostree --repo="$SCRATCH/u/repo" rev-parse "$DEBUG_REF")"
        echo "flatpak_app_content=$(content_sum "$SCRATCH/u/repo" "$APP_REF")"
        echo "flatpak_debug_content=$(content_sum "$SCRATCH/u/repo" "$DEBUG_REF")"
        echo "flatpak_app_xa=$(xa_sum "$SCRATCH/u/repo" "$APP_REF")"
        echo "flatpak_debug_xa=$(xa_sum "$SCRATCH/u/repo" "$DEBUG_REF")"
    } > "$OUT/release-manifest.txt"
    job_output manifest_sha256 "$(sha256_of "$OUT/release-manifest.txt")"
    echo "verified the builds of $TAG"
    ;;
final)
    [ -n "$IN" ] || die 1 "--in is needed"
    M=$IN/release-manifest.txt
    S=$IN/flatpak-signed.txt
    [ -f "$M" ] && [ -f "$S" ] || die 2 "release-manifest.txt or flatpak-signed.txt is missing in $IN"
    for f in "$M" "$S"; do
        [ "$(kv_get "$f" tag)" = "$TAG" ] && [ "$(kv_get "$f" commit)" = "$COMMIT" ] || die 2 "$f is not for $TAG at $COMMIT"
    done
    [ "$(kv_get "$S" skip)" != 1 ] || die 2 "the signing job skipped $TAG: there is nothing to verify"
    [ "$(kv_get "$S" unsigned_tar_sha256)" = "$(kv_get "$M" flatpak_unsigned_tar_sha256)" ] || die 2 "the signed repository does not come from the verified build"
    linux=$(kv_get "$M" linux)
    for f in flatpak-signed-repo.tar flatpak-pubkey.gpg "$BUNDLE_FILE"; do
        [ -f "$IN/$f" ] && [ ! -L "$IN/$f" ] || die 2 "$IN/$f is missing"
    done
    [ "$(sha256_of "$IN/flatpak-pubkey.gpg")" = "$(kv_get "$S" pubkey_sha256)" ] || die 2 "flatpak-pubkey.gpg does not have the recorded hash (pubkey_sha256): its bytes go into the files users add the repository with"
    [ "$(sha256_of "$IN/flatpak-signed-repo.tar")" = "$(kv_get "$S" repo_tar_sha256)" ] || die 2 "flatpak-signed-repo.tar does not have the recorded hash"
    [ "$(sha256_of "$IN/$BUNDLE_FILE")" = "$(kv_get "$S" bundle_sha256)" ] || die 2 "the Flatpak bundle does not have the recorded hash"
    [ "$(stat -c %s "$IN/$BUNDLE_FILE")" = "$(kv_get "$S" bundle_size)" ] || die 2 "the Flatpak bundle does not have the recorded size"
    fpr=$(pubkey_fpr "$IN/flatpak-pubkey.gpg")
    want=$(conf_hex FLATPAK_KEY_FPR 40 | tr 'a-f' 'A-F')
    [ "$fpr" = "$want" ] || die 2 "flatpak-pubkey.gpg is the key ${fpr:-unknown}, not $want"
    [ "$(kv_get "$S" key_fpr)" = "$want" ] || die 2 "flatpak-signed.txt names another key than $want"
    extract_repo_tar "$IN/flatpak-signed-repo.tar" "$SCRATCH/s"
    verify_signed_repo "$SCRATCH/s/repo" "$IN/flatpak-pubkey.gpg" "$SCRATCH" "$VERSION"
    R=$SCRATCH/s/repo
    [ "$(ostree --repo="$R" rev-parse "$APP_REF")" = "$(kv_get "$S" app_commit)" ] || die 2 "the app commit is not the recorded one"
    [ "$(ostree --repo="$R" rev-parse "$DEBUG_REF")" = "$(kv_get "$S" debug_commit)" ] || die 2 "the Debug commit is not the recorded one"
    [ "$(content_sum "$R" "$APP_REF")" = "$(kv_get "$M" flatpak_app_content)" ] || die 2 "the signed app content is not the verified build's"
    [ "$(content_sum "$R" "$DEBUG_REF")" = "$(kv_get "$M" flatpak_debug_content)" ] || die 2 "the signed Debug content is not the verified build's"
    if [ "$linux" = 1 ]; then
        for f in "$TARBALL:tarball_sha256" "$APPIMAGE:appimage_sha256" "$ZSYNC:zsync_sha256"; do
            [ -f "$IN/${f%%:*}" ] && [ ! -L "$IN/${f%%:*}" ] || die 2 "$IN/${f%%:*} is missing"
            [ "$(sha256_of "$IN/${f%%:*}")" = "$(kv_get "$M" "${f##*:}")" ] || die 2 "${f%%:*} is not the verified build"
        done
        files=$(conf RELEASE_FILES)
        [ -n "$files" ] || die 2 "RELEASE_FILES is empty"
    else
        files=$BUNDLE_FILE
    fi
    rm -rf "$OUT"
    mkdir -p "$OUT"
    for f in $files; do
        [ -f "$IN/$f" ] && [ ! -L "$IN/$f" ] || die 2 "$IN/$f is missing (RELEASE_FILES)"
        cp "$IN/$f" "$OUT/$f"
    done
    if [ "$linux" = 1 ]; then
        # shellcheck disable=SC2086
        (cd "$OUT" && sha256sum -- $files | LC_ALL=C sort -k2 > SHA256SUMS && sha256sum --strict -c SHA256SUMS > /dev/null) || die 2 "SHA256SUMS does not check"
        # shellcheck disable=SC2086
        [ "$(wc -l < "$OUT/SHA256SUMS")" = "$(printf '%s\n' $files | wc -l)" ] || die 2 "SHA256SUMS has not one line per file"
    fi
    echo "verified the release files of $TAG"
    ;;
*) die 1 "unknown command $CMD" ;;
esac
