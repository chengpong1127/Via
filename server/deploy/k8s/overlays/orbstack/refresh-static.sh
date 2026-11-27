#!/usr/bin/env bash
# Weekly static-data refresh, run by the weekly-refresh CronJob (docs/local-orbstack.md).
#
# One staged job: fetch (GTFS and every TDX file, paced), build, validate, activate.
# Activation points the server Deployment's static hostPath at the new build; the
# rolling update keeps the old pod serving until the new one is Ready, and a failed
# rollout is undone. Anything that fails before activation leaves the server on the
# build it already uses. Nothing under $DATA_DIR is ever deleted.
set -euo pipefail

VIA_DATA="${VIA_DATA:-via-data}"
KUBECTL="${KUBECTL:-kubectl}"
DATA_DIR="${DATA_DIR:-/build/server/data}"
HOST_DATA_DIR="${HOST_DATA_DIR:?HOST_DATA_DIR (the node path of the data directory) is required}"
SERVER_DEPLOYMENT="${SERVER_DEPLOYMENT:-server}"
SOURCE_CONFIG="${VIA_CONFIG:-/etc/via/via.toml}"
REQUESTS_PER_MINUTE="${VIA_FETCH_REQUESTS_PER_MINUTE:-60}"
ROLLOUT_TIMEOUT="${ROLLOUT_TIMEOUT:-600s}"

log() { echo "$(date -u +%H:%M:%S) $*" >&2; }
die() { log "ERROR: $*"; exit 1; }

case "$REQUESTS_PER_MINUTE" in
  '' | *[!0-9]* | 0) die "VIA_FETCH_REQUESTS_PER_MINUTE must be a positive integer, got '$REQUESTS_PER_MINUTE'" ;;
esac

build_dir="$DATA_DIR/build"
latest_link="$build_dir/.via-static-latest.json"
work="$(mktemp -d)"
previous_latest=""
new_latest=""
finished=0

restore_latest_link() {
  # Only the link `via-data build` moved; the build directories stay. Restore it
  # only if nobody else has moved it since.
  if [ -n "$previous_latest" ] && [ -n "$new_latest" ] && [ "$(readlink "$latest_link" || true)" = "$new_latest" ]; then
    ln -sfn "$previous_latest" "$latest_link"
    log "restored $latest_link -> $previous_latest"
  fi
}

on_exit() {
  status=$?
  rm -rf "$work"
  if [ "$finished" != 1 ]; then
    restore_latest_link
    log "refresh failed (exit $status); the server keeps the build it was using"
  fi
}
trap on_exit EXIT

if command -v flock >/dev/null 2>&1; then
  mkdir -p "$DATA_DIR"
  exec 9>"$DATA_DIR/.refresh.lock"
  flock -n 9 || die "another refresh is running"
fi

server_path() {
  "$KUBECTL" get "deploy/$SERVER_DEPLOYMENT" \
    -o 'jsonpath={.spec.template.spec.volumes[?(@.name=="static")].hostPath.path}'
}

# Fail before any TDX call if the server cannot be reached or has no static hostPath.
previous_path="$(server_path)" || die "cannot read deployment $SERVER_DEPLOYMENT (RBAC or not deployed)"
[ -n "$previous_path" ] || die "deployment $SERVER_DEPLOYMENT has no static hostPath volume"
log "server currently serves $previous_path"

# The shipped config with only the fetch pacing changed.
config="$work/via.toml"
sed -E "s/^(requests_per_minute[[:space:]]*=[[:space:]]*)[0-9]+/\\1$REQUESTS_PER_MINUTE/" "$SOURCE_CONFIG" >"$config"
[ "$(grep -c "^requests_per_minute = $REQUESTS_PER_MINUTE\$" "$config")" = 1 ] \
  || die "expected exactly one requests_per_minute line in $SOURCE_CONFIG"
"$VIA_DATA" --config "$config" validate-config >/dev/null

previous_latest="$(readlink "$latest_link" || true)"

log "fetching GTFS and TDX files at $REQUESTS_PER_MINUTE requests/minute"
"$VIA_DATA" --config "$config" fetch | tee "$work/fetch.log" || die "fetch failed"
snapshot="$(sed -n 's/^fetch snapshot: //p' "$work/fetch.log" | tail -1)"
[ -n "$snapshot" ] && [ -f "$snapshot/manifest.json" ] || die "fetch did not report a snapshot"
reused="$(jq -r '(.reused_sources // {}) | keys | join(", ")' "$snapshot/manifest.json")"
[ -z "$reused" ] || die "partial fetch, TDX files reused from an earlier snapshot: $reused; not building"

log "building from $snapshot"
"$VIA_DATA" --config "$config" build --input "$snapshot" | tee "$work/build.log" || die "build failed"
artifact="$(sed -n 's/^static artifact: //p' "$work/build.log" | tail -1)"
[ -n "$artifact" ] && [ -s "$artifact" ] || die "build did not produce an artifact"
new_latest="$(readlink "$latest_link" || true)"
build_id="$(basename "$(dirname "$artifact")")"

log "validating $artifact"
"$VIA_DATA" --config "$config" validate --static "$artifact" >/dev/null || die "artifact $build_id is invalid"

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
