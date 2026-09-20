#!/usr/bin/env bash
set -euo pipefail

: "${VALHALLA_OSM_PBF:?VALHALLA_OSM_PBF must be set}"

readonly DATA_DIR="${VALHALLA_DATA_DIR:-/data/valhalla}"
readonly OSM_PBF_PATH="$VALHALLA_OSM_PBF"
readonly OSM_PBF_URL="${VALHALLA_OSM_PBF_URL:-}"
readonly EXPECTED_SHA256="${VALHALLA_OSM_PBF_SHA256:-}"
readonly EXPECTED_TILESET_LAST_MODIFIED="${VALHALLA_TILESET_LAST_MODIFIED:-}"
readonly VALHALLA_CONFIG_PATH="$DATA_DIR/valhalla.json"
readonly REVISION_FILE="$DATA_DIR/graph-revision"

if [[ ! -s "$OSM_PBF_PATH" ]]; then
  if [[ -z "$OSM_PBF_URL" ]]; then
    echo "Valhalla graph input does not exist and VALHALLA_OSM_PBF_URL is not set" >&2
    exit 1
  fi

  mkdir -p "$(dirname "$OSM_PBF_PATH")"
  echo "Downloading Valhalla graph input from $OSM_PBF_URL"
  wget --output-document "$OSM_PBF_PATH.part" "$OSM_PBF_URL"
  mv "$OSM_PBF_PATH.part" "$OSM_PBF_PATH"
else
  echo "Using existing Valhalla graph input: $OSM_PBF_PATH"
fi

actual_sha256="$(sha256sum "$OSM_PBF_PATH" | awk '{print $1}')"
if [[ -n "$EXPECTED_SHA256" ]] && [[ "$actual_sha256" != "$EXPECTED_SHA256" ]]; then
  echo "Valhalla graph input checksum does not match the pinned revision" >&2
  exit 1
fi

readonly SHA256_PREFIX="${actual_sha256:0:12}"
if [[ -n "${VALHALLA_GRAPH_REVISION:-}" ]]; then
  if [[ "$VALHALLA_GRAPH_REVISION" != *"$SHA256_PREFIX"* ]]; then
    echo "VALHALLA_GRAPH_REVISION must contain the graph input checksum prefix" >&2
    exit 1
  fi
  readonly GRAPH_REVISION="$VALHALLA_GRAPH_REVISION"
else
  readonly GRAPH_REVISION="valhalla-3.8.3-osm-$SHA256_PREFIX"
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
    > "$VALHALLA_CONFIG_PATH"
  valhalla_build_tiles -c "$VALHALLA_CONFIG_PATH" "$OSM_PBF_PATH"
  valhalla_build_extract -c "$VALHALLA_CONFIG_PATH" -v
  if [[ -n "$EXPECTED_TILESET_LAST_MODIFIED" ]]; then
    if ! [[ "$EXPECTED_TILESET_LAST_MODIFIED" =~ ^[0-9]+$ ]]; then
      echo "VALHALLA_TILESET_LAST_MODIFIED must be a Unix timestamp" >&2
      exit 1
    fi
    touch -d "@$EXPECTED_TILESET_LAST_MODIFIED" "$DATA_DIR/tiles.tar"
  fi
  printf '%s\n' "$GRAPH_REVISION" > "$REVISION_FILE"
elif [[ -n "$EXPECTED_TILESET_LAST_MODIFIED" ]]; then
  if ! [[ "$EXPECTED_TILESET_LAST_MODIFIED" =~ ^[0-9]+$ ]]; then
    echo "VALHALLA_TILESET_LAST_MODIFIED must be a Unix timestamp" >&2
    exit 1
  fi
  actual_tileset_last_modified="$(stat -c '%Y' "$DATA_DIR/tiles.tar")"
  if [[ "$actual_tileset_last_modified" != "$EXPECTED_TILESET_LAST_MODIFIED" ]]; then
    echo "The mounted tileset does not match the pinned last-modified timestamp" >&2
    exit 1
  fi
fi

exec valhalla_service "$VALHALLA_CONFIG_PATH" 1
