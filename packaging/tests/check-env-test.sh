#!/bin/sh
# Checks packaging/release-check-env.sh, the pre-flight check of the signing environment: saved API answers (the
# shape GitHub returns for GET /repos/OWNER/REPO/environments/NAME) and a stub of gh for the missing and unreadable
# cases. Needs jq. No network.
#   sh packaging/tests/check-env-test.sh
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
CHECK=$HERE/../release-check-env.sh
command -v jq > /dev/null || { echo "SKIP: jq is needed"; exit 77; }

T=$(mktemp -d "${TMPDIR:-/tmp}/check-env-test.XXXXXX")
trap 'rm -rf "$T"' EXIT
fail=0

# expect CODE NAME ARGS...: the check exits with CODE
expect() {
    want=$1 name=$2
    shift 2
    got=0
    "$CHECK" "$@" > "$T/out" 2>&1 || got=$?
    if [ "$got" = "$want" ]; then
        echo "ok   $name"
    else
        echo "FAIL $name: exit $got, wanted $want"
        sed 's/^/     /' "$T/out"
        fail=1
    fi
}

# The real answer, reduced: one required reviewer, selected deployment refs.
cat > "$T/good.json" <<'JSON'
{"name":"flatpak-signing","can_admins_bypass":true,
 "protection_rules":[{"id":1,"type":"required_reviewers","prevent_self_review":false,"reviewers":[{"type":"User","reviewer":{"login":"maintainer"}}]},{"id":2,"type":"branch_policy"}],
 "deployment_branch_policy":{"protected_branches":false,"custom_branch_policies":true}}
JSON
expect 0 "reviewer and selected refs" --json "$T/good.json"

# The environment GitHub makes when a job names one that is missing: no rules, no policy.
echo '{"name":"flatpak-signing","protection_rules":[],"deployment_branch_policy":null}' > "$T/bare.json"
expect 3 "unprotected environment" --json "$T/bare.json"

echo '{"name":"flatpak-signing","protection_rules":[{"id":2,"type":"branch_policy"},{"id":3,"type":"wait_timer","wait_timer":5}],"deployment_branch_policy":{"protected_branches":false,"custom_branch_policies":true}}' > "$T/noreviewer.json"
expect 3 "no required reviewer" --json "$T/noreviewer.json"

echo '{"name":"flatpak-signing","protection_rules":[{"id":1,"type":"required_reviewers","reviewers":[]}],"deployment_branch_policy":{"protected_branches":false,"custom_branch_policies":true}}' > "$T/emptyreviewers.json"
expect 3 "required_reviewers with nobody in it" --json "$T/emptyreviewers.json"

echo '{"name":"flatpak-signing","protection_rules":[{"id":1,"type":"required_reviewers","reviewers":[{"type":"User"}]}],"deployment_branch_policy":null}' > "$T/anyref.json"
expect 3 "any ref may deploy" --json "$T/anyref.json"

echo '{"name":"flatpak-signing","protection_rules":[{"id":1,"type":"required_reviewers","reviewers":[{"type":"User"}]}],"deployment_branch_policy":{"protected_branches":false,"custom_branch_policies":false}}' > "$T/allrefs.json"
expect 3 "policy object that restricts nothing" --json "$T/allrefs.json"
sed 's/"protected_branches":false,"custom_branch_policies":true/"protected_branches":true,"custom_branch_policies":false/' "$T/good.json" > "$T/protected.json"
expect 3 "protected branches only, which admit no tag" --json "$T/protected.json"

sed 's/"flatpak-signing"/"github-pages"/' "$T/good.json" > "$T/other.json"
expect 3 "answer for another environment" --json "$T/other.json"

echo '[]' > "$T/array.json"
expect 5 "not an object" --json "$T/array.json"
echo 'not json' > "$T/garbage.json"
expect 5 "not JSON" --json "$T/garbage.json"
expect 1 "unreadable file" --json "$T/missing.json"
expect 0 "another environment name" --env github-pages --json "$T/other.json"

# gh stub: behaviour chosen by GH_STUB (good, notfound, forbidden); records the path it was asked for.
mkdir "$T/bin"
cat > "$T/bin/gh" <<STUB
#!/bin/sh
[ "\$1" = api ] || exit 2
echo "\$2" > "$T/asked"
case \$GH_STUB in
    good) cat "$T/good.json" ;;
    notfound) echo 'gh: Not Found (HTTP 404)' >&2; exit 1 ;;
    forbidden) echo 'gh: Resource not accessible by integration (HTTP 403)' >&2; exit 1 ;;
esac
STUB
chmod +x "$T/bin/gh"
# via_gh MODE CODE NAME [REPO]: runs the check through the stub, in a subshell so the environment stays as it was
via_gh() {
    (
        GH_STUB=$1
        GH_REPO=${4-owner/repo}
        GITHUB_REPOSITORY=
        PATH=$T/bin:$PATH
        export GH_STUB GH_REPO GITHUB_REPOSITORY PATH
        expect "$2" "$3"
    ) || fail=1
}
via_gh good 0 "through gh"
if [ "$(cat "$T/asked")" = repos/owner/repo/environments/flatpak-signing ]; then echo "ok   asks the environments endpoint of GH_REPO"; else echo "FAIL asked $(cat "$T/asked")"; fail=1; fi
via_gh notfound 3 "environment missing (404)"
via_gh forbidden 5 "GitHub refuses the read (403)"
via_gh good 1 "no repository given" ''

exit "$fail"
