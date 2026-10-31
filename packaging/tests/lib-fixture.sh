#!/bin/sh
# Builders of synthetic test data for packaging/tests/release-sim.sh (sourced, never run): a sandboxed
# environment, throwaway signing keys, a project that looks like this one, files that look like the build
# outputs (and are structurally valid, nothing more), and an unsigned Flatpak repository made with
# "flatpak build-export" from a synthetic app, no runtime and no network needed.

# fx_nonce: a different string every time, so that two "builds" of the same source differ, like real ones do.
fx_nonce() { od -An -N8 -tx1 /dev/urandom | tr -d ' \n'; }

# fx_env BASE: HOME, XDG_*, FLATPAK_* and git configuration under BASE, so that no tool reads or writes the
# real home or the real Flatpak installation.
fx_env() {
    mkdir -p "$1/home/.config" "$1/home/.cache" "$1/home/.local/share" "$1/flatpak/user" "$1/flatpak/system" "$1/flatpak/config" "$1/flatpak/triggers"
    HOME=$1/home
    XDG_CONFIG_HOME=$1/home/.config
    XDG_CACHE_HOME=$1/home/.cache
    XDG_DATA_HOME=$1/home/.local/share
    XDG_RUNTIME_DIR=$1/runtime
    mkdir -p "$XDG_RUNTIME_DIR"
    chmod 700 "$XDG_RUNTIME_DIR"
    FLATPAK_USER_DIR=$1/flatpak/user
    FLATPAK_SYSTEM_DIR=$1/flatpak/system
    FLATPAK_CONFIG_DIR=$1/flatpak/config
    FLATPAK_TRIGGERSDIR=$1/flatpak/triggers
    GIT_CONFIG_GLOBAL=$1/home/.gitconfig
    GIT_CONFIG_SYSTEM=/dev/null
    export HOME XDG_CONFIG_HOME XDG_CACHE_HOME XDG_DATA_HOME XDG_RUNTIME_DIR FLATPAK_USER_DIR FLATPAK_SYSTEM_DIR FLATPAK_CONFIG_DIR FLATPAK_TRIGGERSDIR GIT_CONFIG_GLOBAL GIT_CONFIG_SYSTEM
    unset DBUS_SESSION_BUS_ADDRESS GNUPGHOME GPG_KEY_ID WAYLAND_DISPLAY DISPLAY
    cat > "$GIT_CONFIG_GLOBAL" <<'GITCONF'
[user]
	name = Sim
	email = sim@example.invalid
[init]
	defaultBranch = main
[protocol "file"]
	allow = always
[advice]
	detachedHead = false
GITCONF
}

# fx_key DIR NAME: an ed25519 key without a passphrase in a keyring of its own (DIR/NAME.home), exported to
# DIR/NAME.sec (armored secret key) and DIR/NAME.pub (binary public key). Prints the fingerprint.
fx_key() {
    h=$1/$2.home
    mkdir -p "$h"
    chmod 700 "$h"
    gpg --homedir "$h" --batch --quiet --passphrase '' --quick-gen-key "Sim $2" ed25519 sign never > /dev/null 2>&1
    f=$(gpg --homedir "$h" --batch --list-secret-keys --with-colons | awk -F: '/^fpr/ { print $10; exit }')
    gpg --homedir "$h" --batch --pinentry-mode loopback --passphrase '' --armor --export-secret-keys "$f" > "$1/$2.sec" 2> /dev/null
    gpg --homedir "$h" --batch --export "$f" > "$1/$2.pub" 2> /dev/null
    gpgconf --homedir "$h" --kill all > /dev/null 2>&1 || true
    rm -rf "$h"
    printf '%s\n' "$f"
}

# fx_metainfo FILE ID VERSION DATE [development]: a minimal metainfo whose first <release> is VERSION, with one change
# in its list, followed by an older 1.0.0.
fx_metainfo() {
    {
        echo '<?xml version="1.0" encoding="UTF-8"?>'
        echo '<component type="desktop-application">'
        echo "  <id>$2</id>"
        echo '  <releases>'
        if [ -n "${5:-}" ]; then
            echo "    <release version=\"$3\" date=\"$4\" type=\"$5\">"
        else
            echo "    <release version=\"$3\" date=\"$4\">"
        fi
        echo '      <description><p>Test.</p><ul><li>A change of <code>paleto</code>.</li></ul></description>'
        echo '    </release>'
        echo '    <release version="1.0.0" date="2026-01-01"><description><p>Old.</p></description></release>'
        echo '  </releases>'
        echo '</component>'
    } > "$1"
}

