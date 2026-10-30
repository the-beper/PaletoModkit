#!/bin/sh
# Decides whether a change needs the CI jobs. Reads the changed file names, one per line, on stdin and writes
# "code=true" or "code=false" to $GITHUB_OUTPUT (stdout when it is not set).
#   git diff --name-only "$BASE" HEAD | sh packaging/ci-changes.sh
# false only when every file is documentation or repository chrome (.claude/CLAUDE.md is a *.md; the scripts of
# .claude/ are code: CI shellchecks them). An empty list means that the diff failed or says nothing, so it is true:
# when in doubt, run everything.
set -eu

out=${GITHUB_OUTPUT:-/dev/stdout}
code=false
seen=false

while IFS= read -r f; do
    [ -n "$f" ] || continue
    seen=true
    case $f in
        *.md | docs/* | screenshots/* | LICENSE | .github/ISSUE_TEMPLATE/* | .github/dependabot.yml | .gitignore) ;;
        *) code=true ;;
    esac
done
[ "$seen" = true ] || code=true

echo "code=$code" >>"$out"
