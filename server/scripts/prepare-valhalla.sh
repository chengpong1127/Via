#!/usr/bin/env bash
set -euo pipefail

readonly DATA_DIR=/data/valhalla
readonly FIXTURE=/fixtures/bike.osm.pbf
readonly FIXTURE_SHA256=d4cfdad98205cb8d60b0ed542010c152377748179d2958ee9afe4c1373d6d1c4
readonly GRAPH_REVISION=valhalla-3.8.3-bike-fixture-d4cfdad98205
readonly CONFIG="$DATA_DIR/valhalla.json"
readonly REVISION_FILE="$DATA_DIR/graph-revision"

actual_sha256="$(sha256sum "$FIXTURE" | cut -d ' ' -f 1)"
if [[ "$actual_sha256" != "$FIXTURE_SHA256" ]]; then
  echo "Fixture checksum does not match the pinned graph input" >&2
  exit 1
fi

mkdir -p "$DATA_DIR/tiles"

if [[ -f "$REVISION_FILE" ]] && [[ "$(<"$REVISION_FILE")" != "$GRAPH_REVISION" ]]; then
  echo "The mounted graph volume belongs to a different immutable revision" >&2
  exit 1
fi

if [[ ! -f "$DATA_DIR/tiles.tar" ]]; then
  valhalla_build_config \
    --mjolnir-tile-dir "$DATA_DIR/tiles" \
    --mjolnir-tile-extract "$DATA_DIR/tiles.tar" \
    > "$CONFIG"
  valhalla_build_tiles -c "$CONFIG" "$FIXTURE"
  valhalla_build_extract -c "$CONFIG" -v
  printf '%s\n' "$GRAPH_REVISION" > "$REVISION_FILE"
fi

exec valhalla_service "$CONFIG" 1
