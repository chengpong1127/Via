#!/usr/bin/env bash
# Lints .github/workflows/deploy-orbstack.yml: it runs on the owner's Mac, so only safe triggers may start it.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
workflow="$repo/.github/workflows/deploy-orbstack.yml"
ci_workflow="$repo/.github/workflows/server.yml"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

lint() {
  local file="$1" events ci_name code
  code="$(grep -v '^[[:space:]]*#' "$file")"
  events="$(awk '/^on:/ {inon=1; next} /^[^ #]/ {inon=0} inon && /^  [a-z_]+:/ {sub(/:.*/, "", $1); print $1}' "$file" | sort | tr '\n' ' ')"
  [ "$events" = "workflow_dispatch workflow_run " ] || echo "triggers must be exactly workflow_run and workflow_dispatch, got: $events"
  grep -Eq '^    workflows: \["Rust server"\]$' "$file" || echo "workflow_run must watch the Rust server workflow"
  ci_name="$(sed -n 's/^name: //p' "$ci_workflow" | head -1)"
  [ "$ci_name" = "Rust server" ] || echo "server.yml is no longer named Rust server"
  grep -Eq '^    branches: \[main\]$' "$file" || echo "workflow_run must be limited to branch main"
  grep -q "workflow_run.conclusion == 'success'" "$file" || echo "missing conclusion == 'success' guard"
  grep -q "workflow_run.event == 'push'" "$file" || echo "missing event == 'push' guard (fork pull requests can have head branch main)"
  grep -q "workflow_run.head_repository.full_name == github.repository" "$file" || echo "missing same-repository guard"
  grep -Eq '^    runs-on: \[self-hosted, via-orbstack\]$' "$file" || echo "runs-on must be [self-hosted, via-orbstack]"
  awk '/^permissions:/ {p=1; next} /^[^ ]/ {p=0} p && NF' "$file" | tr -d ' ' | tr '\n' ' ' | grep -qx 'contents:read ' || echo "top-level permissions must be exactly contents: read"
  ! grep -Eq 'secrets\.|secrets:' <<<"$code" || echo "must not use secrets"
  grep -Eq '^  cancel-in-progress: false$' "$file" || echo "concurrency must not cancel a running deploy"
  grep -Eq '^    timeout-minutes: [0-9]+$' "$file" || echo "missing job timeout"
  ! grep -Eq 'pull_request|issue_comment' <<<"$code" || echo "mentions a fork-triggerable event"
  ! grep -Eq 'github\.event\.(workflow_run\.(head_branch|display_title|head_commit)|inputs)' "$file" || echo "interpolates attacker-controllable text"
}

fails=0
expect_clean() {
  local out
  out="$(lint "$1")"
  if [ -z "$out" ]; then echo "ok   $2"; else echo "FAIL $2: $out"; fails=$((fails + 1)); fi
}
expect_violation() {
  local out
  out="$(lint "$1")"
  if [ -n "$out" ]; then echo "ok   $2"; else echo "FAIL $2 (lint passed)"; fails=$((fails + 1)); fi
}
mutate() { sed "$1" "$workflow" >"$tmp/m.yml"; }

expect_clean "$workflow" "deploy workflow passes the lint"
mutate 's/^  workflow_dispatch:/  pull_request:/'
expect_violation "$tmp/m.yml" "pull_request trigger is rejected"
mutate 's/^  workflow_dispatch:/  pull_request_target:/'
expect_violation "$tmp/m.yml" "pull_request_target trigger is rejected"
mutate 's/^  workflow_dispatch:/  issue_comment:/'
expect_violation "$tmp/m.yml" "issue_comment trigger is rejected"
mutate 's/^  workflow_dispatch:/  push:/'
expect_violation "$tmp/m.yml" "push trigger (no CI gate) is rejected"
mutate "s/ && github.event.workflow_run.event == 'push'//"
expect_violation "$tmp/m.yml" "missing push-event guard is rejected"
mutate "s/ == 'success'/ == 'failure'/"
expect_violation "$tmp/m.yml" "missing success guard is rejected"
mutate 's/^    runs-on: .*/    runs-on: ubuntu-latest/'
expect_violation "$tmp/m.yml" "wrong runner is rejected"
mutate 's/^  contents: read/  contents: write/'
expect_violation "$tmp/m.yml" "write permission is rejected"
mutate 's/^  cancel-in-progress: false/  cancel-in-progress: true/'
expect_violation "$tmp/m.yml" "cancelling a running deploy is rejected"
mutate 's/^    timeout-minutes:.*//'
expect_violation "$tmp/m.yml" "missing timeout is rejected"
{ cat "$workflow"; echo '      - run: echo ${{ secrets.TDX_CLIENT_SECRET }}'; } >"$tmp/m.yml"
expect_violation "$tmp/m.yml" "secrets use is rejected"

[ "$fails" -eq 0 ] || { echo "$fails failed"; exit 1; }
echo "all passed"
