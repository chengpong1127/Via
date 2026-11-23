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

s3_get() {
  "$AWS_BIN" s3 cp "$(s3_uri "$1")" "$2" --only-show-errors
}

s3_put() {
  "$AWS_BIN" s3 cp "$1" "$(s3_uri "$2")" --only-show-errors
}

s3_get_text() {
  local key="$1" out err
  err="$(mktemp)"
  if out="$(s3_get "$key" - 2>"$err")"; then
    rm -f "$err"
    printf '%s' "$out"
    return 0
  fi
  if grep -qE '404|Not Found|does not exist|NoSuchKey' "$err"; then
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
