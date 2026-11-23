#!/usr/bin/env bash
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="$(dirname "$here")/via-fetch-metric.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
export VIA_TEXTFILE_DIR="$tmp/textfile"
prom="$VIA_TEXTFILE_DIR/via_artifact_fetch.prom"

fails=0
check() {
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1 (expected '$3', got '$2')"; fails=$((fails + 1)); fi
}
value() { awk -v m="$1" '$1 == m { print $2 }' "$prom"; }

SERVICE_RESULT=success VIA_NOW=1000 "$script"
check "success flag" "$(value via_artifact_fetch_last_success)" 1
check "run timestamp" "$(value via_artifact_fetch_last_run_timestamp_seconds)" 1000
check "success timestamp" "$(value via_artifact_fetch_last_success_timestamp_seconds)" 1000

SERVICE_RESULT=exit-code VIA_NOW=2000 "$script"
check "failure flag" "$(value via_artifact_fetch_last_success)" 0
check "failure run timestamp" "$(value via_artifact_fetch_last_run_timestamp_seconds)" 2000
check "success timestamp kept" "$(value via_artifact_fetch_last_success_timestamp_seconds)" 1000

check "no temp files left" "$(find "$VIA_TEXTFILE_DIR" -name '.*' -type f | wc -l | tr -d ' ')" 0

rm -rf "$VIA_TEXTFILE_DIR"
SERVICE_RESULT=timeout VIA_NOW=3000 "$script"
check "first run failure has no success timestamp" "$(value via_artifact_fetch_last_success_timestamp_seconds)" 0

[ "$fails" -eq 0 ]
