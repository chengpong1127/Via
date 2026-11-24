#!/usr/bin/env bash
# deploy.sh must reject an unknown VIA_PROFILE before touching anything.
set -euo pipefail
d="$(cd "$(dirname "$0")" && pwd)"
set +e
out=$(VIA_PROFILE=typo "$d/deploy.sh" abcdef1 2>&1); rc=$?
set -e
[ "$rc" = 2 ] || { echo "FAIL: expected exit 2, got $rc: $out" >&2; exit 1; }
grep -q "VIA_PROFILE" <<<"$out" || { echo "FAIL: message missing: $out" >&2; exit 1; }
echo "deploy profile validation OK"
