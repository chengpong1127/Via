# shellcheck shell=bash
AWS_BIN="${AWS_BIN:-aws}"

log() {
  echo "$(date -u +%H:%M:%S) $*" >&2
}

die() {
  log "ERROR: $*"
  exit 1
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

valid_id() {
  [[ "$1" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$ ]]
}

require_bucket() {
  [ -n "${VIA_ARTIFACT_BUCKET:-}" ] || die "VIA_ARTIFACT_BUCKET is not set"
}

s3_uri() {
  echo "s3://${VIA_ARTIFACT_BUCKET}/$1"
}

NOT_FOUND_RE='An error occurred \((404|NoSuchKey|NotFound)\)'
EXISTS_RE='An error occurred \((PreconditionFailed|412)\)'

s3_get() {
  "$AWS_BIN" s3 cp "$(s3_uri "$1")" "$2" --only-show-errors
}

s3_put() {
  "$AWS_BIN" s3 cp "$1" "$(s3_uri "$2")" --only-show-errors
}

# s3_put_new <file> <key>: exit 0 created, 4 already exists, 1 error
s3_put_new() {
  local err rc=0
  err="$(mktemp)"
  "$AWS_BIN" s3api put-object --bucket "$VIA_ARTIFACT_BUCKET" --key "$2" --body "$1" --if-none-match '*' >/dev/null 2>"$err" || rc=$?
  if [ "$rc" -eq 0 ]; then
    rm -f "$err"
    return 0
  fi
  if grep -qE "$EXISTS_RE" "$err"; then
    rm -f "$err"
    return 4
  fi
  cat "$err" >&2
  rm -f "$err"
  return 1
}

# s3_exists <key>: exit 0 exists, 3 missing, 1 error
s3_exists() {
  local err rc=0
  err="$(mktemp)"
  "$AWS_BIN" s3api head-object --bucket "$VIA_ARTIFACT_BUCKET" --key "$1" >/dev/null 2>"$err" || rc=$?
  if [ "$rc" -eq 0 ]; then
    rm -f "$err"
    return 0
  fi
  if grep -qE "$NOT_FOUND_RE" "$err"; then
    rm -f "$err"
    return 3
  fi
  cat "$err" >&2
  rm -f "$err"
  return 1
}

# s3_get_text <key>: prints content; exit 3 when missing, 1 on any other error
s3_get_text() {
  local key="$1" out err
  err="$(mktemp)"
  if out="$(s3_get "$key" - 2>"$err")"; then
    rm -f "$err"
    printf '%s' "$out"
    return 0
  fi
  if grep -qE "$NOT_FOUND_RE" "$err"; then
    rm -f "$err"
    return 3
  fi
  cat "$err" >&2
  rm -f "$err"
  return 1
}

manifest_value() {
  awk -v k="$2" '$1 == k { print $2 }' <<<"$1"
}

# tree_hash <dir>: hash of "<sha256>  <path>" lines for every file except *.osm.pbf and bookkeeping markers
tree_hash() {
  local listing
  listing="$(mktemp)"
  (
    cd "$1" || exit 1
    find . -type f ! -name '*.osm.pbf' ! -name '.via-verified' ! -name '.tiles-id' | LC_ALL=C sort | while IFS= read -r f; do
      printf '%s  %s\n' "$(sha256 "$f")" "$f"
    done
  ) >"$listing"
  sha256 "$listing"
  rm -f "$listing"
}
