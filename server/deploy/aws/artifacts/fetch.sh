#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$here/lib.sh"

if [ -f /etc/via-bootstrap.env ]; then
  # shellcheck source=/dev/null
  . /etc/via-bootstrap.env
fi

usage() {
  echo "usage: $0 [--static [id]] [--tiles [id]]   (no flags: both, latest)" >&2
  exit 2
}

VIA_ROOT="${VIA_ROOT:-/var/lib/via}"
KEEP="${VIA_KEEP_BUILDS:-3}"
ROLLOUT_TIMEOUT="${VIA_ROLLOUT_TIMEOUT:-300s}"
KUBECTL="${KUBECTL:-kubectl}"
export KUBECONFIG="${KUBECONFIG:-/etc/rancher/k3s/k3s.yaml}"

do_static=0
do_tiles=0
static_id=""
tiles_id=""
while [ $# -gt 0 ]; do
  case "$1" in
    --static)
      do_static=1
      shift
      if [ $# -gt 0 ] && [[ "$1" != --* ]]; then static_id="$1"; shift; fi
      ;;
    --tiles)
      do_tiles=1
      shift
      if [ $# -gt 0 ] && [[ "$1" != --* ]]; then tiles_id="$1"; shift; fi
      ;;
    *) usage ;;
  esac
done
if [ "$do_static" -eq 0 ] && [ "$do_tiles" -eq 0 ]; then
  do_static=1
  do_tiles=1
fi

require_bucket
command -v zstd >/dev/null 2>&1 || die "zstd is required"

if command -v flock >/dev/null 2>&1; then
  lock="${VIA_LOCK_FILE:-/run/via-fetch.lock}"
  exec 9>"$lock"
  flock -n 9 || die "another fetch is running"
fi

mkdir -p "$VIA_ROOT"
work="$(mktemp -d "$VIA_ROOT/.fetch.XXXXXX")"
trap 'rm -rf "$work"' EXIT

if mv --version >/dev/null 2>&1; then
  replace_link() { mv -fT "$1" "$2"; }
else
  replace_link() { mv -fh "$1" "$2"; }
fi

resolve_id() {
  local prefix="$1" requested="$2" id
  if [ -n "$requested" ]; then
    id="$requested"
  else
    id="$(s3_get_text "$prefix/latest")" || die "cannot read $prefix/latest"
    id="$(tr -d '[:space:]' <<<"$id")"
  fi
  valid_id "$id" || die "invalid id: $id"
  echo "$id"
}

