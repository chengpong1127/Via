#!/usr/bin/env bash
# Tests deploy/orbstack-deploy.sh against a throwaway git repo with fake kubectl, docker and curl.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
deploy="$(dirname "$here")"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

fails=0
ok() { echo "ok   $1"; }
bad() { echo "FAIL $1"; fails=$((fails + 1)); }
check() { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (expected '$3', got '$2')"; fi; }
has() { if grep -q -- "$3" "$2"; then ok "$1"; else bad "$1 (no '$3' in $2)"; fi; }
hasnt() { if grep -q -- "$3" "$2"; then bad "$1 ('$3' found in $2)"; else ok "$1"; fi; }

fakebin="$tmp/bin"
mkdir -p "$fakebin"
cat >"$fakebin/kubectl" <<'F'
#!/usr/bin/env bash
echo "kubectl $*" >>"$CALLS"
case "$*" in
  *"rollout undo"*) touch "$STATE/undone" ;;
  *"rollout status"*) [ ! -f "$STATE/rollout-fails" ] || [ -f "$STATE/undone" ] || exit 1 ;;
  *"get jobs"*) [ ! -f "$STATE/job-active" ] || echo 1 ;;
  *"get deploy"*deployed-sha*) cat "$STATE/deployed-sha" 2>/dev/null || true ;;
  *"get deploy"*revision*) echo 7 ;;
esac
exit 0
F
cat >"$fakebin/docker" <<'F'
#!/usr/bin/env bash
echo "docker $*" >>"$CALLS"
[ "$1" != build ] || [ ! -f "$STATE/build-fails" ] || exit 1
exit 0
F
cat >"$fakebin/curl" <<'F'
#!/usr/bin/env bash
echo "curl $*" >>"$CALLS"
case "$*" in
  *actions/workflows*) cat "$STATE/ci.json" ;;
  *) [ ! -f "$STATE/ready-fails" ] || [ -f "$STATE/undone" ] || exit 22 ;;
