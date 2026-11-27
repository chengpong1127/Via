#!/usr/bin/env bash
# Weekly static-data refresh, run by the weekly-refresh CronJob (docs/local-orbstack.md).
#
# One staged job: fetch (GTFS and every TDX file, paced by VIA_FETCH_REQUESTS_PER_MINUTE),
# build, validate, activate. Activation points the server Deployment's static hostPath at
# the new build; the rolling update keeps the old pod serving until the new one is Ready,
# and a failed rollout is undone. Anything that fails before activation leaves the server
# on the build it already uses, and puts back the .via-static-latest.json link that
# `via-data build` moved. Nothing under $DATA_DIR is ever deleted.
set -euo pipefail

VIA_DATA="${VIA_DATA:-via-data}"
KUBECTL="${KUBECTL:-kubectl}"
DATA_DIR="${DATA_DIR:-/build/server/data}"
HOST_DATA_DIR="${HOST_DATA_DIR:?HOST_DATA_DIR (the node path of the data directory) is required}"
SERVER_DEPLOYMENT="${SERVER_DEPLOYMENT:-server}"
VALHALLA_URL="${VIA_VALHALLA_URL:-http://valhalla:8002/}"
ROLLOUT_TIMEOUT="${ROLLOUT_TIMEOUT:-600s}"
BUILD_TIMEOUT_SECONDS="${BUILD_TIMEOUT_SECONDS:-7200}"
BUILD_EXIT_GRACE_SECONDS="${BUILD_EXIT_GRACE_SECONDS:-30}"
POLL_SECONDS="${POLL_SECONDS:-5}"

log() { echo "$(date -u +%H:%M:%S) $*" >&2; }
die() { log "ERROR: $*"; exit 1; }

build_dir="$DATA_DIR/build"
latest_link="$build_dir/.via-static-latest.json"
work="$(mktemp -d)"
started_marker="$work/started"
previous_latest=""
build_pid=""
tail_pid=""
finished=0

stop_build() {
  [ -z "$build_pid" ] && return 0
  if kill -0 "$build_pid" 2>/dev/null; then
    kill "$build_pid" 2>/dev/null || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "$build_pid" 2>/dev/null || break; sleep 0.5; done
    kill -9 "$build_pid" 2>/dev/null || true
  fi
  wait "$build_pid" 2>/dev/null || true
  build_pid=""
}

restore_latest_link() {
  # `via-data build` moves the link as its last step, before it prints the artifact
  # line, so a failed, hung or killed build may already have moved it. Put it back if it
  # now points at a build directory this run created.
  local current target
  current="$(readlink "$latest_link" || true)"
  [ -n "$previous_latest" ] && [ -n "$current" ] && [ "$current" != "$previous_latest" ] || return 0
  target="$build_dir/$(dirname "$current")"
  if [ -d "$target" ] && ! [ "$started_marker" -nt "$target" ]; then
    ln -sfn "$previous_latest" "$latest_link"
    log "restored $latest_link -> $previous_latest"
  fi
}

on_exit() {
  status=$?
  [ -z "$tail_pid" ] || kill "$tail_pid" 2>/dev/null || true
  stop_build
  if [ "$finished" != 1 ]; then
    restore_latest_link
    log "refresh failed (exit $status); the server keeps the build it was using"
  fi
  rm -rf "$work"
}
trap on_exit EXIT
trap 'exit 143' TERM INT

if command -v flock >/dev/null 2>&1; then
  mkdir -p "$DATA_DIR"
  exec 9>"$DATA_DIR/.refresh.lock"
  flock -n 9 || die "another refresh is running"
fi

server_path() {
  "$KUBECTL" get "deploy/$SERVER_DEPLOYMENT" \
    -o 'jsonpath={.spec.template.spec.volumes[?(@.name=="static")].hostPath.path}'
}

valhalla_ready() {
  if [ -n "${VALHALLA_CHECK:-}" ]; then "$VALHALLA_CHECK"; return; fi
  local hostport host port
  hostport="${VALHALLA_URL#*://}"
  hostport="${hostport%%/*}"
  host="${hostport%%:*}"
  port="${hostport##*:}"
  [ "$port" != "$hostport" ] || port=80
  # shellcheck disable=SC2016
  timeout 10 bash -c '
    exec 3<>"/dev/tcp/$0/$1"
    printf "GET /status HTTP/1.0\r\n\r\n" >&3
    read -r line <&3
    case "$line" in *" 200 "*) exit 0 ;; *) exit 1 ;; esac' "$host" "$port" 2>/dev/null
}

