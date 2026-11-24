#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$here/lib.sh"

if [ -f /etc/via-bootstrap.env ]; then
  # shellcheck source=/dev/null
  . /etc/via-bootstrap.env
fi
if [ -z "${AWS_DEFAULT_REGION:-}" ] && [ -n "${VIA_REGION:-}" ]; then
  export AWS_DEFAULT_REGION="$VIA_REGION"
fi
export PATH="$PATH:/snap/bin:/usr/local/bin"

usage() {
  echo "usage: $0 [--static [id]] [--tiles [id]] | --incoming   (no flags: both, latest)" >&2
  exit 2
}

VIA_ROOT="${VIA_ROOT:-/var/lib/via}"
KEEP="${VIA_KEEP_BUILDS:-3}"
MAX_ATTEMPTS="${VIA_MAX_ATTEMPTS:-2}"
ROLLOUT_TIMEOUT="${VIA_ROLLOUT_TIMEOUT:-900s}"
KUBECTL="${KUBECTL:-kubectl}"
CTR="${CTR:-k3s ctr -n k8s.io}"
export KUBECONFIG="${KUBECONFIG:-/etc/rancher/k3s/k3s.yaml}"

do_static=0
do_tiles=0
incoming=0
explicit=0
static_id=""
tiles_id=""
while [ $# -gt 0 ]; do
  case "$1" in
    --static)
      do_static=1
      explicit=1
      shift
      if [ $# -gt 0 ] && [[ "$1" != --* ]]; then static_id="$1"; shift; fi
      ;;
    --tiles)
      do_tiles=1
      explicit=1
      shift
      if [ $# -gt 0 ] && [[ "$1" != --* ]]; then tiles_id="$1"; shift; fi
      ;;
    --incoming)
      incoming=1
      do_static=1
      shift
      ;;
    *) usage ;;
  esac
done
if [ "$incoming" -eq 1 ] && [ "$explicit" -eq 1 ]; then
  echo "--incoming cannot be combined with --static or --tiles" >&2
  exit 2
fi
if [ "$do_static" -eq 0 ] && [ "$do_tiles" -eq 0 ]; then
  do_static=1
  do_tiles=1
fi

if [ "$incoming" -eq 0 ]; then require_bucket; fi
mkdir -p "$VIA_ROOT"

# Consume READY before anything can fail (lock, cluster, tools): the path unit
# fires on READY, so leaving it behind would restart this service in a loop.
# (zstd/jq are checked after this for the same reason.) The build stays in incoming/<id>; the job rebuilds next night because the
# built-checksum marker is only written after a successful activation.
taken=""
if [ "$incoming" -eq 1 ]; then
  inc_dir="$VIA_ROOT/static/incoming"
  if [ -d "$inc_dir/READY" ] && [ ! -L "$inc_dir/READY" ]; then
    rm -rf "$inc_dir/READY"
    die "incoming/READY is a directory"
  elif [ -f "$inc_dir/READY" ] && [ ! -L "$inc_dir/READY" ] && [ ! -L "$inc_dir" ]; then
    mv -f "$inc_dir/READY" "$inc_dir/READY.taken"
    taken="$(tr -d '[:space:]' <"$inc_dir/READY.taken")"
  elif [ -L "$inc_dir" ] || [ -L "$inc_dir/READY" ]; then
    rm -f "$inc_dir/READY"
    die "incoming is a symlink"
  else
    log "no incoming build"
    exit 0
  fi
fi
command -v zstd >/dev/null 2>&1 || die "zstd is required"
command -v jq >/dev/null 2>&1 || die "jq is required"

