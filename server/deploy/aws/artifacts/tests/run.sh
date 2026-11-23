#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
art="$(dirname "$here")"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

export AWS_BIN="$here/fake-aws"
export KUBECTL="$here/fake-kubectl"
export FAKE_S3="$tmp/s3"
export KUBECTL_LOG="$tmp/kubectl.log"
export KUBECTL_FAIL_FLAG="$tmp/fail-rollout"
export VIA_ARTIFACT_BUCKET=bkt
export VIA_ROOT="$tmp/via"
export VIA_VALIDATE_CMD="$here/validate-ok"
export VIA_LOCK_FILE="$tmp/lock"
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

cat >"$here/validate-ok" <<'V'
#!/usr/bin/env bash
[ ! -f "$1/.invalid" ] && grep -q '"ok"' "$1/.via-static.json"
V
chmod +x "$here/validate-ok"
trap 'rm -rf "$tmp"; rm -f "$here/validate-ok"' EXIT

mkbuild() {
  mkdir -p "$tmp/builds/$1"
  printf '{"ok":"%s"}' "$2" >"$tmp/builds/$1/.via-static.json"
}
current() { readlink "$VIA_ROOT/static/current" || echo none; }

mkbuild b1 one
mkbuild b2 two
"$art/publish.sh" "$tmp/builds/b1" >/dev/null 2>&1
check "publish writes pointer" "$(cat "$FAKE_S3/bkt/static/latest")" "b1"
"$art/publish.sh" "$tmp/builds/b1" >/dev/null 2>&1
check "republish identical is a no-op" "$(cat "$FAKE_S3/bkt/static/latest")" "b1"
printf '{"ok":"changed"}' >"$tmp/builds/b1/.via-static.json"
expect_fail "publish refuses changed content for existing id" "$art/publish.sh" "$tmp/builds/b1"
printf '{"ok":"one"}' >"$tmp/builds/b1/.via-static.json"

"$art/fetch.sh" --static >/dev/null 2>&1
check "good artifact activates" "$(current)" "builds/b1"
check "server restarted" "$(grep -c 'rollout restart deploy/server' "$KUBECTL_LOG")" "1"
"$art/fetch.sh" --static >/dev/null 2>&1
check "re-run is a no-op" "$(grep -c 'rollout restart deploy/server' "$KUBECTL_LOG")" "1"

"$art/publish.sh" "$tmp/builds/b2" >/dev/null 2>&1
printf '%s' "$(cat "$FAKE_S3/bkt/static/b2/via-static.json.zst")x" >"$FAKE_S3/bkt/static/b2/via-static.json.zst"
expect_fail "corrupt sha refuses" "$art/fetch.sh" --static
check "corrupt keeps previous current" "$(current)" "builds/b1"

"$art/publish.sh" "$tmp/builds/b2" >/dev/null 2>&1 || true
mkbuild b3 three
printf '{"bad":"x"}' >"$tmp/builds/b3/.via-static.json"
"$art/publish.sh" "$tmp/builds/b3" >/dev/null 2>&1
expect_fail "failed validation refuses" "$art/fetch.sh" --static
check "failed validation keeps previous current" "$(current)" "builds/b1"
"$art/fetch.sh" --static >/dev/null 2>&1
check "rejected build is skipped on re-run" "$(current)" "builds/b1"

mkbuild b4 four
"$art/publish.sh" "$tmp/builds/b4" >/dev/null 2>&1
touch "$KUBECTL_FAIL_FLAG"
expect_fail "readiness failure fails" "$art/fetch.sh" --static
check "readiness failure restores previous current" "$(current)" "builds/b1"
rm -f "$KUBECTL_FAIL_FLAG"

"$art/fetch.sh" --static b4 >/dev/null 2>&1 || true
rm -f "$VIA_ROOT/static/builds/b4.rejected"
"$art/fetch.sh" --static b4 >/dev/null 2>&1
check "pinned id activates (rollback target)" "$(current)" "builds/b4"
"$art/fetch.sh" --static b1 >/dev/null 2>&1
check "rollback to earlier build" "$(current)" "builds/b1"

tiles="$tmp/tiles"
mkdir -p "$tiles/valhalla_tiles/0"
echo '{}' >"$tiles/valhalla.json"
echo tile >"$tiles/valhalla_tiles/0/a.gph"
echo pbf >"$tiles/map.osm.pbf"
"$art/publish.sh" "$tmp/builds/b1" --tiles "$tiles" --tiles-id t1 >/dev/null 2>&1
mkdir -p "$VIA_ROOT/valhalla"
echo old >"$VIA_ROOT/valhalla/old.txt"
"$art/fetch.sh" --tiles >/dev/null 2>&1
check "tiles activate" "$(cat "$VIA_ROOT/valhalla/.tiles-id")" "t1"
check "tiles exclude pbf" "$(test -e "$VIA_ROOT/valhalla/map.osm.pbf" && echo present || echo absent)" "absent"
archived=0
for d in "$VIA_ROOT"/valhalla-builds/initial-*; do [ -d "$d" ] && archived=$((archived + 1)); done
check "previous tiles archived" "$archived" "1"
check "valhalla restarted" "$(grep -c 'rollout restart deploy/valhalla' "$KUBECTL_LOG")" "1"
"$art/fetch.sh" --tiles >/dev/null 2>&1
check "tiles re-run is a no-op" "$(grep -c 'rollout restart deploy/valhalla' "$KUBECTL_LOG")" "1"

echo "failures: $fails"
exit "$fails"
