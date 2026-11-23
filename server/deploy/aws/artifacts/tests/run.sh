#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
art="$(dirname "$here")"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

export AWS_BIN="$here/fake-aws"
export KUBECTL="$here/fake-kubectl"
export CTR="$here/fake-ctr"
export FAKE_S3="$tmp/s3"
export KUBECTL_LOG="$tmp/kubectl.log"
export KUBECTL_FAIL_FLAG="$tmp/fail-rollout"
export VIA_ARTIFACT_BUCKET=bkt
export VIA_ROOT="$tmp/via"
cat >"$tmp/validate-ok" <<'V'
#!/usr/bin/env bash
[ ! -f "$1/.invalid" ] && grep -q '"ok"' "$1/.via-static.json"
V
chmod +x "$tmp/validate-ok"
export VIA_VALIDATE_CMD="$tmp/validate-ok"
export VIA_LOCK_FILE="$tmp/lock"
export VIA_MAX_ATTEMPTS=1
export KUBECONFIG=/dev/null
: >"$KUBECTL_LOG"

fails=0
check() {
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1 (expected '$3', got '$2')"; fails=$((fails + 1)); fi
}
expect_fail() {
  local name="$1"
  shift
  if "$@" >/dev/null 2>&1; then echo "FAIL $name (succeeded)"; fails=$((fails + 1)); else echo "ok   $name"; fi
}
expect_ok() {
  local name="$1"
  shift
  if "$@" >/dev/null 2>&1; then echo "ok   $name"; else echo "FAIL $name (failed)"; fails=$((fails + 1)); fi
}

mkbuild() {
  mkdir -p "$tmp/builds/$1"
  printf '{"ok":"%s"}' "$2" >"$tmp/builds/$1/.via-static.json"
}
current() { readlink "$VIA_ROOT/static/current" || echo none; }
count_log() { grep -c "$1" "$KUBECTL_LOG" || true; }
publish() { "$art/publish.sh" "$@" >/dev/null 2>&1; }
fetch() { "$art/fetch.sh" "$@" >/dev/null 2>&1; }

mkbuild b1 one
mkbuild b2 two
publish "$tmp/builds/b1"
check "publish writes pointer" "$(cat "$FAKE_S3/bkt/static/latest")" "b1"
publish "$tmp/builds/b1"
check "republish identical is a no-op" "$(cat "$FAKE_S3/bkt/static/latest")" "b1"
printf '{"ok":"changed"}' >"$tmp/builds/b1/.via-static.json"
expect_fail "publish refuses changed content for existing id" "$art/publish.sh" "$tmp/builds/b1"
printf '{"ok":"one"}' >"$tmp/builds/b1/.via-static.json"

FAKE_S3_FORBIDDEN=1 expect_fail "403 on publish is an error, not 'missing'" "$art/publish.sh" "$tmp/builds/b2"
check "403 did not write anything" "$(test -e "$FAKE_S3/bkt/static/b2" && echo written || echo clean)" "clean"
FAKE_S3_FORBIDDEN=1 expect_fail "403 on fetch is an error" "$art/fetch.sh" --static

rm -rf "$FAKE_S3/bkt/static/b1/via-static.json.zst"
publish "$tmp/builds/b1"
check "missing payload under an existing manifest is re-uploaded" "$(test -f "$FAKE_S3/bkt/static/b1/via-static.json.zst" && echo yes || echo no)" "yes"

mkdir -p "$VIA_ROOT/static"
ln -s junk "$VIA_ROOT/static/current.new"
fetch --static
check "good artifact activates (stale current.new tolerated)" "$(current)" "builds/b1"
check "server restarted" "$(count_log 'rollout restart deploy/server')" "1"
fetch --static
check "re-run is a no-op" "$(count_log 'rollout restart deploy/server')" "1"

publish "$tmp/builds/b2"
cp "$FAKE_S3/bkt/static/b2/via-static.json.zst" "$tmp/b2.zst"
printf 'x' >>"$FAKE_S3/bkt/static/b2/via-static.json.zst"
expect_fail "corrupt sha refuses" "$art/fetch.sh" --static
check "corrupt keeps previous current" "$(current)" "builds/b1"
cp "$tmp/b2.zst" "$FAKE_S3/bkt/static/b2/via-static.json.zst"

mkbuild b3 three
printf '{"bad":"x"}' >"$tmp/builds/b3/.via-static.json"
publish "$tmp/builds/b3"
expect_fail "failed validation refuses" "$art/fetch.sh" --static
check "failed validation keeps previous current" "$(current)" "builds/b1"
expect_fail "rejected latest keeps failing (not silent)" "$art/fetch.sh" --static
VIA_MAX_ATTEMPTS=3 expect_fail "retry count allows another attempt (still fails)" "$art/fetch.sh" --static
check "attempts recorded" "$(cat "$VIA_ROOT/static/builds/b3.rejected")" "2"