if [ -z "${VIA_FETCH_CHILD:-}" ]; then
  if command -v flock >/dev/null 2>&1; then
    exec 9>"${VIA_LOCK_FILE:-/run/via-fetch.lock}"
    if [ "$incoming" -eq 1 ]; then
      flock -w "${VIA_LOCK_WAIT:-7200}" 9 || die "fetch lock still busy after ${VIA_LOCK_WAIT:-7200}s"
    else
      flock -n 9 || die "another fetch is running"
    fi
  else
    lockdir="${VIA_LOCK_FILE:-$VIA_ROOT/.fetch.lock}.d"
    waited=0
    until mkdir "$lockdir" 2>/dev/null; do
      [ "$incoming" -eq 1 ] && [ "$waited" -lt "${VIA_LOCK_WAIT:-7200}" ] || die "another fetch is running ($lockdir)"
      sleep 1
      waited=$((waited + 1))
    done
    trap 'rmdir "$lockdir"' EXIT
  fi
  for stale in "$VIA_ROOT"/.fetch.*; do
    [ -d "$stale" ] && [ "$stale" != "${lockdir:-}" ] && rm -rf "$stale"
  done
  if [ "$do_static" -eq 1 ] && [ "$do_tiles" -eq 1 ]; then
    rc=0
    VIA_FETCH_CHILD=1 "$0" --static ${static_id:+"$static_id"} || rc=1
    VIA_FETCH_CHILD=1 "$0" --tiles ${tiles_id:+"$tiles_id"} || rc=1
    exit "$rc"
  fi
  export VIA_FETCH_CHILD=1
fi

work="$(mktemp -d "$VIA_ROOT/.fetch.XXXXXX")"
trap 'rm -rf "$work"; [ -z "${lockdir:-}" ] || rmdir "$lockdir" 2>/dev/null || true' EXIT

if mv --version >/dev/null 2>&1; then
  replace_link() { mv -fT "$1" "$2"; }
else
  replace_link() { mv -fh "$1" "$2"; }
fi

set_link() {
  rm -f "$VIA_ROOT/static/current.new"
  ln -sfn "$2" "$VIA_ROOT/static/current.new"
  replace_link "$VIA_ROOT/static/current.new" "$VIA_ROOT/static/$1"
}

wait_for_cluster() {
  local deadline=$((SECONDS + ${VIA_CLUSTER_WAIT:-120}))
  until "$KUBECTL" get --raw /readyz >/dev/null 2>&1; do
    [ "$SECONDS" -lt "$deadline" ] || die "Kubernetes API not ready"
    sleep 3
  done
}

resolve_id() {
  local prefix="$1" requested="$2" id rc=0
  if [ -n "$requested" ]; then
    id="$requested"
  else
    id="$(s3_get_text "$prefix/latest")" || rc=$?
    [ "$rc" -eq 0 ] || die "cannot read $prefix/latest (rc=$rc)"
    id="$(tr -d '[:space:]' <<<"$id")"
  fi
  valid_id "$id" || die "invalid id: $id"
  echo "$id"
}

attempts() {
  if [ -f "$1.rejected" ]; then cat "$1.rejected"; else echo 0; fi
}

record_failure() {
  echo $(($(attempts "$1") + 1)) >"$1.rejected"
}

download_verified() {
  local prefix="$1" id="$2" payload_name="$3" manifest zst
  manifest="$(s3_get_text "$prefix/$id/manifest")" || die "no manifest for $prefix/$id"
  RAW_SHA="$(manifest_value "$manifest" raw)"
  zst="$(manifest_value "$manifest" zst)"
  [ -n "$RAW_SHA" ] && [ -n "$zst" ] || die "malformed manifest for $prefix/$id"
  s3_get "$prefix/$id/$payload_name" "$work/$payload_name"
  [ "$(sha256 "$work/$payload_name")" = "$zst" ] || die "sha256 mismatch for $prefix/$id/$payload_name"
  zstd -q -t "$work/$payload_name" || die "$payload_name is not a valid zstd file"
}

restart_and_wait() {
  local deploy="$1"
  if ! "$KUBECTL" -n via get "deploy/$deploy" >/dev/null 2>&1; then
    log "deploy/$deploy does not exist yet; skipping restart"
    return 0
  fi
  "$KUBECTL" -n via rollout restart "deploy/$deploy" &&
    "$KUBECTL" -n via rollout status "deploy/$deploy" --timeout="$ROLLOUT_TIMEOUT"
}

prune() {
  local dir="$1" protect_a="$2" protect_b="$3" count=0 name
  # shellcheck disable=SC2012
  ls -1t "$dir" | while IFS= read -r name; do
    case "$name" in .*|*.rejected) continue ;; esac
    count=$((count + 1))
    if [ "$count" -gt "$KEEP" ] && [ "$name" != "$protect_a" ] && [ "$name" != "$protect_b" ]; then
      rm -rf "${dir:?}/$name" "${dir:?}/$name.rejected"
    fi
  done
}

