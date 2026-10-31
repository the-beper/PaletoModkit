#!/bin/sh
# Sets the version of the sources for a release, in the three places release-validate.sh compares with the tag
# (docs/release.md, Cutting a release): [workspace.package] version of Cargo.toml, every paleto-* package of
# Cargo.lock, and a new first <release> of the metainfo dated today (UTC). Never commits, tags or pushes.
#   version-bump.sh X.Y.Z|X.Y.Z-rc.N [NOTES]
#       NOTES  a text file: the first line is the summary (the <p> for software centers), every other line is
#              "- text", one item each (the notes of the GitHub release); empty lines are skipped. Needed for a
#              new <release>. A version that follows candidates of the same X.Y.Z (rc.N to rc.M, or rc.N to X.Y.Z)
#              replaces their entries and, without NOTES, keeps the notes of the newest one.
#   BUMP_ROOT=dir  the checkout (default: the one this script is in)
# The notes are plain text (no markup). The tag check sees only the tags of this clone: run git fetch --tags first.
# Exit codes: 0 done; 1 usage; 2 bad version, notes or sources; 3 refused (not above the current version or the newest
# release tag, the tag exists, uncommitted changes in a file to change). Nothing is written unless every check passed.
set -eu
. "$(dirname "$0")/release-lib.sh"

