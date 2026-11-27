#!/usr/bin/env bash
# Behaviour tests for refresh-static.sh with fake via-data and kubectl: nothing touches TDX or a cluster.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
overlay="$(dirname "$here")"
server_dir="$(cd "$overlay/../../../.." && pwd)"
script="$overlay/refresh-static.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

export VIA_DATA="$here/fake-via-data"
export KUBECTL="$here/fake-kubectl"
export FAKE_LOG="$tmp/calls.log"
export FAKE_STATE="$tmp/state"
export DATA_DIR="$tmp/data"
export HOST_DATA_DIR="/host/via/server/data"
export VIA_CONFIG="$server_dir/config/via.toml"
export BUILD_TIMEOUT_SECONDS=3
export BUILD_EXIT_GRACE_SECONDS=1
export POLL_SECONDS=0.2
export VALHALLA_CHECK="$here/fake-valhalla-check"
export TDX_CLIENT_ID=test-client-id-0000
export TDX_CLIENT_SECRET=test-client-secret-9999
export TMPDIR="$tmp"

fails=0
check() {
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1 (expected '$3', got '$2')"; fails=$((fails + 1)); fi
}
calls() { grep -c "$1" "$FAKE_LOG" || true; }
server_path() { cat "$FAKE_STATE/server-path"; }
latest_target() { readlink "$DATA_DIR/build/.via-static-latest.json"; }
run() { "$script" >"$tmp/out.log" 2>&1; }

reset() {
  rm -rf "$DATA_DIR" "$FAKE_STATE"
  mkdir -p "$DATA_DIR/build/20261126T132852Z" "$DATA_DIR/fetch/20261125T003000Z" "$FAKE_STATE"
  echo '{"ok":"old"}' >"$DATA_DIR/build/20261126T132852Z/.via-static.json"
  ln -sfn 20261126T132852Z/.via-static.json "$DATA_DIR/build/.via-static-latest.json"
  ln -sfn 20261125T003000Z "$DATA_DIR/fetch/latest"
  printf '%s' "$HOST_DATA_DIR/build/20261126T132852Z" >"$FAKE_STATE/server-path"
  : >"$FAKE_LOG"
  unset FAKE_FETCH_FAIL FAKE_BUILD_MODE FAKE_VALIDATE_FAIL FAKE_REUSED || true
}
untouched() { # the old build and the old snapshot are still there
  [ -f "$DATA_DIR/build/20261126T132852Z/.via-static.json" ] && [ -d "$DATA_DIR/fetch/20261125T003000Z" ] && echo yes || echo no
}

reset
if run; then check "happy path exits 0" ok ok; else check "happy path exits 0" failed ok; cat "$tmp/out.log"; fi
new_id="$(basename "$(dirname "$(readlink -f "$DATA_DIR/build/.via-static-latest.json" 2>/dev/null || latest_target)")")"
check "server points at the new build under the host data dir" "$(server_path)" "$HOST_DATA_DIR/build/$new_id"
check "new build differs from the old one" "$([ "$new_id" != 20261126T132852Z ] && echo yes || echo no)" "yes"
check "one full fetch, no --gtfs-only" "$(calls '^fetch$')/$(calls 'gtfs-only')" "1/0"
check "one build" "$(calls '^build --input ')" "1"
check "build reads the snapshot fetch just wrote" "$(grep -c "^build --input $DATA_DIR/fetch/20" "$FAKE_LOG")" "1"
check "artifact validated before activation" "$(calls '^validate --static ')" "1"
check "rollout waited for" "$(calls 'rollout status deploy/server')" "1"
check "no undo on success" "$(calls 'rollout undo')" "0"
check "patch keeps the volume name and Directory type" "$(jq -r '.spec.template.spec.volumes[0] | "\(.name) \(.hostPath.type)"' "$FAKE_STATE/patches.json")" "static Directory"
check "old build and snapshot are kept" "$(untouched)" "yes"
check "tdx usage block reaches the job log" "$(grep -c '^tdx usage:' "$tmp/out.log")" "1"
check "secrets are never printed" "$(grep -c -e test-client-secret-9999 -e test-client-id-0000 "$tmp/out.log" || true)" "0"
check "the shipped config is used as it is (pacing lives in config/via.toml only)" "$(calls -- '--config')" "0"

reset
export VALHALLA_CHECK=false
if run; then check "unready valhalla fails" ran failed; else check "unready valhalla fails" failed failed; fi
check "unready valhalla fails before any fetch (no TDX spend)" "$(calls '^fetch')" "0"
check "unready valhalla is named in the log" "$(grep -c -i valhalla "$tmp/out.log")" "1"
export VALHALLA_CHECK="$here/fake-valhalla-check"

reset
touch "$FAKE_STATE/no-server"
if run; then check "missing server deployment fails" ran failed; else check "missing server deployment fails" failed failed; fi
check "missing server deployment fails before any fetch (no TDX spend)" "$(calls '^fetch')" "0"

reset
export FAKE_FETCH_FAIL=1
if run; then check "failed fetch exits non-zero" ran failed; else check "failed fetch exits non-zero" failed failed; fi
check "failed fetch builds nothing" "$(calls '^build')" "0"
check "failed fetch leaves the server alone" "$(calls 'patch deploy/server')" "0"
check "failed fetch keeps the previous build active" "$(server_path)" "$HOST_DATA_DIR/build/20261126T132852Z"

