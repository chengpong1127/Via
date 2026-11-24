#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$here/lib.sh"

usage() {
  echo "usage: VIA_ARTIFACT_BUCKET=<bucket> $0 <build-dir> [--tiles <valhalla-custom-files-dir>] [--tiles-id <id>]" >&2
  exit 2
}

[ $# -ge 1 ] || usage
build_dir="${1%/}"
shift
tiles_dir=""
tiles_id=""
while [ $# -gt 0 ]; do
  case "$1" in
    --tiles) tiles_dir="${2:?}"; shift 2 ;;
    --tiles-id) tiles_id="${2:?}"; shift 2 ;;
    *) usage ;;
  esac
done

require_bucket
command -v zstd >/dev/null 2>&1 || die "zstd is required"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

put_pointer() {
  printf '%s\n' "$2" >"$work/pointer"
  s3_put "$work/pointer" "$1/latest"
}

upload_object() {
  local prefix="$1" id="$2" payload="$3" payload_name="$4" raw_sha="$5"
  local manifest_key="$prefix/$id/manifest" payload_key="$prefix/$id/$payload_name"
  local existing rc=0 zst_sha existing_zst

  zst_sha="$(sha256 "$payload")"
  existing="$(s3_get_text "$manifest_key")" || rc=$?
  if [ "$rc" -eq 3 ]; then
    printf 'raw %s\nzst %s\n' "$raw_sha" "$zst_sha" >"$work/manifest"
    rc=0
    s3_put_new "$work/manifest" "$manifest_key" || rc=$?
    if [ "$rc" -eq 4 ]; then
      existing="$(s3_get_text "$manifest_key")" || die "cannot read $manifest_key"
    elif [ "$rc" -ne 0 ]; then
      die "cannot write $manifest_key"
    else
      existing="$(cat "$work/manifest")"
    fi
  elif [ "$rc" -ne 0 ]; then
    die "cannot read $manifest_key"
  fi

  [ "$(manifest_value "$existing" raw)" = "$raw_sha" ] ||
    die "$prefix/$id already exists with different content; pick a new id"

  rc=0
  s3_exists "$payload_key" || rc=$?
  if [ "$rc" -eq 0 ]; then
    log "$prefix/$id already published"
    return
  elif [ "$rc" -ne 3 ]; then
    die "cannot check $payload_key"
  fi
  existing_zst="$(manifest_value "$existing" zst)"
  [ "$existing_zst" = "$zst_sha" ] || die "$payload_key is missing and recompressing gives different bytes than the manifest records; pick a new id"
  s3_put "$payload" "$payload_key"
  log "uploaded $payload_key"
}

static_file="$build_dir/.via-static.json"
[ -f "$static_file" ] || die "$static_file not found"
static_id="$(basename "$build_dir")"
valid_id "$static_id" || die "invalid build id: $static_id"
zstd -q -19 -T0 -o "$work/via-static.json.zst" "$static_file"
upload_object static "$static_id" "$work/via-static.json.zst" via-static.json.zst "$(sha256 "$static_file")"

if [ -n "$tiles_dir" ]; then
  [ -f "$tiles_dir/valhalla.json" ] || die "$tiles_dir/valhalla.json not found"
  tiles_dir="${tiles_dir%/}"
  tree="$(tree_hash "$tiles_dir")"
  [ -n "$tiles_id" ] || tiles_id="${tree:0:16}"
  valid_id "$tiles_id" || die "invalid tiles id: $tiles_id"
  (
    cd "$tiles_dir"
    find . -type f ! -name '*.osm.pbf' | LC_ALL=C sort >"$work/files.txt"
    tar -cf - -T "$work/files.txt" | zstd -q -T0 -o "$work/tiles.tar.zst"
  )
  upload_object valhalla "$tiles_id" "$work/tiles.tar.zst" tiles.tar.zst "$tree"
  put_pointer valhalla "$tiles_id"
fi

put_pointer static "$static_id"
log "published; static/latest is $static_id"
