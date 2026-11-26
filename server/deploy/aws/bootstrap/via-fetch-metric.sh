#!/usr/bin/env bash
set -euo pipefail

dir="${VIA_TEXTFILE_DIR:-/var/lib/via/textfile}"
prom="$dir/via_artifact_fetch.prom"
now="${VIA_NOW:-$(date +%s)}"
mkdir -p "$dir"

success=0
[ "${SERVICE_RESULT:-}" = success ] && success=1

last_success=0
if [ "$success" -eq 1 ]; then
  last_success="$now"
elif [ -f "$prom" ]; then
  last_success="$(awk '$1 == "via_artifact_fetch_last_success_timestamp_seconds" { print $2 }' "$prom")"
  last_success="${last_success:-0}"
fi

tmp="$(mktemp "$dir/.via_artifact_fetch.XXXXXX")"
trap 'rm -f "$tmp"' EXIT
cat >"$tmp" <<METRICS
# TYPE via_artifact_fetch_last_success gauge
via_artifact_fetch_last_success $success
# TYPE via_artifact_fetch_last_run_timestamp_seconds gauge
via_artifact_fetch_last_run_timestamp_seconds $now
# TYPE via_artifact_fetch_last_success_timestamp_seconds gauge
via_artifact_fetch_last_success_timestamp_seconds $last_success
METRICS
chmod 0644 "$tmp"
mv "$tmp" "$prom"
