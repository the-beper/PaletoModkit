#!/bin/sh
# Checks packaging/ci-changes.sh: which changes make CI run. No network, no git.
#   sh packaging/tests/ci-changes-test.sh
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
fail=0

# expect <true|false> <file>... : the verdict for a change of exactly these files ("-" for none); the verdict goes to
# stdout even inside GitHub Actions, which sets GITHUB_OUTPUT for every step
expect() {
    want=$1
    shift
    got=$(for f in "$@"; do [ "$f" = - ] || printf '%s\n' "$f"; done | GITHUB_OUTPUT=/dev/stdout sh "$HERE/../ci-changes.sh")
    if [ "$got" = "code=$want" ]; then
        echo "ok   $want: $*"
    else
        echo "FAIL $want: $* (got $got)"
        fail=1
    fi
}

expect false README.md
expect false docs/user-guide.md docs/images/a.png screenshots/main.png LICENSE .gitignore
expect false .github/ISSUE_TEMPLATE/bug_report.yml .github/dependabot.yml crates/gui/NOTES.md
expect false .claude/CLAUDE.md
expect true .claude/flatpak-bg.sh
expect true crates/core/src/lib.rs
expect true README.md crates/core/src/lib.rs
expect true .github/workflows/ci.yml
expect true packaging/release-lib.sh
expect true Cargo.lock
expect true -

exit "$fail"