# Fail before any TDX call if the server cannot be reached, has no static hostPath, or Valhalla is down.
previous_path="$(server_path)" || die "cannot read deployment $SERVER_DEPLOYMENT (RBAC or not deployed)"
[ -n "$previous_path" ] || die "deployment $SERVER_DEPLOYMENT has no static hostPath volume"
log "server currently serves $previous_path"
valhalla_ready || die "Valhalla at $VALHALLA_URL is not ready; not fetching"

previous_latest="$(readlink "$latest_link" || true)"

log "fetching GTFS and TDX files"
"$VIA_DATA" fetch | tee "$work/fetch.log" || die "fetch failed"
snapshot="$(sed -n 's/^fetch snapshot: //p' "$work/fetch.log" | tail -1)"
[ -n "$snapshot" ] && [ -f "$snapshot/manifest.json" ] || die "fetch did not report a snapshot"
reused="$(jq -r '(.reused_sources // {}) | keys | join(", ")' "$snapshot/manifest.json")"
[ -z "$reused" ] || die "partial fetch, TDX files reused from an earlier snapshot: $reused; not building"

log "building from $snapshot"
touch "$started_marker"
: >"$work/build.log"
tail -n +1 -f "$work/build.log" &
tail_pid=$!
"$VIA_DATA" build --input "$snapshot" >>"$work/build.log" 2>&1 &
build_pid=$!

# Wait for the artifact line. via-data prints it after moving the link and has been seen not to
# exit afterwards, so a build that has printed it only gets a short grace period to exit by
# itself; a build that never prints it is stopped at the timeout.
deadline=$((SECONDS + BUILD_TIMEOUT_SECONDS))
artifact_seen_at=""
while kill -0 "$build_pid" 2>/dev/null; do
  if [ -z "$artifact_seen_at" ] && grep -q '^static artifact: ' "$work/build.log"; then
    artifact_seen_at=$SECONDS
  fi
  if [ -n "$artifact_seen_at" ] && [ $((SECONDS - artifact_seen_at)) -ge "$BUILD_EXIT_GRACE_SECONDS" ]; then
    log "build printed its artifact but did not exit; validating, then stopping it"
    break
  fi
  [ "$SECONDS" -lt "$deadline" ] || die "build did not finish within ${BUILD_TIMEOUT_SECONDS}s"
  sleep "$POLL_SECONDS"
done
if kill -0 "$build_pid" 2>/dev/null; then
  build_status=0
else
  build_status=0
  wait "$build_pid" || build_status=$?
  build_pid=""
fi
sleep 1
[ "$build_status" = 0 ] || die "build failed (exit $build_status)"
artifact="$(sed -n 's/^static artifact: //p' "$work/build.log" | tail -1)"
[ -n "$artifact" ] && [ -s "$artifact" ] || die "build did not produce an artifact"
build_id="$(basename "$(dirname "$artifact")")"

log "validating $artifact"
"$VIA_DATA" validate --static "$artifact" >/dev/null || die "artifact $build_id is invalid"
stop_build

log "activating build $build_id"
patch="$(jq -cn --arg path "$HOST_DATA_DIR/build/$build_id" \
  '{spec: {template: {spec: {volumes: [{name: "static", hostPath: {path: $path, type: "Directory"}}]}}}}')"
"$KUBECTL" patch "deploy/$SERVER_DEPLOYMENT" -p "$patch" >/dev/null
if ! "$KUBECTL" rollout status "deploy/$SERVER_DEPLOYMENT" --timeout="$ROLLOUT_TIMEOUT"; then
  log "new build did not become Ready; undoing the rollout"
  "$KUBECTL" rollout undo "deploy/$SERVER_DEPLOYMENT" >/dev/null || log "WARNING: rollout undo failed, check deploy/$SERVER_DEPLOYMENT by hand"
  "$KUBECTL" rollout status "deploy/$SERVER_DEPLOYMENT" --timeout="$ROLLOUT_TIMEOUT" || true
  die "rollout of build $build_id failed; back on $previous_path"
fi

finished=1
log "refresh complete: $previous_path -> $HOST_DATA_DIR/build/$build_id"