server_image() {
  if [ -n "${VIA_SERVER_IMAGE:-}" ]; then
    echo "$VIA_SERVER_IMAGE"
  else
    "$KUBECTL" -n via get deploy/server -o 'jsonpath={.spec.template.spec.containers[0].image}' 2>/dev/null || true
  fi
}

validate_static() {
  local dir="$1" image
  if [ -n "${VIA_VALIDATE_CMD:-}" ]; then
    $VIA_VALIDATE_CMD "$dir"
    return
  fi
  if command -v via-data >/dev/null 2>&1; then
    via-data validate --static "$dir/.via-static.json"
    return
  fi
  image="$(server_image)"
  if [ -n "$image" ]; then
    # shellcheck disable=SC2086
    $CTR images ls -q 2>/dev/null | grep -qxF "$image" || $CTR images pull "$image" >/dev/null
    # shellcheck disable=SC2086
    $CTR run --rm --mount "type=bind,src=$dir,dst=/data,options=rbind:ro" "$image" "via-validate-$$" \
      via-data validate --static /data/.via-static.json
    return
  fi
  if [ "${VIA_ALLOW_UNVALIDATED:-0}" = 1 ]; then
    log "WARNING: no via-data and no server image known; activating unvalidated (VIA_ALLOW_UNVALIDATED=1)"
    return 0
  fi
  log "no way to run via-data validate: install via-data, deploy the server first, or set VIA_SERVER_IMAGE (or VIA_ALLOW_UNVALIDATED=1)"
  return 1
}

# Build handed over by the nightly CronJob: $base/incoming/<id> plus READY
# holding the id. READY is always consumed so a failure is not retried in a loop.
fetch_incoming() {
  local base="$VIA_ROOT/static" id="$taken" taken_file="$VIA_ROOT/static/incoming/READY.taken" rc=0 gsha=""
  rm -f "$taken_file"
  valid_id "$id" || die "invalid incoming id: $id"
  [ ! -L "$base/incoming/$id" ] || die "incoming $id is a symlink"
  [ -f "$base/incoming/$id/.via-static.json" ] && [ ! -L "$base/incoming/$id/.via-static.json" ] &&
    [ -s "$base/incoming/$id/.via-static.json" ] || die "incoming $id has no regular artifact"
  # Read the checksum from the incoming copy now, even if builds/$id already exists.
  if [ -f "$base/incoming/$id/.gtfs-sha" ] && [ ! -L "$base/incoming/$id/.gtfs-sha" ]; then
    gsha="$(tr -d '[:space:]' <"$base/incoming/$id/.gtfs-sha")"
    [[ "$gsha" =~ ^[A-Za-z0-9-]{1,128}$ ]] || gsha=""
  fi
  if [ ! -d "$base/builds/$id" ]; then
    jq --stream -n 'reduce inputs as $i (0; . + 1) | select(. > 0)' "$base/incoming/$id/.via-static.json" >/dev/null ||
      die "incoming $id is not valid JSON"
    touch "$base/incoming/$id/.via-verified"
    mkdir -p "$base/builds"
    mv "$base/incoming/$id" "$base/builds/$id"
  fi
  activate_static "$id" || rc=$?
  if [ "$rc" -eq 0 ] && [ -n "$gsha" ]; then
    mkdir -p "$VIA_ROOT/pipeline"
    echo "$gsha" >"$VIA_ROOT/pipeline/.built-gtfs-sha"
  fi
  rm -rf "${base:?}/incoming/$id"
  return "$rc"
}

fetch_static() {
  local id
  id="$(resolve_id static "$static_id")"
  activate_static "$id"
}

