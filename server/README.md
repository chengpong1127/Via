# Via server

The Rust server owns Via's provider-neutral street-routing boundary. Journey
Planning receives a pinned `StreetRoutingSession`; it never constructs a
Valhalla client or branches on a provider name.

Implemented capabilities:

- coordinate correlation;
- one-to-many and many-to-one Walking/YouBike matrices;
- detailed geometry and semantic maneuver facts;
- immutable graph-revision pinning and startup capability checks;
- normalized provider errors, strict timeout/concurrency/location/body bounds;
- structured OSM attribution; and
- Operational Events containing only provider category, revision, latency,
  outcome, and anonymous counts.

## Run locally

Start the pinned Valhalla 3.8.3 image and build the checked-in deterministic
fixture:

```sh
docker compose up -d valhalla
./scripts/wait-for-valhalla.sh
```

Record the `tileset_last_modified` integer from that status response. The server
requires it so a configured graph revision cannot silently name different tiles.

Then start Axum:

```sh
VIA_STREET_GRAPH_REVISION=valhalla-3.8.3-bike-fixture-d4cfdad98205 \
VIA_VALHALLA_TILESET_LAST_MODIFIED=<value-from-status> \
cargo run
```

The server fails startup until Valhalla is healthy. Its operator endpoint is
`GET /health/street-routing`; responses use `Cache-Control: no-store` and report
both the current and startup-pinned graph revision.

Optional configuration:

| Variable | Meaning |
| --- | --- |
| `VIA_VALHALLA_URL` | Valhalla base URL; defaults to `http://127.0.0.1:8002/`. |
| `PORT` | Axum port; defaults to `3000`. |

## Tests

```sh
cargo test --test provider_contract
cargo test --test valhalla_contract
cargo test
```

The Valhalla contract tests use an isolated local HTTP fixture and do not need
Docker. The deterministic fake is the default seam for Journey Planning tests.

## Graph publication

Graph builds are offline publication work. The fixture checksum, Valhalla image,
and Via graph revision are pinned. A mounted volume with a different revision is
rejected rather than rebuilt in place. Publish a new graph to a new volume,
validate `/status`, then direct new Planning Requests to the new provider
instance. Existing `StreetRoutingSession` values remain pinned to the old
revision. Rollback selects the prior healthy volume.

The checked-in fixture is intentionally tiny. Taipei–New Taipei production use
still requires a separately pinned OSM extract, the accepted route-quality
corpus, attribution/legal review, and the p50/p95/p99 adoption benchmark from
issue #13.
