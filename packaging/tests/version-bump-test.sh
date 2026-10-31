#!/bin/sh
# Checks version-bump.sh on a small checkout of its own (a git repository with Cargo.toml, Cargo.lock and the
# metainfo): the three places move together, the notes become the new <release>, candidates are replaced, and every
# refusal leaves the files as they were. No network.
#   sh packaging/tests/version-bump-test.sh
set -eu
unset BUMP_ROOT

HERE=$(cd "$(dirname "$0")" && pwd)
T=$(mktemp -d "${TMPDIR:-/tmp}/version-bump-test.XXXXXX")
trap 'rm -rf "$T"' EXIT
fail=0
ok() { echo "ok   $1"; }
bad() {
    echo "FAIL $1"
    fail=1
}
same() { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (expected '$2', got '$3')"; fi; }
META=packaging/io.github.the_beper.PaletoModkit.metainfo.xml
TODAY=$(date -u +%F)

# fixture VERSION [TYPE]: a fresh checkout at VERSION, committed; a dependency shares the version string.
fixture() {
    R=$T/repo
    rm -rf "$R"
    mkdir -p "$R/packaging"
    cp "$HERE/../version-bump.sh" "$HERE/../release-lib.sh" "$R/packaging/"
    printf '[workspace]\nmembers = ["a", "b"]\n\n[workspace.package]\nversion = "%s"\nedition = "2024"\n\n[workspace.dependencies]\nzed = { version = "%s" }\n' "$1" "$1" > "$R/Cargo.toml"
    for n in paleto-a zed paleto-b; do printf '[[package]]\nname = "%s"\nversion = "%s"\n\n' "$n" "$1"; done > "$R/Cargo.lock"
    cat > "$R/$META" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<component type="desktop-application">
  <releases>
    <release version="$1" date="2026-01-02"${2:-}>
      <description>
        <p>Release candidate: older notes.</p>
        <ul>
          <li>Old item.</li>
        </ul>
      </description>
    </release>
    <release version="0.9.0" date="2025-12-01">
      <description>
        <p>First.</p>
      </description>
    </release>
  </releases>
</component>
EOF
    git -C "$R" init -q
    git -C "$R" add -A
    git -C "$R" -c user.name=t -c user.email=t@t commit -q -m fixture
}
bump() { (cd "$R" && sh packaging/version-bump.sh "$@") > "$T/out" 2>&1; }
state() { cat "$R/Cargo.toml" "$R/Cargo.lock" "$R/$META" | sha256sum; }
lockv() { awk -v n="$1" '$0 == "name = \"" n "\"" { getline; sub(/^version = "/, ""); sub(/"$/, ""); print }' "$R/Cargo.lock"; }
first() { awk '/<release / { print; exit }' "$R/$META" | sed 's/^ *//'; }
refused() { # refused NAME CODE ARGS...: the exit code, and nothing changed
    name=$1
    code=$2
    shift 2
    before=$(state)
    rc=0
    bump "$@" || rc=$?
    same "$name: exit $code" "$code" "$rc"
    same "$name: files unchanged" "$before" "$(state)"
}

printf 'Smaller updates & a <safer> restore.\n\n- First item.\n- Second "item" with < and &.\n' > "$T/notes"

# A stable release to the next minor, with notes.
fixture 1.6.0
bump 1.7.0 "$T/notes" || bad "1.6.0 -> 1.7.0 failed: $(cat "$T/out")"
same "Cargo.toml" 'version = "1.7.0"' "$(grep '^version' "$R/Cargo.toml")"
same "the dependency in Cargo.toml" 'zed = { version = "1.6.0" }' "$(grep '^zed' "$R/Cargo.toml")"
same "Cargo.lock paleto-a" 1.7.0 "$(lockv paleto-a)"
same "Cargo.lock paleto-b" 1.7.0 "$(lockv paleto-b)"
same "Cargo.lock dependency" 1.6.0 "$(lockv zed)"
same "new first <release>" "<release version=\"1.7.0\" date=\"$TODAY\">" "$(first)"
same "summary, escaped" '<p>Smaller updates &amp; a &lt;safer&gt; restore.</p>' "$(grep -m1 '<p>' "$R/$META" | sed 's/^ *//')"
same "items, escaped" '<li>First item.</li>|<li>Second "item" with &lt; and &amp;.</li>|<li>Old item.</li>' "$(grep '<li>' "$R/$META" | sed 's/^ *//' | paste -sd'|')"
same "the old entries stay" 3 "$(grep -c '<release ' "$R/$META")"
if command -v python3 > /dev/null; then
    same "release-notes.py reads the items" "$(printf '%s\n\n%s\n%s\n\n%s' "## What's Changed" "- First item." "- Second \"item\" with < and &." "**Full Changelog**: https://github.com/o/n/compare/v1.6.0...v1.7.0")" "$(python3 -I "$HERE/../release-notes.py" "$R/$META" 1.7.0 --repo o/n)"
fi
printf '  Indented summary.\n- Item.\n' > "$T/n4"
fixture 1.6.0
bump 1.7.0 "$T/n4" || bad "notes with an indented summary failed: $(cat "$T/out")"
same "the summary loses its indent" '<p>Indented summary.</p>' "$(grep -m1 '<p>' "$R/$META" | sed 's/^ *//')"

# Refusals.
fixture 1.6.0
refused "no notes for a new entry" 2 1.7.0
refused "same version" 3 1.6.0 "$T/notes"
refused "lower version" 3 1.5.9 "$T/notes"
refused "the candidate of the current version" 3 1.6.0-rc.4 "$T/notes"
for v in 1.7 v1.7.0 01.7.0 1.7.0-rc.0 1.7.0-rc.01 1.7.0-beta.1 "1.7.0 "; do refused "version '$v'" 2 "$v" "$T/notes"; done
refused "missing notes file" 2 1.7.0 "$T/none"
refused "an empty notes argument" 2 1.7.0 ""
printf -- '- An item first.\n' > "$T/n1"
refused "notes start with an item" 2 1.7.0 "$T/n1"
printf 'Summary.\nNot an item.\n' > "$T/n2"
refused "a line that is not an item" 2 1.7.0 "$T/n2"
printf 'Summary only.\n' > "$T/n3"
refused "notes without items" 2 1.7.0 "$T/n3"
refused "usage" 1
git -C "$R" tag v1.7.0
refused "the tag exists" 3 1.7.0 "$T/notes"
git -C "$R" tag -d v1.7.0 > /dev/null
git -C "$R" tag v1.8.0
refused "below the newest release tag" 3 1.7.0 "$T/notes"
git -C "$R" tag -d v1.8.0 > /dev/null
printf 'Summary.\n- An item with a \001 control character.\n' > "$T/n5"
refused "a control character in the notes" 2 1.7.0 "$T/n5"
echo "# edit" >> "$R/Cargo.lock"
refused "uncommitted Cargo.lock" 3 1.7.0 "$T/notes"
fixture 1.6.0
sed -i 's/^version = "1.6.0"/version = "1.6.1"/' "$R/Cargo.toml"
git -C "$R" -c user.name=t -c user.email=t@t commit -qam drift
refused "sources at different versions" 2 1.7.0 "$T/notes"
fixture 1.6.0
sed -i 's/^  <releases>/  <releasez>/' "$R/$META"
git -C "$R" -c user.name=t -c user.email=t@t commit -qam noblock
refused "no <releases> block" 2 1.7.0 "$T/notes"

# Candidates: a new one with notes, the next without (keeps them), then the release (keeps them, not as a candidate).
fixture 1.6.0
bump 1.7.0-rc.1 "$T/notes" || bad "1.6.0 -> 1.7.0-rc.1 failed: $(cat "$T/out")"
same "rc.1 is a development release" "<release version=\"1.7.0-rc.1\" date=\"$TODAY\" type=\"development\">" "$(first)"
git -C "$R" -c user.name=t -c user.email=t@t commit -qam rc1
bump 1.7.0-rc.2 || bad "rc.1 -> rc.2 failed: $(cat "$T/out")"
same "rc.2 replaces rc.1" "<release version=\"1.7.0-rc.2\" date=\"$TODAY\" type=\"development\">" "$(first)"
same "one candidate entry" 1 "$(grep -c '<release version="1.7.0-rc' "$R/$META")"
same "rc.2 keeps the notes" '<li>First item.</li>' "$(grep -m1 '<li>' "$R/$META" | sed 's/^ *//')"
git -C "$R" -c user.name=t -c user.email=t@t commit -qam rc2
bump 1.7.0 || bad "rc.2 -> 1.7.0 failed: $(cat "$T/out")"
same "the release replaces its candidates" "<release version=\"1.7.0\" date=\"$TODAY\">" "$(first)"
same "no candidate entry left" 0 "$(grep -c '<release version="1.7.0-rc' "$R/$META")"
same "the release keeps the notes" 2 "$(awk '/<release version="1.7.0"/,/<\/release>/' "$R/$META" | grep -c '<li>')"
same "Cargo.lock at the release" "1.7.0 1.7.0" "$(lockv paleto-a) $(lockv paleto-b)"

# From a candidate the summary loses "Release candidate: ".
fixture 2.0.0-rc.1 ' type="development"'
bump 2.0.0 || bad "2.0.0-rc.1 -> 2.0.0 failed: $(cat "$T/out")"
same "summary without the candidate opening" '<p>Older notes.</p>' "$(grep -m1 '<p>' "$R/$META" | sed 's/^ *//')"
same "only the release and the old one" 2 "$(grep -c '<release ' "$R/$META")"
fixture 2.0.0-rc.1 ' type="development"'
bump 2.1.0 "$T/notes" || bad "2.0.0-rc.1 -> 2.1.0 failed: $(cat "$T/out")"
same "another version keeps the old candidate" 3 "$(grep -c '<release ' "$R/$META")"

# Two stacked candidates (as left by hand): the release keeps the newest one's notes and drops both entries.
fixture 1.7.0-rc.2 ' type="development"'
awk '/<release version="0.9.0"/ { print "    <release version=\"1.7.0-rc.1\" date=\"2026-01-01\" type=\"development\">"; print "      <description>"; print "        <p>Release candidate: rc.1 notes.</p>"; print "      </description>"; print "    </release>" } { print }' "$R/$META" > "$T/m" && cp "$T/m" "$R/$META"
git -C "$R" -c user.name=t -c user.email=t@t commit -qam stacked
bump 1.7.0 || bad "two candidates -> 1.7.0 failed: $(cat "$T/out")"
same "both candidates replaced" "<release version=\"1.7.0\" date=\"$TODAY\">|<release version=\"0.9.0\" date=\"2025-12-01\">" "$(grep '<release ' "$R/$META" | sed 's/^ *//' | paste -sd'|')"
same "the newest candidate's notes" '<p>Older notes.</p>' "$(grep -m1 '<p>' "$R/$META" | sed 's/^ *//')"
fixture 1.7.0-rc.1 ' type="development"'
bump 1.7.0-rc.2 "$T/notes" || bad "rc.1 -> rc.2 with notes failed: $(cat "$T/out")"
same "rc.2 with notes replaces rc.1" "<release version=\"1.7.0-rc.2\" date=\"$TODAY\" type=\"development\">|<release version=\"0.9.0\" date=\"2025-12-01\">" "$(grep '<release ' "$R/$META" | sed 's/^ *//' | paste -sd'|')"
same "rc.2 has the new notes" '<li>First item.</li>' "$(grep -m1 '<li>' "$R/$META" | sed 's/^ *//')"

[ "$fail" = 0 ] && echo "version-bump-test: all passed" || { echo "version-bump-test: FAILED"; exit 1; }