# fx_set_version DIR VERSION [development]: makes the project in DIR be at VERSION (Cargo.toml, Cargo.lock, metainfo).
fx_set_version() {
    sed -i "s/^version = \".*\"/version = \"$2\"/" "$1/Cargo.toml"
    awk -v v="$2" '/^name = "paleto-/ { p = 1 } /^name = "/ && !/paleto-/ { p = 0 } /^version = / && p { $0 = "version = \"" v "\""; p = 0 } { print }' "$1/Cargo.lock" > "$1/Cargo.lock.new"
    mv "$1/Cargo.lock.new" "$1/Cargo.lock"
    type=""
    case $2 in *-rc.*) type=development ;; esac
    fx_metainfo "$1/packaging/io.github.the_beper.PaletoModkit.metainfo.xml" io.github.the_beper.PaletoModkit "$2" 2026-10-29 "$type"
}

# fx_project DIR VERSION CODEWALKER_URL REAL_PACKAGING: the files of a project with the shape the release scripts
# check, with the real scripts of this repository in packaging/ (not the submodule: the caller adds it).
fx_project() {
    mkdir -p "$1/packaging" "$1/crates"
    cp "$4"/*.sh "$4"/release-notes.py "$4"/release.conf "$1/packaging/"
    cp -r "$4/icons" "$1/packaging/icons"
    mkdir -p "$1/packaging/tests"
    cp "$4/tests/gh" "$1/packaging/tests/gh"
    printf '[workspace]\nmembers = ["crates/*"]\n\n[workspace.package]\nversion = "%s"\nedition = "2024"\n' "$2" > "$1/Cargo.toml"
    {
        echo 'version = 4'
        for n in cli core formats gui rpf; do
            printf '\n[[package]]\nname = "paleto-%s"\nversion = "%s"\n' "$n" "$2"
            mkdir -p "$1/crates/$n"
            printf '[package]\nname = "paleto-%s"\nversion.workspace = true\n' "$n" > "$1/crates/$n/Cargo.toml"
        done
        printf '\n[[package]]\nname = "serde"\nversion = "1.0.0"\n'
    } > "$1/Cargo.lock"
    fx_set_version "$1" "$2"
    echo "marker-$(fx_nonce)" > "$1/marker.txt"
}

# ---- fake build outputs ----------------------------------------------------------------------------------------

# fx_linux SRC OUT [nocwconv]: what build-release.sh would leave, structurally valid, carrying the version and
# marker of the source tree SRC.
fx_linux() {
    v=$(sed -n 's/^version = "\(.*\)"/\1/p' "$1/Cargo.toml" | head -n1)
    id=io.github.the_beper.PaletoModkit
    mkdir -p "$2"
    t=$(mktemp -d "${TMPDIR:-/tmp}/fx-linux.XXXXXX")
    p=$t/paleto-modkit-$v
    mkdir -p "$p/bin" "$p/lib/paleto-modkit/cwconv" "$p/share/metainfo"
    echo "gui $(fx_nonce)" > "$p/bin/paleto-modkit"
    echo "cli" > "$p/bin/paleto"
    [ "${3:-}" = nocwconv ] || echo "cwconv" > "$p/lib/paleto-modkit/cwconv/paleto-cwconv"
    echo "install" > "$p/install.sh"
    cp "$1/packaging/$id.metainfo.xml" "$p/share/metainfo/$id.metainfo.xml"
    cp "$1/marker.txt" "$p/MARKER"
    tar -C "$t" -czf "$2/PaletoModkit-x86_64.tar.gz" "paleto-modkit-$v"
    rm -rf "$t"
    a=$2/PaletoModkit-x86_64.AppImage
    {
        printf '\177ELF\002\001\001\000AI\002\000\000\000\000\000'
        printf 'gh-releases-zsync|the-beper|PaletoModkit|latest|PaletoModkit-x86_64.AppImage.zsync\n'
        cat "$1/marker.txt"
        fx_nonce
    } > "$a"
    {
        printf 'zsync: 0.6.2\nFilename: PaletoModkit-x86_64.AppImage\nMTime: Thu, 29 Oct 2026 10:00:00 +0000\nBlocksize: 2048\n'
        printf 'Length: %s\nHash-Lengths: 1,2,4\nURL: PaletoModkit-x86_64.AppImage\nSHA-1: %s\n\n' "$(stat -c %s "$a")" "$(sha1sum "$a" | cut -d' ' -f1)"
        printf 'blocks\n'
    } > "$2/PaletoModkit-x86_64.AppImage.zsync"
    {
        echo "format=1"
        echo "version=$v"
        echo "commit=$(git -C "$1" rev-parse HEAD)"
        echo "arch=x86_64"
        echo "appimagetool_sha256=fake"
        echo "appimage_runtime_sha256=fake"
        echo "rustc=fake"
        echo "dotnet=fake"
    } > "$2/linux-build.txt"
}

# fx_unsigned_repo DIR METAINFO ID: DIR/repo, an unsigned OSTree repository made by flatpak build-export from a
# synthetic app (and its Debug extension), as flatpak-builder leaves it after build-update-repo.
fx_unsigned_repo() {
    rm -rf "$1"
    mkdir -p "$1/app/files/bin" "$1/app/files/share/metainfo" "$1/app/export/share/metainfo" "$1/dbg/usr/lib/debug" "$1/dbg/files"
    cat > "$1/app/metadata" <<APPMETA
[Application]
name=$3
runtime=org.freedesktop.Platform/x86_64/26.08
sdk=org.freedesktop.Sdk/x86_64/26.08
command=hello
APPMETA
    printf '#!/bin/sh\necho hello\n' > "$1/app/files/bin/hello"
    echo "build $(fx_nonce)" > "$1/app/files/share/build-id"
    cp "$2" "$1/app/files/share/metainfo/$3.metainfo.xml"
    cp "$2" "$1/app/export/share/metainfo/$3.metainfo.xml"
    cat > "$1/dbg/metadata" <<DBGMETA
[Runtime]
name=$3.Debug
runtime=org.freedesktop.Platform/x86_64/26.08
sdk=org.freedesktop.Sdk/x86_64/26.08
DBGMETA
    echo "debug $(fx_nonce)" > "$1/dbg/usr/lib/debug/x"
    ostree --repo="$1/repo" init --mode=archive-z2
    flatpak build-export --no-update-summary "$1/repo" "$1/app" master > /dev/null
    flatpak build-export --no-update-summary --runtime "$1/repo" "$1/dbg" master > /dev/null
    flatpak build-update-repo "$1/repo" > /dev/null
    rm -rf "$1/app" "$1/dbg" "$1/repo/tmp"
    mkdir "$1/repo/tmp"
}

# fx_flatpak SRC OUT: what build-flatpak.sh with EXPORT_REPO=1 leaves in OUT, built from the source tree SRC.
fx_flatpak() {
    id=io.github.the_beper.PaletoModkit
    mkdir -p "$2"
    t=$(mktemp -d "${TMPDIR:-/tmp}/fx-flatpak.XXXXXX")
    fx_unsigned_repo "$t" "$1/packaging/$id.metainfo.xml" "$id"
    tar -C "$t" --owner=0 --group=0 --numeric-owner -cf "$2/flatpak-unsigned-repo.tar" repo
    {
        echo "format=1"
        echo "version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$1/Cargo.toml" | head -n1)"
        echo "commit=$(git -C "$1" rev-parse HEAD)"
        echo "image=fake"
        echo "app_commit=$(ostree --repo="$t/repo" rev-parse "app/$id/x86_64/master")"
        echo "debug_commit=$(ostree --repo="$t/repo" rev-parse "runtime/$id.Debug/x86_64/master")"
    } > "$2/flatpak-build.txt"
    rm -rf "$t"
}

# fx_delta_shim DIR MODE: DIR/ostree, a stand-in for the real ostree (found now, on the PATH of the caller) that spoils the static delta
# of --finish and passes on everything else. corrupt: a byte of the first part is flipped after "static-delta generate". unsigned: the
# delta is made in a copy of the repository without the detached metadata of its commits, as one made before the signing would be
# (no signature in its superblock), and moved in. Put DIR first on the PATH of --finish.
fx_delta_shim() {
    mkdir -p "$1"
    real=$(command -v ostree)
    case $2 in
        corrupt)
            sed "s|@REAL@|$real|" > "$1/ostree" <<'SHIM'
#!/bin/sh
"@REAL@" "$@" || exit $?
case " $* " in
    *" static-delta generate "*)
        part=$(find "${1#--repo=}/deltas" -type f -name 0 | head -n1)
        printf '\377' | dd of="$part" bs=1 seek=100 conv=notrunc 2> /dev/null
        ;;
esac
SHIM
            ;;
        unsigned)
            sed "s|@REAL@|$real|" > "$1/ostree" <<'SHIM'
#!/bin/sh
case " $* " in
    *" static-delta generate "*)
        r=${1#--repo=}
        t=$(mktemp -d "${TMPDIR:-/tmp}/shim.XXXXXX")
        cp -a "$r" "$t/repo"
        find "$t/repo/objects" -name '*.commitmeta' -delete
        shift
        "@REAL@" --repo="$t/repo" "$@" || exit $?
        mkdir -p "$r/deltas"
        cp -a "$t/repo/deltas/." "$r/deltas/"
        rm -rf "$t"
        ;;
    *) exec "@REAL@" "$@" ;;
esac
SHIM
            ;;
    esac
    chmod +x "$1/ostree"
}