mkbuild b4 four
publish "$tmp/builds/b4"
echo server >"$KUBECTL_FAIL_FLAG"
expect_fail "readiness failure fails" "$art/fetch.sh" --static
check "readiness failure restores previous current" "$(current)" "builds/b1"
rm -f "$KUBECTL_FAIL_FLAG" "$VIA_ROOT/static/builds/b4.rejected"
fetch --static b4
check "pinned id activates" "$(current)" "builds/b4"
fetch --static b1
check "rollback to earlier build" "$(current)" "builds/b1"

rm -rf "$VIA_ROOT"
mkbuild f1 first
publish "$tmp/builds/f1"
echo server >"$KUBECTL_FAIL_FLAG"
expect_fail "first load readiness failure fails" "$art/fetch.sh" --static
check "first load failure leaves no current" "$(current)" "none"
expect_fail "first load failure keeps failing on re-run" "$art/fetch.sh" --static
rm -f "$KUBECTL_FAIL_FLAG"

rm -rf "$VIA_ROOT"
mkdir -p "$tmp/slow"
cat >"$tmp/slow/validate" <<'S'
#!/usr/bin/env bash
sleep 3
grep -q ok "$1/.via-static.json"
S
chmod +x "$tmp/slow/validate"
VIA_VALIDATE_CMD="$tmp/slow/validate" "$art/fetch.sh" --static >/dev/null 2>&1 &
slow_pid=$!
sleep 1
expect_fail "concurrent run is refused" "$art/fetch.sh" --static
wait "$slow_pid"
check "first run finished" "$(current)" "builds/f1"

rm -rf "$VIA_ROOT"
env -u VIA_VALIDATE_CMD PATH="$PATH" CTR="$here/fake-ctr" "$art/fetch.sh" --static >/dev/null 2>&1
check "validation through the server image (ctr)" "$(count_log 'ctr run')" "1"
check "image validation activates" "$(current)" "builds/f1"

rm -rf "$VIA_ROOT"
cat >"$tmp/noimage" <<'K'
#!/usr/bin/env bash
if [[ "$*" == *jsonpath* ]]; then exit 1; fi
exit 0
K
chmod +x "$tmp/noimage"
expect_fail "unvalidated activation is refused by default" env -u VIA_VALIDATE_CMD KUBECTL="$tmp/noimage" "$art/fetch.sh" --static
check "unvalidated refusal activates nothing" "$(current)" "none"
rm -f "$VIA_ROOT/static/builds/f1.rejected"
expect_ok "VIA_ALLOW_UNVALIDATED=1 permits it" env -u VIA_VALIDATE_CMD KUBECTL="$tmp/noimage" VIA_ALLOW_UNVALIDATED=1 "$art/fetch.sh" --static

rm -rf "$VIA_ROOT"
tiles="$tmp/tiles"
mkdir -p "$tiles/valhalla_tiles/0"
echo '{}' >"$tiles/valhalla.json"
echo tile >"$tiles/valhalla_tiles/0/a.gph"
echo pbf >"$tiles/map.osm.pbf"
publish "$tmp/builds/f1" --tiles "$tiles"
tid="$(cat "$FAKE_S3/bkt/valhalla/latest")"
publish "$tmp/builds/f1" --tiles "$tiles"
check "tiles id derives from content (idempotent)" "$(cat "$FAKE_S3/bkt/valhalla/latest")" "$tid"
mkdir -p "$VIA_ROOT/valhalla"
echo old >"$VIA_ROOT/valhalla/old.txt"
fetch --tiles
check "tiles activate" "$(cat "$VIA_ROOT/valhalla/.tiles-id")" "$tid"
check "tiles exclude pbf" "$(test -e "$VIA_ROOT/valhalla/map.osm.pbf" && echo present || echo absent)" "absent"
archived=0
for d in "$VIA_ROOT"/valhalla-builds/initial-*; do [ -d "$d" ] && archived=$((archived + 1)); done
check "previous tiles archived" "$archived" "1"
check "valhalla restarted" "$(count_log 'rollout restart deploy/valhalla')" "1"
fetch --tiles
check "tiles re-run is a no-op" "$(count_log 'rollout restart deploy/valhalla')" "1"

echo '{"x":1}' >"$tiles/valhalla_tiles/0/b.gph"
publish "$tmp/builds/f1" --tiles "$tiles"
tid2="$(cat "$FAKE_S3/bkt/valhalla/latest")"
echo valhalla >"$KUBECTL_FAIL_FLAG"
expect_fail "tiles readiness failure fails" "$art/fetch.sh" --tiles
check "tiles readiness failure restores previous tiles" "$(cat "$VIA_ROOT/valhalla/.tiles-id")" "$tid"
check "failed tiles marked rejected" "$(test -f "$VIA_ROOT/valhalla-builds/$tid2.rejected" && echo yes || echo no)" "yes"
rm -f "$KUBECTL_FAIL_FLAG"

rm -rf "$VIA_ROOT"
mkbuild g1 good
publish "$tmp/builds/g1" --tiles "$tiles"
echo valhalla >"$KUBECTL_FAIL_FLAG"
expect_fail "failing tiles do not block static (combined exit is non-zero)" "$art/fetch.sh"
check "static still activated when tiles fail" "$(current)" "builds/g1"
rm -f "$KUBECTL_FAIL_FLAG"

echo "failures: $fails"
exit "$fails"
