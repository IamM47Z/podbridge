#!/usr/bin/env bash
# Lists upstream LibrePods changes to review for podbridge in one tracking issue.
# Verdicts live in .github/upstream-prs.tsv and the last reviewed upstream commit in
# .github/upstream-base; this script never merges anything.
set -euo pipefail

UPSTREAM=librepods-org/librepods
BASE=linux/rust
PATH_FILTER=linux-rust/
STATE=${UPSTREAM_STATE:-.github/upstream-prs.tsv}
BASE_FILE=${UPSTREAM_BASE_FILE:-.github/upstream-base}
LABEL=upstream-watch
REPO=${GITHUB_REPOSITORY:?}

# shortcut: gh lists at most 100 files per PR, so a PR whose linux-rust/ files all come
# later is missed. Upgrade: page through the PR files API when a PR shows 100 files.
prs=$(gh pr list -R "$UPSTREAM" --base "$BASE" --state all --limit 500 \
    --json number,title,url,state,headRefOid,files |
    jq -c --arg p "$PATH_FILTER" '[.[] | select(any(.files[]; .path | startswith($p))) | del(.files)]')

# One JSON object per PR, joined with its verdict row when there is one.
report=$(jq -r --rawfile state "$STATE" '
    ($state | split("\n") | .[1:] | map(select(length > 0) | split("\t")
        | {key: .[0], value: {head: .[1], verdict: .[2], reason: .[3]}}) | from_entries) as $v
    | map(. + {seen: $v[(.number | tostring)]})
    | {
        new:     map(select(.state == "OPEN" and .seen == null)),
        updated: map(select(.state == "OPEN" and .seen != null and .seen.head != .headRefOid
                            and .seen.verdict != "skipped")),
        landed:  map(select(.state != "OPEN" and .seen != null
                            and (.seen.verdict == "waiting" or .seen.verdict == "review"))),
        waiting: map(select(.state == "OPEN" and .seen != null
                            and (.seen.verdict == "waiting" or .seen.verdict == "review")))
      }
    # Titles are upstream input: no HTML that could hide the body, no @mentions that ping.
    | def row: "- [#\(.number)](\(.url)) \(.title | gsub("<"; "&lt;") | gsub("@"; "@\u200b"))";
      def section(t; xs): if (xs | length) > 0 then "### \(t)\n" + (xs | map(row) | join("\n")) + "\n" else "" end;
      section("New PRs"; .new)
      + section("PRs with new commits"; .updated)
      + section("Merged or closed upstream while waiting"; .landed)
      + "\n<!-- actionable: \([.new, .updated, .landed] | add | map("\(.number)@\(.headRefOid)") | join(",")) -->\n"
      + section("Waiting (no action needed)"; .waiting)
' <<<"$prs")

# podbridge has its own history, so new upstream commits count from the recorded base.
git fetch -q "https://github.com/$UPSTREAM.git" "$BASE"
commits=$(git log --no-merges --format='- %h %s' "$(cat "$BASE_FILE")..FETCH_HEAD" -- "$PATH_FILTER")
base_key=$(git rev-parse FETCH_HEAD)
if [[ -n $commits ]]; then
    report="### New commits on \`$BASE\`"$'\n'"$commits"$'\n\n'"$report"
    report=${report/<!-- actionable: /<!-- actionable: base@$base_key,}
fi

actionable=$(grep -o '<!-- actionable: [^>]*-->' <<<"$report" | sed 's/<!-- actionable: \(.*\) -->/\1/; s/,$//')
body="Upstream: \`$UPSTREAM\` \`$BASE\`, changes touching \`$PATH_FILTER\`. Verdicts: \`$STATE\`. Port them in a local session that follows \`.github/upstream-review.md\`."$'\n\n'"$report"
if [[ -n ${DRY_RUN:-} ]]; then printf '%s\n\nactionable: %s\n' "$body" "${actionable:-none}"; exit 0; fi

gh label create "$LABEL" -R "$REPO" --color c5def5 --description "Upstream changes to review" --force >/dev/null
# One issue, reused: open while there is something to act on, closed otherwise.
# A failed lookup must stop the run: going on would open a second issue.
found=$(gh issue list -R "$REPO" --label "$LABEL" --state all --limit 1 \
    --json number,state -q '.[0] // empty | "\(.number) \(.state)"')
read -r issue state <<<"$found" || true
if [[ -z ${issue:-} && -z $actionable ]]; then
    echo "no issue; actionable: none"
    exit 0
elif [[ -z ${issue:-} ]]; then
    url=$(gh issue create -R "$REPO" --title "Upstream to review" --label "$LABEL" --body "$body")
    issue=${url##*/} state=OPEN old=""
else
    old=$(gh issue view "$issue" -R "$REPO" --json body -q .body |
        grep -o '<!-- actionable: [^>]*-->' | sed 's/<!-- actionable: \(.*\) -->/\1/; s/,$//' || true)
    gh issue edit "$issue" -R "$REPO" --body "$body" >/dev/null
fi

# Comment only when the set of things to act on changes, so each change notifies once.
if [[ -n $actionable ]]; then
    [[ $state == CLOSED ]] && gh issue reopen "$issue" -R "$REPO" >/dev/null
    if [[ $actionable != "$old" ]]; then
        gh issue comment "$issue" -R "$REPO" --body "New upstream changes to review. See the issue body." >/dev/null
    fi
elif [[ $state == OPEN ]]; then
    gh issue close "$issue" -R "$REPO" --reason completed >/dev/null
fi
echo "issue #$issue; actionable: ${actionable:-none}"
