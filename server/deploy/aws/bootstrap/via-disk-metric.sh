#!/usr/bin/env bash
set -euo pipefail

dir="${VIA_TEXTFILE_DIR:-/var/lib/via/textfile}"
df_cmd="${VIA_DF_CMD:-df}"
mkdir -p "$dir"

tmp="$(mktemp "$dir/.via_disk.XXXXXX")"
trap 'rm -f "$tmp"' EXIT
{
  echo "# TYPE via_disk_avail_bytes gauge"
  echo "# TYPE via_disk_size_bytes gauge"
  "$df_cmd" -P -B1 / /var/lib/via | awk 'NR > 1 && !seen[$6]++ {
    printf "via_disk_avail_bytes{mountpoint=\"%s\"} %s\n", $6, $4
    printf "via_disk_size_bytes{mountpoint=\"%s\"} %s\n", $6, $2
  }'
} >"$tmp"
chmod 0644 "$tmp"
mv "$tmp" "$dir/via_disk.prom"
