#!/usr/bin/env bash
set -euo pipefail

readonly STATUS_URL="${VIA_VALHALLA_URL:-http://127.0.0.1:8002}/status"
readonly MAX_ATTEMPTS="${VIA_VALHALLA_HEALTH_ATTEMPTS:-60}"

for ((attempt = 1; attempt <= MAX_ATTEMPTS; attempt += 1)); do
  if status="$(curl --fail --silent "$STATUS_URL")"; then
    printf '%s\n' "$status"
    exit 0
  fi
  sleep 1
done

echo "Valhalla did not become ready after $MAX_ATTEMPTS attempts" >&2
exit 1
