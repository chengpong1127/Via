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

host_activates() { # what fetch.sh --incoming does after a successful activation
  cp "$OUT_DIR/incoming/$(cat "$OUT_DIR/incoming/READY")/.gtfs-sha" "$PIPELINE_DIR/.built-gtfs-sha"
}
export FAKE_GTFS_SHA=aaa
run
check "job does not claim the build before the host activated it" "$(test -e "$PIPELINE_DIR/.built-gtfs-sha" && echo written || echo absent)" "absent"
check "hand-over carries the GTFS checksum" "$(cat "$OUT_DIR/incoming/$(cat "$OUT_DIR/incoming/READY")/.gtfs-sha")" "aaa"
check "first run does a full fetch (no previous snapshot)" "$(grep -c '^fetch$' "$VIA_DATA_LOG")" "1"
check "first run builds" "$(calls build)" "1"
check "first run hands over one build" "$(incoming_ids)" "1"
check "first run marks READY" "$(ready)" "yes"
id="$(cat "$OUT_DIR/incoming/READY")"
check "handed-over artifact is the built file" "$(cat "$OUT_DIR/incoming/$id/.via-static.json")" '{"ok":"aaa"}'

run
check "build not activated by the host is rebuilt next night" "$(calls build)" "2"
host_activates
export FAKE_GTFS_SHA=bbb
run
check "later run fetches GTFS only" "$(calls 'fetch --gtfs-only')" "2"
check "changed GTFS builds again" "$(calls build)" "3"
check "changed GTFS marks READY" "$(ready)" "yes"
host_activates

rm -rf "$OUT_DIR/incoming"
snapshots_before="$(find "$PIPELINE_DIR/fetch" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')"
run
check "unchanged GTFS does not build" "$(calls build)" "3"
check "unchanged GTFS hands nothing over" "$(ready)" "no"
check "unchanged GTFS drops the duplicate snapshot" "$(find "$PIPELINE_DIR/fetch" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')" "$snapshots_before"
check "latest still points at a snapshot with the built checksum" "$(grep -o bbb "$PIPELINE_DIR/fetch/latest/manifest.json")" "bbb"

export FAKE_GTFS_SHA=ccc FAKE_BUILD_FAIL=1
snaps_before_fail="$(find "$PIPELINE_DIR/fetch" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')"
if run; then echo "FAIL build failure exits non-zero"; fails=$((fails + 1)); else echo "ok   build failure exits non-zero"; fi
check "build failure hands nothing over" "$(ready)" "no"
check "failed build still prunes snapshots to the budget" "$(find "$PIPELINE_DIR/fetch" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')" "$((snaps_before_fail + 1 > 2 ? 2 : snaps_before_fail + 1))"
unset FAKE_BUILD_FAIL
run
check "retry after failed build builds even though GTFS is already fetched" "$(calls build)" "5"
check "retry hands over" "$(ready)" "yes"

rm -rf "$OUT_DIR/incoming"
export FAKE_GTFS_SHA=ddd FAKE_FETCH_FAIL=1
if run; then echo "FAIL fetch failure exits non-zero"; fails=$((fails + 1)); else echo "ok   fetch failure exits non-zero"; fi
check "fetch failure does not build" "$(calls build)" "5"
unset FAKE_FETCH_FAIL

check "only fetch and build are ever invoked" "$(grep -vc '^\(fetch\|build\)' "$VIA_DATA_LOG" || true)" "0"

export FAKE_GTFS_SHA=eee VIA_KEEP_SNAPSHOTS=2 VIA_KEEP_BUILDS=1
run
check "old fetch snapshots pruned" "$(find "$PIPELINE_DIR/fetch" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')" "2"
check "old builds pruned" "$(find "$PIPELINE_DIR/build" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')" "1"

ln -s "$tmp" "$tmp/linkdest" 2>/dev/null || true
rm -rf "$OUT_DIR/incoming"; ln -s "$tmp" "$OUT_DIR/incoming"
export FAKE_GTFS_SHA=zzz
if run; then echo "FAIL symlinked incoming dir is refused"; fails=$((fails + 1)); else echo "ok   symlinked incoming dir is refused"; fi
rm -f "$OUT_DIR/incoming"

if command -v flock >/dev/null 2>&1; then
  export FAKE_GTFS_SHA=yyy
  exec 8>"$PIPELINE_DIR/.lock"; flock -n 8
  if run; then echo "FAIL concurrent job is refused"; fails=$((fails + 1)); else echo "ok   concurrent job is refused"; fi
  exec 8>&-
else
  echo "skip concurrent-job lock test (no flock on this host; covered on Linux)"
fi

echo "failures: $fails"
exit "$fails"