download_verified() {
  local prefix="$1" id="$2" payload_name="$3" dest="$4" manifest raw zst
  manifest="$(s3_get_text "$prefix/$id/manifest")" || die "no manifest for $prefix/$id"
  raw="$(manifest_value "$manifest" raw)"
  zst="$(manifest_value "$manifest" zst)"
  [ -n "$raw" ] && [ -n "$zst" ] || die "malformed manifest for $prefix/$id"
  s3_get "$prefix/$id/$payload_name" "$work/$payload_name"
  [ "$(sha256 "$work/$payload_name")" = "$zst" ] || die "sha256 mismatch for $prefix/$id/$payload_name"
  mkdir -p "$dest"
  echo "$raw" >"$work/raw.sha"
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

run_validate() {
  local path="$1"
  if [ -n "${VIA_VALIDATE_CMD:-}" ]; then
    $VIA_VALIDATE_CMD "$path"
  elif command -v via-data >/dev/null 2>&1; then
    via-data validate --static "$path/.via-static.json"
  else
    log "via-data not installed: skipping offline validation; the readiness gate will roll back a bad artifact"
    [ -s "$path/.via-static.json" ]
  fi
}

fetch_static() {
  local base="$VIA_ROOT/static" id builds prev
  id="$(resolve_id static "$static_id")"
  builds="$base/builds"
  mkdir -p "$builds"
  prev=""
  if [ -L "$base/current" ]; then prev="$(readlink "$base/current")"; fi

  if [ "$prev" = "builds/$id" ]; then
    log "static $id already active"
    return 0
  fi
  if [ -e "$builds/$id.rejected" ]; then
    log "static $id was rejected earlier; remove $builds/$id.rejected to retry"
    return 0
  fi

  if [ ! -f "$builds/$id/.via-verified" ]; then
    download_verified static "$id" via-static.json.zst "$work/stage"
    zstd -q -d -o "$work/stage/.via-static.json" "$work/via-static.json.zst"
    [ "$(sha256 "$work/stage/.via-static.json")" = "$(cat "$work/raw.sha")" ] || die "sha256 mismatch after decompression of static $id"
    touch "$work/stage/.via-verified"
    rm -rf "${builds:?}/$id"
    mv "$work/stage" "$builds/$id"
  fi

  if ! run_validate "$builds/$id"; then
    log "validation failed for static $id; keeping ${prev:-nothing}"
    touch "$builds/$id.rejected"
    return 1
  fi

  ln -s "builds/$id" "$base/current.new"
  replace_link "$base/current.new" "$base/current"
  log "activated static $id"
  if ! restart_and_wait server; then
    log "server not ready with static $id; restoring ${prev:-nothing}"
    touch "$builds/$id.rejected"
    if [ -n "$prev" ]; then
      ln -s "$prev" "$base/current.new"
      replace_link "$base/current.new" "$base/current"
      restart_and_wait server || log "server still not ready after restore"
    fi
    return 1
  fi
  prune "$builds" "$id" "$(basename "${prev:-none}")"
}

fetch_tiles() {
  local active="$VIA_ROOT/valhalla" archive="$VIA_ROOT/valhalla-builds" id prev stage
  id="$(resolve_id valhalla "$tiles_id")"
  mkdir -p "$archive"
  prev=""
  if [ -f "$active/.tiles-id" ]; then prev="$(cat "$active/.tiles-id")"; fi

  if [ "$prev" = "$id" ]; then
    log "tiles $id already active"
    return 0
  fi
  if [ -e "$archive/$id.rejected" ]; then
    log "tiles $id was rejected earlier; remove $archive/$id.rejected to retry"
    return 0
  fi

  stage="$archive/$id"
  if [ ! -f "$stage/.via-verified" ]; then
    download_verified valhalla "$id" tiles.tar.zst "$work/stage"
    zstd -q -d -c "$work/tiles.tar.zst" | tar -xf - -C "$work/stage"
    [ -f "$work/stage/valhalla.json" ] || die "tiles $id has no valhalla.json"
    echo "$id" >"$work/stage/.tiles-id"
    touch "$work/stage/.via-verified"
    rm -rf "${archive:?}/$id"
    mv "$work/stage" "$stage"
  fi

  local prev_dir=""
  mkdir -p "$active"
  if [ -n "$(ls -A "$active" 2>/dev/null)" ]; then
    prev_dir="$archive/${prev:-initial-$(date -u +%Y%m%dT%H%M%SZ)}"
    [ -e "$prev_dir" ] && rm -rf "${prev_dir:?}"
    mv "$active" "$prev_dir"
  else
    rmdir "$active"
  fi
  mv "$stage" "$active"
  log "activated tiles $id"
  if ! restart_and_wait valhalla; then
    log "valhalla not ready with tiles $id; restoring ${prev_dir:-nothing}"
    mv "$active" "$archive/$id"
    touch "$archive/$id.rejected"
    if [ -n "$prev_dir" ]; then
      mv "$prev_dir" "$active"
      restart_and_wait valhalla || log "valhalla still not ready after restore"
    fi
    return 1
  fi
  prune "$archive" "$id" "$(basename "${prev_dir:-none}")"
}

if [ "$do_static" -eq 1 ]; then fetch_static; fi
if [ "$do_tiles" -eq 1 ]; then fetch_tiles; fi