esac
F
chmod +x "$fakebin"/*

make_world() {
  world="$tmp/world-$1"
  mkdir -p "$world/origin.git" "$world/checkout/server" "$world/state" "$world/data/build/b1" "$world/tiles"
  git init -q --bare -b main "$world/origin.git"
  cp -R "$deploy" "$world/checkout/server/deploy"
  rm -rf "$world/checkout/server/deploy/tests"
  echo '{}' >"$world/data/build/b1/.via-static.json"
  ln -s b1/.via-static.json "$world/data/build/.via-static-latest.json"
  (
    cd "$world/checkout"
    git init -q -b main
    git config user.email t@example.com
    git config user.name t
    git remote add origin "$world/origin.git"
    git add -A
    git commit -q -m one
    echo two >server/two
    git add -A
    git commit -q -m two
    git push -q origin main
  )
  sha="$(git -C "$world/checkout" rev-parse HEAD)"
  parent="$(git -C "$world/checkout" rev-parse HEAD~1)"
  echo "{\"workflow_runs\":[{\"head_sha\":\"$sha\",\"head_branch\":\"main\",\"event\":\"push\",\"conclusion\":\"success\"}]}" >"$world/state/ci.json"
  : >"$world/calls"
}

run_deploy() {
  local sha_arg="$1"
  shift
  (
    cd "$world/checkout"
    PATH="$fakebin:$PATH" CALLS="$world/calls" STATE="$world/state" \
      VIA_DATA_DIR="${DATA_DIR-$world/data}" VIA_TILES_DIR="${TILES_DIR:-$world/tiles}" \
      VIA_NO_DEPLOY_FILE="$world/no-deploy" VIA_READY_TIMEOUT=2 GITHUB_REPOSITORY=o/r \
      GITHUB_STEP_SUMMARY="$world/summary" HOME="$world" \
      "$world/checkout/server/deploy/orbstack-deploy.sh" "$sha_arg" "$@"
  ) >"$world/out" 2>&1
}
status() { if run_deploy "$@"; then echo 0; else echo $?; fi; }

make_world pause
touch "$world/no-deploy"
check "pause file: exit 0" "$(status "$sha")" 0
has "pause file: clear log line" "$world/out" "paused"
check "pause file: no cluster or docker calls" "$(grep -c '^\(kubectl\|docker\)' "$world/calls" || true)" 0

make_world mismatch
check "HEAD is not the requested commit: fails" "$(status "$parent")" 1
check "HEAD is not the requested commit: nothing run" "$(wc -c <"$world/calls" | tr -d ' ')" 0

make_world offmain
git -C "$world/checkout" checkout -q -b side
echo x >"$world/checkout/server/x"
git -C "$world/checkout" add -A
git -C "$world/checkout" commit -q -m side
sha="$(git -C "$world/checkout" rev-parse HEAD)"
check "commit not on origin/main: fails" "$(status "$sha")" 1
has "commit not on origin/main: says so" "$world/out" "not on origin/main"
hasnt "commit not on origin/main: no build" "$world/calls" "docker build"

make_world red
echo '{"workflow_runs":[]}' >"$world/state/ci.json"
check "no green CI run: fails" "$(status "$sha")" 1
hasnt "no green CI run: no build" "$world/calls" "docker build"

make_world forkpr
echo "{\"workflow_runs\":[{\"head_sha\":\"$sha\",\"head_branch\":\"main\",\"event\":\"pull_request\",\"conclusion\":\"success\"}]}" >"$world/state/ci.json"
check "green run from a pull_request is not enough: fails" "$(status "$sha")" 1

make_world nodata
check "VIA_DATA_DIR unset: fails" "$(DATA_DIR='' status "$sha")" 1

make_world datainside
mkdir -p "$world/checkout/server/data/build/b1"
echo '{}' >"$world/checkout/server/data/build/b1/.via-static.json"
check "data dir inside the checkout: fails" "$(DATA_DIR="$world/checkout/server/data" status "$sha")" 1
has "data dir inside the checkout: says so" "$world/out" "inside the checkout"

make_world dry
check "dry run: exit 0" "$(status "$sha" --dry-run)" 0
hasnt "dry run: no build" "$world/calls" "docker build"
hasnt "dry run: no apply" "$world/calls" "apply"
hasnt "dry run: no undo or restart" "$world/calls" "rollout \(undo\|restart\)"
has "dry run: reads the cluster" "$world/calls" "get secret tdx-credentials"

make_world ok
before="$(cd "$world/data" && find . | LC_ALL=C sort | cksum)"
check "deploy: exit 0" "$(status "$sha")" 0
has "deploy: builds the image" "$world/calls" "docker build -t via-server:dev"
has "deploy: applies the overlay" "$world/calls" "apply -k"
has "deploy: checks readiness" "$world/calls" "health/ready"
has "deploy: records the deployed commit" "$world/calls" "annotate deploy/server via.local/deployed-sha=$sha"
if [ -f "$world/checkout/server/deploy/k8s/overlays/orbstack/volumes.yaml" ]; then ok "deploy: volumes.yaml written in the checkout"; else bad "deploy: volumes.yaml written in the checkout"; fi
check "deploy: shared data dir unchanged" "$(cd "$world/data" && find . | LC_ALL=C sort | cksum)" "$before"
has "deploy: step summary" "$world/summary" "$sha"

make_world rollout
touch "$world/state/rollout-fails"
check "rollout failure: exit 1" "$(status "$sha")" 1
tag_line="$(grep -n 'docker tag via-server:previous via-server:dev' "$world/calls" | head -1 | cut -d: -f1)"
undo_line="$(grep -n 'rollout undo' "$world/calls" | head -1 | cut -d: -f1)"
if [ -n "$tag_line" ] && [ -n "$undo_line" ] && [ "$tag_line" -lt "$undo_line" ]; then ok "rollout failure: previous image restored before undo"; else bad "rollout failure: previous image restored before undo"; fi
has "rollout failure: server undone to its old revision" "$world/calls" "rollout undo deploy/server --to-revision=7"
has "rollout failure: gateway undone" "$world/calls" "rollout undo deploy/tdx-gateway --to-revision=7"
has "rollout failure: valhalla undone" "$world/calls" "rollout undo deploy/valhalla --to-revision=7"
hasnt "rollout failure: commit not recorded as deployed" "$world/calls" "annotate"
has "rollout failure: reported as rolled back" "$world/out" "rolled back"

make_world ready
touch "$world/state/ready-fails"
check "not ready after deploy: exit 1" "$(status "$sha")" 1
has "not ready after deploy: undone" "$world/calls" "rollout undo deploy/server"
hasnt "not ready after deploy: commit not recorded" "$world/calls" "annotate"

make_world build
touch "$world/state/build-fails"
check "build failure: exit 1" "$(status "$sha")" 1
hasnt "build failure: nothing applied" "$world/calls" "apply"

make_world newer
echo "$sha" >"$world/state/deployed-sha"
echo three >"$world/checkout/server/three"
git -C "$world/checkout" add -A
git -C "$world/checkout" commit -q -m three
git -C "$world/checkout" push -q origin main
newer="$(git -C "$world/checkout" rev-parse HEAD)"
echo "$newer" >"$world/state/deployed-sha"
git -C "$world/checkout" checkout -q "$sha"
check "older than the deployed commit: skipped with exit 0" "$(status "$sha")" 0
has "older than the deployed commit: says so" "$world/out" "already at or beyond"
hasnt "older than the deployed commit: no build" "$world/calls" "docker build"

make_world nonserver
echo docs >"$world/checkout/README.md"
git -C "$world/checkout" add -A
git -C "$world/checkout" commit -q -m "docs only"
git -C "$world/checkout" push -q origin main
docs_sha="$(git -C "$world/checkout" rev-parse HEAD)"
check "non-server commit on top of a green server commit: deploys" "$(status "$docs_sha")" 0
has "non-server commit: builds" "$world/calls" "docker build"

make_world nonservernoci
echo docs >"$world/checkout/README.md"
git -C "$world/checkout" add -A
git -C "$world/checkout" commit -q -m "docs only"
git -C "$world/checkout" push -q origin main
docs_sha="$(git -C "$world/checkout" rev-parse HEAD)"
echo '{"workflow_runs":[]}' >"$world/state/ci.json"
check "non-server commit whose server ancestor has no green run: fails" "$(status "$docs_sha")" 1

make_world job
touch "$world/state/job-active"
check "active Job in the namespace: skipped with exit 0" "$(status "$sha")" 0
has "active Job in the namespace: says so" "$world/out" "Job is active"
hasnt "active Job in the namespace: no build" "$world/calls" "docker build"

check "script uses no secrets and no destructive commands" "$(grep -cE 'secrets\.|rm -rf|rm -r |rmdir|mv ' "$deploy/orbstack-deploy.sh" || true)" 0

[ "$fails" -eq 0 ] || { echo "$fails failed"; exit 1; }
echo "all passed"
