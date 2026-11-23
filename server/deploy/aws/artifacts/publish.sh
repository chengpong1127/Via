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

publish_object() {
  local prefix="$1" id="$2" payload="$3" payload_name="$4" raw_sha="$5"
  local manifest_key="$prefix/$id/manifest" existing rc=0

  existing="$(s3_get_text "$manifest_key")" || rc=$?
  if [ "$rc" -eq 0 ]; then
    if [ "$(manifest_value "$existing" raw)" = "$raw_sha" ]; then
      log "$prefix/$id already published with identical content"
      s3_put_pointer "$prefix" "$id"
      return
    fi
    die "$prefix/$id already exists with different content; pick a new id"
  elif [ "$rc" -ne 3 ]; then
    die "cannot read $manifest_key"
  fi

  local zst_sha
  zst_sha="$(sha256 "$payload")"
  s3_put "$payload" "$prefix/$id/$payload_name"
  printf 'raw %s\nzst %s\n' "$raw_sha" "$zst_sha" >"$work/manifest"
  s3_put "$work/manifest" "$manifest_key"
  s3_put_pointer "$prefix" "$id"
  log "published $prefix/$id"
}

s3_put_pointer() {
  printf '%s\n' "$2" >"$work/pointer"
  s3_put "$work/pointer" "$1/latest"
}

tree_hash() {
  (
    cd "$1"
    find . -type f ! -name '*.osm.pbf' | LC_ALL=C sort | while IFS= read -r f; do
      printf '%s  %s\n' "$(sha256 "$f")" "$f"
    done
  ) >"$work/tree.txt"
  sha256 "$work/tree.txt"
}

static_file="$build_dir/.via-static.json"
[ -f "$static_file" ] || die "$static_file not found"
static_id="$(basename "$build_dir")"
valid_id "$static_id" || die "invalid build id: $static_id"
zstd -q -19 -T0 -o "$work/via-static.json.zst" "$static_file"
publish_object static "$static_id" "$work/via-static.json.zst" via-static.json.zst "$(sha256 "$static_file")"

if [ -n "$tiles_dir" ]; then
  [ -f "$tiles_dir/valhalla.json" ] || die "$tiles_dir/valhalla.json not found"
  tiles_dir="${tiles_dir%/}"
  tree="$(tree_hash "$tiles_dir")"
  [ -n "$tiles_id" ] || tiles_id="$(date -u +%Y%m%dT%H%M%SZ)-${tree:0:8}"
  valid_id "$tiles_id" || die "invalid tiles id: $tiles_id"
  (
    cd "$tiles_dir"
    find . -type f ! -name '*.osm.pbf' | LC_ALL=C sort >"$work/files.txt"
    tar -cf - -T "$work/files.txt" | zstd -q -T0 -o "$work/tiles.tar.zst"
  )
  publish_object valhalla "$tiles_id" "$work/tiles.tar.zst" tiles.tar.zst "$tree"
fi