reset
export FAKE_REUSED=bike-stations-taipei
if run; then check "partial fetch (reused file) exits non-zero" ran failed; else check "partial fetch (reused file) exits non-zero" failed failed; fi
check "partial fetch names the reused source" "$(grep -c bike-stations-taipei "$tmp/out.log")" "1"
check "partial fetch builds nothing" "$(calls '^build')" "0"
check "partial fetch leaves the server alone" "$(calls 'patch deploy/server')" "0"

reset
export FAKE_BUILD_MODE=fail
if run; then check "failed build exits non-zero" ran failed; else check "failed build exits non-zero" failed failed; fi
check "failed build leaves the server alone" "$(calls 'patch deploy/server')" "0"
check "failed build keeps the shared latest link" "$(latest_target)" "20261126T132852Z/.via-static.json"
check "failed build keeps old data" "$(untouched)" "yes"

reset
export FAKE_VALIDATE_FAIL=1
if run; then check "invalid artifact exits non-zero" ran failed; else check "invalid artifact exits non-zero" failed failed; fi
check "invalid artifact leaves the server alone" "$(calls 'patch deploy/server')" "0"
check "invalid artifact restores the shared latest link" "$(latest_target)" "20261126T132852Z/.via-static.json"
check "invalid artifact keeps old data" "$(untouched)" "yes"

reset
touch "$FAKE_STATE/rollout-fails"
if run; then check "rollout failure exits non-zero" ran failed; else check "rollout failure exits non-zero" failed failed; fi
check "rollout failure undoes the patch" "$(calls 'rollout undo deploy/server')" "1"
check "rollout failure restores the previous build" "$(server_path)" "$HOST_DATA_DIR/build/20261126T132852Z"
check "rollout failure restores the shared latest link" "$(latest_target)" "20261126T132852Z/.via-static.json"
new_builds=$(find "$DATA_DIR/build" -mindepth 1 -maxdepth 1 -type d -name '20[2-9]*' ! -name 20261126T132852Z | wc -l | tr -d ' ')
check "rollout failure keeps the new build on disk" "$new_builds" "1"

reset
export FAKE_BUILD_MODE=link-then-fail
if run; then check "build that moves the link then fails exits non-zero" ran failed; else check "build that moves the link then fails exits non-zero" failed failed; fi
check "link moved then build failed: latest link restored" "$(latest_target)" "20261126T132852Z/.via-static.json"
check "link moved then build failed: server untouched" "$(calls 'patch deploy/server')" "0"

reset
export FAKE_BUILD_MODE=link-then-hang
start=$(date +%s)
if run; then check "build that moves the link then hangs exits non-zero" ran failed; else check "build that moves the link then hangs exits non-zero" failed failed; fi
check "hung build is stopped by the timeout, not left to the job deadline" "$(( $(date +%s) - start < 20 ? 1 : 0 ))" "1"
check "link moved then hung: latest link restored" "$(latest_target)" "20261126T132852Z/.via-static.json"
check "link moved then hung: server untouched" "$(calls 'patch deploy/server')" "0"
check "link moved then hung: build process is gone" "$(kill -0 "$(cat "$FAKE_STATE/build.pid")" 2>/dev/null && echo alive || echo gone)" "gone"

reset
export FAKE_BUILD_MODE=artifact-then-hang
if run; then check "build that finishes but never exits is validated and activated" ok ok; else check "build that finishes but never exits is validated and activated" failed ok; cat "$tmp/out.log"; fi
check "lingering build: artifact validated" "$(calls '^validate --static ')" "1"
check "lingering build: server patched to the new build" "$(server_path | grep -c "^$HOST_DATA_DIR/build/20[2-9]")" "1"
check "lingering build: process ended" "$(kill -0 "$(cat "$FAKE_STATE/build.pid")" 2>/dev/null && echo alive || echo gone)" "gone"

reset
export FAKE_BUILD_MODE=link-then-hang BUILD_TIMEOUT_SECONDS=60
"$script" >"$tmp/out.log" 2>&1 &
job=$!
for _ in $(seq 1 100); do [ -f "$FAKE_STATE/build.pid" ] && break; sleep 0.1; done
kill -TERM "$job"
wait "$job" || true
check "SIGTERM mid-build (pod deleted or deadline) restores the latest link" "$(latest_target)" "20261126T132852Z/.via-static.json"
check "SIGTERM mid-build stops the build process" "$(kill -0 "$(cat "$FAKE_STATE/build.pid")" 2>/dev/null && echo alive || echo gone)" "gone"
export BUILD_TIMEOUT_SECONDS=3

reset
if command -v flock >/dev/null 2>&1; then
  exec 8>"$DATA_DIR/.refresh.lock"
  flock -n 8
  if run; then check "second refresh is refused while one runs" ran refused; else check "second refresh is refused while one runs" refused refused; fi
  check "refused refresh spends no TDX call" "$(calls '^fetch')" "0"
  exec 8>&-
else
  echo "skip lock test (no flock on this host; the pod image has it)"
fi

echo "failures: $fails"
exit "$fails"
