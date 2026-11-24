#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
art="$(dirname "$here")"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

export VIA_DATA="$here/fake-via-data"
export VIA_DATA_LOG="$tmp/via-data.log"
export PIPELINE_DIR="$tmp/pipeline"
export OUT_DIR="$tmp/static"
mkdir -p "$PIPELINE_DIR" "$OUT_DIR"
: >"$VIA_DATA_LOG"

fails=0
check() {
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1 (expected '$3', got '$2')"; fails=$((fails + 1)); fi
}
run() { "$art/nightly-build.sh" >"$tmp/out.log" 2>&1; }
incoming_ids() { find "$OUT_DIR/incoming" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | wc -l | tr -d ' '; }
ready() { test -f "$OUT_DIR/incoming/READY" && echo yes || echo no; }
calls() { grep -c "^$1" "$VIA_DATA_LOG" || true; }

export FAKE_GTFS_SHA=aaa
run
check "first run does a full fetch (no previous snapshot)" "$(grep -c '^fetch$' "$VIA_DATA_LOG")" "1"
check "first run builds" "$(calls build)" "1"
check "first run hands over one build" "$(incoming_ids)" "1"
check "first run marks READY" "$(ready)" "yes"
id="$(cat "$OUT_DIR/incoming/READY")"
check "handed-over artifact is the built file" "$(cat "$OUT_DIR/incoming/$id/.via-static.json")" '{"ok":"aaa"}'

rm -f "$OUT_DIR/incoming/READY"
export FAKE_GTFS_SHA=bbb
run
check "later run fetches GTFS only" "$(calls 'fetch --gtfs-only')" "1"
check "changed GTFS builds again" "$(calls build)" "2"
check "changed GTFS marks READY" "$(ready)" "yes"

rm -rf "$OUT_DIR/incoming"
snapshots_before="$(find "$PIPELINE_DIR/fetch" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')"
run
check "unchanged GTFS does not build" "$(calls build)" "2"
check "unchanged GTFS hands nothing over" "$(ready)" "no"
check "unchanged GTFS drops the duplicate snapshot" "$(find "$PIPELINE_DIR/fetch" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')" "$snapshots_before"
check "latest still points at a snapshot with the built checksum" "$(grep -o bbb "$PIPELINE_DIR/fetch/latest/manifest.json")" "bbb"

export FAKE_GTFS_SHA=ccc FAKE_BUILD_FAIL=1
if run; then echo "FAIL build failure exits non-zero"; fails=$((fails + 1)); else echo "ok   build failure exits non-zero"; fi
check "build failure hands nothing over" "$(ready)" "no"
unset FAKE_BUILD_FAIL
run
check "retry after failed build builds even though GTFS is already fetched" "$(calls build)" "4"
check "retry hands over" "$(ready)" "yes"

rm -rf "$OUT_DIR/incoming"
export FAKE_GTFS_SHA=ddd FAKE_FETCH_FAIL=1
if run; then echo "FAIL fetch failure exits non-zero"; fails=$((fails + 1)); else echo "ok   fetch failure exits non-zero"; fi
check "fetch failure does not build" "$(calls build)" "4"
unset FAKE_FETCH_FAIL

check "only fetch and build are ever invoked" "$(grep -vc '^\(fetch\|build\)' "$VIA_DATA_LOG" || true)" "0"

export FAKE_GTFS_SHA=eee VIA_KEEP_SNAPSHOTS=2 VIA_KEEP_BUILDS=1
run
check "old fetch snapshots pruned" "$(find "$PIPELINE_DIR/fetch" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')" "2"
check "old builds pruned" "$(find "$PIPELINE_DIR/build" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')" "1"

echo "failures: $fails"
exit "$fails"
