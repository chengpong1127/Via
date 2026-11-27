#!/usr/bin/env bash
# Deploys one CI-green commit of main to the local OrbStack stack. See docs/local-orbstack.md "Auto-deploy".
#   deploy/orbstack-deploy.sh <commit-sha> [--dry-run]
# Run from a checkout of exactly <commit-sha> (the Actions runner workspace). Needs VIA_DATA_DIR and
# VIA_TILES_DIR outside the checkout. Never writes to them. A pause file skips the deploy.
set -euo pipefail

CTX=orbstack
NS=via
SERVER_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_DIR="$(git -C "$SERVER_DIR" rev-parse --show-toplevel)"
PAUSE_FILE="${VIA_NO_DEPLOY_FILE:-$HOME/.via-no-deploy}"
READY_URL="${VIA_READY_URL:-http://127.0.0.1:3000/health/ready}"
READY_TIMEOUT="${VIA_READY_TIMEOUT:-120}"
ANNOTATION=via.local/deployed-sha
DEPLOYMENTS="server tdx-gateway valhalla"

sha="${1:?usage: orbstack-deploy.sh <commit-sha> [--dry-run]}"
dry_run=0
[ "${2:-}" != "--dry-run" ] || dry_run=1

kc() { kubectl --context "$CTX" -n "$NS" "$@"; }
log() { echo "[orbstack-deploy] $*"; }
die() { log "ERROR: $*"; summary "FAILED before deploying $sha: $*"; exit 1; }
summary() {
  [ -z "${GITHUB_STEP_SUMMARY:-}" ] || echo "$*" >>"$GITHUB_STEP_SUMMARY"
}

if [ -e "$PAUSE_FILE" ]; then
  log "paused: $PAUSE_FILE exists; not deploying $sha (remove the file to resume)"
  summary "Deploy of $sha skipped: paused by $PAUSE_FILE"
  exit 0
fi

[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || die "not a full commit sha: $sha"
[ "$(git -C "$REPO_DIR" rev-parse HEAD)" = "$sha" ] || die "checkout is at $(git -C "$REPO_DIR" rev-parse HEAD), not $sha"
git -C "$REPO_DIR" diff --quiet HEAD || die "checkout has local changes"
git -C "$REPO_DIR" merge-base --is-ancestor "$sha" origin/main || die "$sha is not on origin/main"

repo="${GITHUB_REPOSITORY:-$(git -C "$REPO_DIR" remote get-url origin | sed -E 's#^.*[:/]([^/]+/[^/]+)$#\1#; s#\.git$##')}"
ci_sha="$(git -C "$REPO_DIR" rev-list -1 "$sha" -- server .github/workflows/server.yml)"
[ -n "$ci_sha" ] || die "no commit touching server/ at or before $sha"
ci_url="${VIA_CI_API:-https://api.github.com/repos/$repo/actions/workflows/server.yml/runs}?head_sha=$ci_sha&event=push&status=completed"
green="$(curl -fsS -m 30 "$ci_url" | jq --arg sha "$ci_sha" '[.workflow_runs[] | select(.head_sha == $sha and .head_branch == "main" and .event == "push" and .conclusion == "success")] | length')" \
  || die "could not read the Server CI result for $ci_sha"
[ "$green" -ge 1 ] || die "no green Server CI push run on main for $ci_sha (the last commit at or before $sha that touches server/)"

[ -n "${VIA_DATA_DIR:-}" ] && [ -n "${VIA_TILES_DIR:-}" ] || die "set VIA_DATA_DIR and VIA_TILES_DIR (runner .env file)"
for dir in "$VIA_DATA_DIR" "$VIA_TILES_DIR"; do
  [ -d "$dir" ] || die "no such directory: $dir"
  case "$(cd "$dir" && pwd -P)/" in
    "$(cd "$REPO_DIR" && pwd -P)"/*) die "$dir is inside the checkout; a checkout clean could delete it" ;;
  esac
done
export VIA_DATA_DIR VIA_TILES_DIR

deployed="$(kc get deploy server -o "jsonpath={.metadata.annotations.via\.local/deployed-sha}" 2>/dev/null || true)"
if [ -n "$deployed" ] && [ "$deployed" != "$sha" ] && git -C "$REPO_DIR" merge-base --is-ancestor "$sha" "$deployed" 2>/dev/null; then
  log "cluster is already at or beyond $sha (deployed $deployed); skipping"
  summary "Deploy of $sha skipped: cluster already at or beyond it ($deployed)"
  exit 0
fi

if [ -n "$(kc get jobs -o 'jsonpath={.items[*].status.active}' 2>/dev/null || true)" ]; then
  log "a Job is active in namespace $NS (data refresh?); not deploying $sha"
  summary "Deploy of $sha skipped: a Job is active in namespace $NS"
  exit 0
fi

kc get secret tdx-credentials >/dev/null || die "secret tdx-credentials missing in namespace $NS"
revisions=""
for d in $DEPLOYMENTS; do
  rev="$(kc get deploy "$d" -o "jsonpath={.metadata.annotations.deployment\.kubernetes\.io/revision}" 2>/dev/null || true)"
  [ -z "$rev" ] || revisions="$revisions $d=$rev"
done
log "previous deployment: ${deployed:-unknown commit}, revisions:${revisions:- none}"

if [ "$dry_run" = 1 ]; then
  log "dry run: would build via-server:dev from $sha, run orbstack.sh up, wait for $READY_URL, roll back on failure"
  exit 0
fi

had_previous_image=0
if docker image inspect via-server:dev >/dev/null 2>&1; then
  docker tag via-server:dev via-server:previous
  had_previous_image=1
fi

ready() {
  local deadline=$((SECONDS + READY_TIMEOUT))
  until curl -fsS -m 5 -o /dev/null "$READY_URL"; do
    [ "$SECONDS" -lt "$deadline" ] || return 1
    sleep 3
  done
}

rollback() {
  if [ -z "$revisions" ] || [ "$had_previous_image" = 0 ]; then
    log "no previous deployment to roll back to"
    summary "Deploy of $sha FAILED; nothing to roll back to"
    return 1
  fi
  docker tag via-server:previous via-server:dev
  for entry in $revisions; do
    kc rollout undo "deploy/${entry%%=*}" "--to-revision=${entry##*=}" || true
  done
  kc rollout status deploy/server deploy/tdx-gateway deploy/valhalla --timeout=300s || true
  if ready; then
    log "rolled back to ${deployed:-the previous deployment}; it is serving again"
    summary "Deploy of $sha FAILED and was rolled back to ${deployed:-the previous deployment}"
  else
    log "rolled back but $READY_URL is still not ready"
    summary "Deploy of $sha FAILED; rollback done but the server is NOT ready"
  fi
}

fail() {
  log "ERROR: $1"
  kc get pods || true
  kc logs deploy/server --tail=30 || true
  rollback || true
  exit 1
}

log "deploying $sha"
"$SERVER_DIR/deploy/orbstack.sh" up || fail "orbstack.sh up failed"
ready || fail "$READY_URL not ready after ${READY_TIMEOUT}s"
kc annotate deploy/server "$ANNOTATION=$sha" --overwrite
log "deployed $sha"
summary "Deployed $sha (previous: ${deployed:-unknown})"