activate_static() {
  local id="$1" base="$VIA_ROOT/static" builds prev stage
  builds="$base/builds"
  mkdir -p "$builds"
  prev=""
  if [ -L "$base/current" ]; then prev="$(readlink "$base/current")"; fi

  if [ "$(attempts "$builds/$id")" -ge "$MAX_ATTEMPTS" ]; then
    log "static $id failed $(attempts "$builds/$id") times; remove $builds/$id.rejected to retry"
    return 1
  fi
  if [ "$prev" = "builds/$id" ]; then
    log "static $id already active"
    return 0
  fi

  if [ ! -f "$builds/$id/.via-verified" ]; then
    download_verified static "$id" via-static.json.zst
    stage="$work/stage"
    mkdir -p "$stage"
    zstd -q -d -o "$stage/.via-static.json" "$work/via-static.json.zst"
    [ "$(sha256 "$stage/.via-static.json")" = "$RAW_SHA" ] || die "sha256 mismatch after decompression of static $id"
    jq --stream -n 'reduce inputs as $i (0; . + 1) | select(. > 0)' "$stage/.via-static.json" >/dev/null ||
      die "static $id is not valid JSON"
    touch "$stage/.via-verified"
    rm -rf "${builds:?}/$id"
    mv "$stage" "$builds/$id"
  fi

  if ! validate_static "$builds/$id"; then
    log "validation failed for static $id; keeping ${prev:-nothing}"
    record_failure "$builds/$id"
    return 1
  fi

  set_link current "builds/$id"
  log "activated static $id"
  if ! restart_and_wait server; then
    log "server not ready with static $id; restoring ${prev:-nothing}"
    record_failure "$builds/$id"
    if [ -n "$prev" ]; then
      set_link current "$prev"
      restart_and_wait server || log "server still not ready after restore"
    else
      rm -f "$base/current"
    fi
    return 1
  fi
  rm -f "$builds/$id.rejected"
  prune "$builds" "$id" "$(basename "${prev:-none}")"
}

fetch_tiles() {
  local active="$VIA_ROOT/valhalla" archive="$VIA_ROOT/valhalla-builds" id prev stage prev_dir=""
  id="$(resolve_id valhalla "$tiles_id")"
  mkdir -p "$archive"
  prev=""
  if [ -f "$active/.tiles-id" ]; then prev="$(cat "$active/.tiles-id")"; fi

  if [ "$(attempts "$archive/$id")" -ge "$MAX_ATTEMPTS" ]; then
    log "tiles $id failed $(attempts "$archive/$id") times; remove $archive/$id.rejected to retry"
    return 1
  fi
  if [ "$prev" = "$id" ]; then
    log "tiles $id already active"
    return 0
  fi

  stage="$archive/$id"
  if [ ! -f "$stage/.via-verified" ]; then
    download_verified valhalla "$id" tiles.tar.zst
    mkdir -p "$work/stage"
    zstd -q -d -c "$work/tiles.tar.zst" | tar -xf - -C "$work/stage"
    [ -f "$work/stage/valhalla.json" ] || die "tiles $id has no valhalla.json"
    [ "$(tree_hash "$work/stage")" = "$RAW_SHA" ] || die "tiles $id content hash mismatch after extraction"
    echo "$id" >"$work/stage/.tiles-id"
    touch "$work/stage/.via-verified"
    rm -rf "${archive:?}/$id"
    mv "$work/stage" "$stage"
  fi

  mkdir -p "$active"
  if [ -n "$(ls -A "$active" 2>/dev/null)" ]; then
    prev_dir="$archive/${prev:-initial-$(date -u +%Y%m%dT%H%M%SZ)}"
    rm -rf "${prev_dir:?}"
    mv "$active" "$prev_dir"
  else
    rmdir "$active"
  fi
  mv "$stage" "$active"
  log "activated tiles $id"
  if ! restart_and_wait valhalla; then
    log "valhalla not ready with tiles $id; restoring ${prev_dir:-nothing}"
    record_failure "$archive/$id"
    mv "$active" "$archive/$id"
    if [ -n "$prev_dir" ]; then
      mv "$prev_dir" "$active"
      restart_and_wait valhalla || log "valhalla still not ready after restore"
    fi
    return 1
  fi
  rm -f "$archive/$id.rejected"
  prune "$archive" "$id" "$(basename "${prev_dir:-none}")"
}

wait_for_cluster
if [ "$incoming" -eq 1 ]; then fetch_incoming
elif [ "$do_static" -eq 1 ]; then fetch_static; fi
if [ "$do_tiles" -eq 1 ]; then fetch_tiles; fi