[ $# -ge 1 ] && [ $# -le 2 ] || die 1 "usage: $PROG X.Y.Z|X.Y.Z-rc.N [NOTES]"
NEW=$1
NOTES=
if [ $# -eq 2 ]; then
    NOTES=$2
    [ -f "$NOTES" ] || die 2 "the notes file '$NOTES' does not exist"
    NOTES=$(cd "$(dirname "$NOTES")" && pwd)/$(basename "$NOTES")
fi
ROOT=${BUMP_ROOT:-$(cd "$PKG_DIR/.." && pwd)}
cd "$ROOT"
git rev-parse --git-dir > /dev/null 2>&1 || die 1 "$ROOT is not a git checkout"
META=packaging/io.github.the_beper.PaletoModkit.metainfo.xml
for f in Cargo.toml Cargo.lock "$META"; do [ -f "$f" ] || die 2 "$f is missing"; done

semver_valid "v$NEW" || die 2 "$NEW is not X.Y.Z or X.Y.Z-rc.N (numbers without leading zeros, N from 1)"

# The readers of release-validate.sh, so what is written here is what the release checks.
ws_version() { awk '/^\[/ { s = ($0 == "[workspace.package]"); next } s && /^version *= *"/ { v = $0; sub(/^version *= *"/, "", v); sub(/".*/, "", v); print v; exit }' "$1"; }
lock_versions() {
    awk '/^\[\[package\]\]/ { n = ""; next } /^name = "paleto-/ { n = $0; sub(/^name = "/, "", n); sub(/".*/, "", n) }
        /^version = / && n != "" { v = $0; sub(/^version = "/, "", v); sub(/".*/, "", v); print n, v; n = "" }' "$1"
}

CUR=$(ws_version Cargo.toml)
semver_valid "v$CUR" || die 2 "Cargo.toml has no valid [workspace.package] version (${CUR:-none})"
[ -n "$(lock_versions Cargo.lock)" ] || die 2 "Cargo.lock lists no paleto-* package"
bad=$(lock_versions Cargo.lock | awk -v v="$CUR" '$2 != v { print $1 " " $2 }')
[ -z "$bad" ] || die 2 "Cargo.lock is not at the current version $CUR for: $bad"
# shellcheck disable=SC2046
set -- $(xml_first_release "$META")
[ "${1:-}" = "$CUR" ] || die 2 "the first <release> of $META is ${1:-none}, Cargo.toml says $CUR"

[ "$(semver_cmp "v$NEW" "v$CUR")" = 1 ] || die 3 "$NEW is not above the current version $CUR"
if git rev-parse --verify --quiet "refs/tags/v$NEW" > /dev/null; then die 3 "the tag v$NEW exists already"; fi
TOP=
for tag in $(git tag -l 'v*'); do
    semver_valid "$tag" || continue
    if [ -z "$TOP" ] || [ "$(semver_cmp "$tag" "$TOP")" = 1 ]; then TOP=$tag; fi
done
[ -z "$TOP" ] || [ "$(semver_cmp "v$NEW" "$TOP")" = 1 ] || die 3 "$NEW is not above the newest release tag $TOP (release-validate.sh refuses a release below it)"
[ -z "$(git status --porcelain -- Cargo.toml Cargo.lock "$META")" ] || die 3 "Cargo.toml, Cargo.lock or the metainfo has uncommitted changes: commit or drop them first"

# Candidates of the same X.Y.Z are replaced, not stacked (AppStream sorts X.Y.Z-rc.N above X.Y.Z).
FOLLOW=0
if is_rc "$CUR" && [ "${CUR%-rc.*}" = "${NEW%-rc.*}" ]; then FOLLOW=1; fi
[ -n "$NOTES" ] || [ "$FOLLOW" = 1 ] || die 2 "a new <release> needs its notes: $PROG $NEW NOTES (see the header of this script)"

TYPE=
if is_rc "$NEW"; then TYPE=' type="development"'; fi
DATE=$(date -u +%F)
T=$(mktemp -d "${TMPDIR:-/tmp}/version-bump.XXXXXX")
trap 'rm -rf "$T"' EXIT

# The <description> of the new entry, from NOTES.
if [ -n "$NOTES" ]; then
    awk 'function esc(s) { gsub(/&/, "\\&amp;", s); gsub(/</, "\\&lt;", s); gsub(/>/, "\\&gt;", s); return s }
        { sub(/\r$/, ""); gsub(/\t/, " "); sub(/ +$/, "") }
        /[[:cntrl:]]/ { print "line " NR " has a control character" > "/dev/stderr"; err = 1; exit 1 }
        /^ *$/ { next }
        !p { if ($0 ~ /^- /) { print "the first line must be the summary, not an item" > "/dev/stderr"; err = 1; exit 1 }
             s = $0; sub(/^[ \t]+/, "", s); p = 1; print "        <p>" esc(s) "</p>"; print "        <ul>"; next }
        /^- / { s = substr($0, 3); sub(/^[ \t]+/, "", s); print "          <li>" esc(s) "</li>"; n++; next }
        { print "line " NR " is not an item (\"- text\"): " $0 > "/dev/stderr"; err = 1; exit 1 }
        END { if (err) exit 1; if (!p || !n) { print "the notes need a summary and at least one item" > "/dev/stderr"; exit 1 } print "        </ul>" }' \
        "$NOTES" > "$T/desc" || die 2 "bad notes in $NOTES"
fi

# Cargo.toml and Cargo.lock.
awk -v new="$NEW" '/^\[/ { s = ($0 == "[workspace.package]") } s && !done && /^version *= *"/ { sub(/"[^"]*"/, "\"" new "\""); done = 1 } { print }' Cargo.toml > "$T/toml"
awk -v cur="$CUR" -v new="$NEW" '/^\[\[package\]\]/ { n = 0 } /^name = "paleto-/ { n = 1 } n && $0 == "version = \"" cur "\"" { $0 = "version = \"" new "\""; n = 0 } { print }' Cargo.lock > "$T/lock"

# The metainfo: the new entry goes first; when following candidates, their entries go and, without NOTES, the
# newest one's <description> stays (minus a "Release candidate: " opening of its summary for a stable version).
awk -v new="$NEW" -v date="$DATE" -v type="$TYPE" -v follow="$FOLLOW" -v core="${NEW%-rc.*}" -v desc="$T/desc" -v notes="$NOTES" '
    function head() { print "    <release version=\"" new "\" date=\"" date "\"" type ">" }
    function own(line) { return index(line, "<release version=\"" core "-rc.") > 0 }
    /^  <releases>/ { print; inrel = 1
        if (notes != "") { head(); print "      <description>"; while ((getline l < desc) > 0) print l; print "      </description>"; print "    </release>"; wrote = 1 }
        next }
    inrel && follow && /^    <release / && own($0) { skip = 1; if (!wrote) { head(); keep = 1; wrote = 1 } next }
    skip && /^    <\/release>/ { if (keep) print; skip = 0; keep = 0; next }
    skip && keep { if (type == "" && $0 ~ /<p>Release candidate: /) { sub(/<p>Release candidate: /, "<p>", $0); i = index($0, "<p>") + 3; $0 = substr($0, 1, i - 1) toupper(substr($0, i, 1)) substr($0, i + 1) } print; next }
    skip { next }
    /^  <\/releases>/ { inrel = 0 }
    { print }' "$META" > "$T/meta"
grep -q "<release version=\"$NEW\"" "$T/meta" || die 2 "no <releases> block found in $META"

# Read the new files back as release-validate.sh does, before any is written.
[ "$(ws_version "$T/toml")" = "$NEW" ] || die 2 "could not set the version in Cargo.toml"
bad=$(lock_versions "$T/lock" | awk -v v="$NEW" '$2 != v { print $1 " " $2 }')
[ -z "$bad" ] || die 2 "could not set the version in Cargo.lock for: $bad"
# shellcheck disable=SC2046
set -- $(xml_first_release "$T/meta")
[ "$1 $2" = "$NEW $DATE" ] || die 2 "the first <release> of the new $META would not be $NEW of $DATE"
if is_rc "$NEW"; then [ "$3" = development ] || die 2 "the new <release> lacks type=\"development\""; else [ "$3" = stable ] || die 2 "the new <release> has type=\"development\""; fi

cp "$T/toml" Cargo.toml && cp "$T/lock" Cargo.lock && cp "$T/meta" "$META" || die 2 "could not write the files; undo with: git checkout -- Cargo.toml Cargo.lock $META"

echo "$PROG: $CUR -> $NEW in Cargo.toml, Cargo.lock and $META ($DATE)"
echo "next: review git diff, commit (\"Release $NEW: ...\"), merge through a pull request, then tag v$NEW (docs/release.md)"
