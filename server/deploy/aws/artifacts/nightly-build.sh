#!/usr/bin/env bash
# Nightly static-data job (runs in the server image as a Kubernetes CronJob).
#
# Fetches the latest GTFS with via-data's paced fetch (GTFS only once a snapshot
# exists), skips everything when the GTFS checksum is the one already built,
# otherwise builds the static artifact and hands it to the node host through
# $OUT_DIR/incoming. The host (fetch.sh --incoming, triggered by
# via-static-activate.path) validates, activates, restarts and rolls back.
set -euo pipefail

VIA_DATA="${VIA_DATA:-via-data}"
PIPELINE_DIR="${PIPELINE_DIR:-/build/server/data}"
OUT_DIR="${OUT_DIR:-/out}"
KEEP_SNAPSHOTS="${VIA_KEEP_SNAPSHOTS:-2}"
KEEP_BUILDS="${VIA_KEEP_BUILDS:-1}"

log() { echo "$(date -u +%H:%M:%S) $*" >&2; }
die() { log "ERROR: $*"; exit 1; }

fetch_dir="$PIPELINE_DIR/fetch"
build_dir="$PIPELINE_DIR/build"
built_marker="$PIPELINE_DIR/.built-gtfs-sha"
mkdir -p "$fetch_dir" "$build_dir"

# One job at a time (a manual `kubectl create job` ignores concurrencyPolicy).
if command -v flock >/dev/null 2>&1; then
  exec 9>"$PIPELINE_DIR/.lock"
  flock -n 9 || die "another nightly job is running"
fi

gtfs_sha() {
  jq -r '.sources.gtfs.sha256 // empty' "$fetch_dir/$1/manifest.json"
}

prune() {
  local dir="$1" keep="$2" count=0 name
  # shellcheck disable=SC2012
  ls -1t "$dir" | while IFS= read -r name; do
    case "$name" in .*|latest) continue ;; esac
    [ -d "$dir/$name" ] || continue
    count=$((count + 1))
    [ "$count" -le "$keep" ] || rm -rf "${dir:?}/$name"
  done
}

# Prune on every exit, including failures, so a failing build cannot fill the disk.
trap 'prune "$fetch_dir" "$KEEP_SNAPSHOTS"; prune "$build_dir" "$KEEP_BUILDS"' EXIT

prev=""
if [ -L "$fetch_dir/latest" ]; then prev="$(basename "$(readlink "$fetch_dir/latest")")"; fi

if [ -n "$prev" ]; then
  log "fetching GTFS only (previous snapshot $prev)"
  "$VIA_DATA" fetch --gtfs-only || die "fetch failed"
else
  log "no previous snapshot: full paced fetch"
  "$VIA_DATA" fetch || die "fetch failed"
fi

latest="$(basename "$(readlink "$fetch_dir/latest")")"
sha="$(gtfs_sha "$latest")"
[ -n "$sha" ] || die "snapshot $latest has no GTFS checksum"

if [ -f "$built_marker" ] && [ "$(cat "$built_marker")" = "$sha" ]; then
  log "GTFS checksum unchanged ($sha); nothing to build"
  if [ -n "$prev" ] && [ "$latest" != "$prev" ]; then
    rm -rf "${fetch_dir:?}/$latest"
    ln -sfn "$prev" "$fetch_dir/latest"
  fi
  exit 0
fi

log "GTFS checksum $sha not built yet; building"
"$VIA_DATA" build --input "$fetch_dir/latest" || die "build failed"
artifact="$(readlink -f "$build_dir/.via-static-latest.json")"
[ -s "$artifact" ] || die "build produced no artifact"
id="$(basename "$(dirname "$artifact")")"

incoming="$OUT_DIR/incoming"
[ ! -L "$incoming" ] || die "$incoming is a symlink"
mkdir -p "$incoming"
rm -f "$incoming/READY"
find "$incoming" -mindepth 1 -maxdepth 1 -type d -exec rm -rf {} +
stage="$incoming/.stage-$id"
mkdir -p "$stage"
cp "$artifact" "$stage/.via-static.json"
echo "$sha" >"$stage/.gtfs-sha"
mv "$stage" "$incoming/$id"
echo "$id" >"$incoming/.READY.tmp"
mv "$incoming/.READY.tmp" "$incoming/READY"
log "handed over static build $id (the host records $sha as built after activation)"
