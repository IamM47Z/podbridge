# Reviewing upstream LibrePods changes

podbridge started from the `linux-rust/` folder of `librepods-org/librepods`, branch `linux/rust`. The daily watcher lists upstream changes to that folder in the "Upstream to review" issue. A local session reviews them and proposes the worth-keeping ones as one pull request into `master`. The owner approves by merging it.

## State

- `.github/upstream-prs.tsv` holds one verdict per upstream PR: `merged`, `partial`, `skipped`, `waiting`, `upstream` or `closed`, with the head commit it was judged at and one reason. Rows that say "in linux/rust-maintained" were merged into the old fork and came into podbridge with the import. Read earlier verdicts before judging; do not reverse one without a new reason.
- `.github/upstream-base` holds the last upstream `linux/rust` commit reviewed. The watcher lists commits after it.

## Rules

- Never post on the upstream repository. Draft comments for the owner, who posts them. Reach the maintainer through upstream PR #241.
- Keep changes generic, never specific to one machine or AirPods model.
- When the issue lists no items above "Waiting", stop: change nothing and open no pull request.

## For each item

1. Read the PR description and the full diff of `linux-rust/`. Never judge from the title. A large PR gets its commits read one by one, to find self-contained fixes.
2. Judge: what it changes, whether it fixes something podbridge hits, risk (size, quality, unrelated files, new dependencies, CI or workflow changes, network calls, binaries, author behaviour), and overlap with what podbridge already has. Skip CI and dependency changes and anything trust-relevant.
3. Verdict: `merge`, `partial` (name the commits), `waiting` (with what it waits for) or `skipped`, each with one reason.

## Applying

Upstream paths start with `linux-rust/`; podbridge has them at the root, so every patch is applied with `-p2`.

- Work on a branch `upstream-review/<date>` from `master`.
- One squash commit per upstream PR:
  `git fetch https://github.com/librepods-org/librepods.git pull/<n>/head`, then
  `git diff $(git merge-base FETCH_HEAD <upstream linux/rust>) FETCH_HEAD -- linux-rust/ | git apply -p2 --3way`.
  - Commit it as `<PR title> (#<n>)`, with `--author` set to the PR's opener and `--date` to its head commit's date.
  - Its body is `Squash of upstream PR <url> at <head sha>.`, plus `Conflicts resolved by <you>.` when you resolved any, then a `Co-authored-by:` line for every other commit author in the PR.
  - A partial verdict is one commit too, by the author of the picked commits, titled `<summary> (#<n>, partial)`, made with `git format-patch --stdout <commits> -- linux-rust/ | git am -p2 --3way` and squashed, its body listing the upstream commits it takes.
  - For a PR with new commits that was already merged, squash only its new commits, as a second commit for that PR.
- New commits on `linux/rust` itself: review each, then apply the kept ones the same way, one commit per upstream commit with its author and date, and write the newest reviewed commit to `.github/upstream-base`.
- Resolve small conflicts yourself, keeping both sides' intent. Explain each resolution in the pull request body.
- When an upstream change has a real bug, fix it in its own commit right after its squash, with a `Follow-up to upstream PR #<n>.` line in the body, and draft a comment for that upstream PR in the pull request body.
- Update `.github/upstream-prs.tsv` for every item judged, skips included: the PR's current head commit, the verdict and the reason.
- Run `cargo build --release --locked` and `cargo test --release --locked`. Both must pass. When they do not and you cannot fix it, drop that item, set it to `waiting` with the error, and say so.

## Reporting

- Open one pull request into `master`. Its body is a table of every item with verdict and reason, then conflict resolutions, then drafted upstream comments, then the build and test results.
- When nothing is worth merging, still open the pull request with only the state file changes, so the issue clears once it is merged.
- End the pull request body with `Closes #<issue>`.
