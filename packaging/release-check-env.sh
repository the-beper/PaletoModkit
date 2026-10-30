#!/bin/sh
# Fails unless the GitHub environment that holds the signing key is protected: it exists, has at least one required
# reviewer and limits the refs that may deploy to it. A job that names an environment which does not exist makes GitHub
# create it with no protection, so the release workflow runs this in a job WITHOUT `environment:`, before the signing
# job. The data is public (anyone with read access to the repository can read it), so a read-only token is enough.
#   release-check-env.sh [--env NAME] [--json FILE]
#   GH_REPO (or GITHUB_REPOSITORY)  owner/name; GH_TOKEN  for gh; the default NAME is flatpak-signing
#   --json FILE  check this saved response of GET /repos/OWNER/REPO/environments/NAME instead of asking GitHub (tests)
# Exit codes: see release-lib.sh (3: the environment is missing or not protected; 5: GitHub could not be read).
set -eu

. "$(dirname "$0")/release-lib.sh"

NAME=flatpak-signing
JSON=
while [ $# -gt 0 ]; do
    case $1 in
        --env) NAME=${2:?--env needs a name}; shift 2 ;;
        --json) JSON=${2:?--json needs a file}; shift 2 ;;
        *) die 1 "usage: $PROG [--env NAME] [--json FILE]" ;;
    esac
done
command -v jq > /dev/null || die 1 "jq is needed"

TMP=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/check-env.XXXXXX")
trap 'rm -rf "$TMP"' EXIT
if [ -n "$JSON" ]; then
    cp "$JSON" "$TMP/env.json" || die 1 "cannot read $JSON"
else
    REPO=${GH_REPO:-${GITHUB_REPOSITORY:-}}
    [ -n "$REPO" ] || die 1 "set GH_REPO or GITHUB_REPOSITORY"
    if ! gh api "repos/$REPO/environments/$NAME" > "$TMP/env.json" 2> "$TMP/err"; then
        if grep -q 'HTTP 404' "$TMP/err"; then
            die 3 "environment $NAME does not exist in $REPO: a job that names it would create it without protection"
        fi
        die 5 "cannot read the environment $NAME of $REPO: $(head -n1 "$TMP/err")"
    fi
fi

jq -e 'type == "object"' "$TMP/env.json" > /dev/null 2>&1 || die 5 "the answer for $NAME is not a JSON object"
[ "$(jq -r '.name // empty' "$TMP/env.json")" = "$NAME" ] || die 3 "the answer is not for the environment $NAME"

reviewers=$(jq -r '[.protection_rules[]? | select(.type == "required_reviewers") | (.reviewers // []) | length] | add // 0' "$TMP/env.json")
[ "$reviewers" -ge 1 ] 2> /dev/null || die 3 "environment $NAME has no required reviewer"

# Deployment branches and tags must be "selected" (custom policies): "protected branches" admits no tag, so a release
# would pass here and stop at sign-flatpak, and no policy at all lets any ref deploy. The patterns are not read.
jq -e '.deployment_branch_policy | type == "object" and .custom_branch_policies == true' "$TMP/env.json" > /dev/null 2>&1 \
    || die 3 "environment $NAME does not limit deployments to selected branches and tags"

echo "environment $NAME: $reviewers required reviewer(s), deployment refs restricted"
