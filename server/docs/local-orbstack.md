# Local stack on OrbStack

Runs the whole backend (server x2, TDX gateway, Valhalla) on this Mac's OrbStack Kubernetes cluster, for hand testing from the iOS simulator or a phone. Local only: no AWS. For production see [deploy-aws.md](deploy-aws.md).

## One-time setup

1. OrbStack running with Kubernetes enabled (`kubectl --context orbstack get nodes`).
2. Valhalla tiles at `server/var/valhalla` (or set `VIA_TILES_DIR`) and a static build under `server/data/build/<build-id>/` (see "Refresh data").
3. The TDX credentials Secret. Never commit it or paste it in a chat. From `server/.env` without printing it:

       kubectl --context orbstack create namespace via --dry-run=client -o yaml | kubectl --context orbstack apply -f -
       kubectl --context orbstack -n via create secret generic tdx-credentials \
         --from-literal=TDX_CLIENT_ID="$TDX_CLIENT_ID" \
         --from-literal=TDX_CLIENT_SECRET="$TDX_CLIENT_SECRET" \
         --dry-run=client -o yaml | kubectl --context orbstack apply -f -

   Load the two variables from `.env` into the shell first (`set -a; . ./.env; set +a`); do not echo them.

## Start, update, stop

All from `server/`:

    deploy/orbstack.sh up              # docker build -t via-server:dev ., apply overlay, wait for pods
    deploy/orbstack.sh up <build-id>   # same, with a specific static build
    deploy/orbstack.sh status          # pods, service, /health/ready
    deploy/orbstack.sh data <build-id> # switch static build, restart server pods
    deploy/orbstack.sh lan             # Wi-Fi access for a phone (foreground, port 3001)
    deploy/orbstack.sh down            # delete deployments and services

`up` is also the update path: pull `main`, run `up`. It builds the image from the current checkout, writes the git-ignored `overlays/orbstack/volumes.yaml` (hostPath of the tiles and the chosen build), and restarts the pods. Without a build id it uses the build `data/build/.via-static-latest.json` points to. The server needs the build id directory to hold `.via-static.json`.

Only one TDX consumer may run: the 5 requests per minute budget is per key, so do not run `via-data fetch` or a second server with the same credentials while testing.

## Reaching it

The overlay makes the `server` Service a LoadBalancer on port 3000. OrbStack publishes it on the Mac's `127.0.0.1` only, not on the Wi-Fi address (verified: the LAN IP refuses connections).

- Mac and iOS simulator: `http://127.0.0.1:3000`. The Debug build already defaults to this (`ios/Config/Debug.xcconfig`); nothing to change.
- Real iPhone on the same Wi-Fi: run `deploy/orbstack.sh lan` (a `kubectl port-forward` on 0.0.0.0:3001; keep it running) and use `http://<mac-lan-ip>:3001` (`ipconfig getifaddr en0`). Put it in the git-ignored `ios/Config/Local.xcconfig`:

      VIA_API_BASE_URL = http:/$()/192.168.1.23:3001

  The `/$()/` keeps xcconfig from reading `//` as a comment. Debug builds allow plain HTTP (`ViaServer.validatedURL`); Release builds require HTTPS and are not changed. App Transport Security does not apply to IP-address hosts, so no Info.plist exception is needed. iOS asks once for Local Network permission; accept it. The Mac firewall must allow incoming connections to OrbStack.

Check: `curl -s http://127.0.0.1:3000/health/ready`.

## Refresh data

The static artifact is built on the host (or in the image) against Valhalla, with the TDX budget in mind.

1. Download only the GTFS zip (one TDX call) into a new `server/data/fetch/<timestamp>/`, copy the previous snapshot's `bike-stations-*.json` and `bus-*.json`, and update `manifest.json`. Do not run a bare `via-data fetch`: it bursts past 5 requests per minute.
2. Port-forward Valhalla: `kubectl --context orbstack -n via port-forward svc/valhalla 8002:8002 --address 0.0.0.0`.
3. Build (about 5 minutes), with the image so the code matches the running server:

       docker run --rm --entrypoint via-data \
         -e VIA_VALHALLA_URL=http://host.docker.internal:8002/ \
         -v "$PWD/data:/build/server/data" via-server:dev \
         build --input /build/server/data/fetch/<timestamp>

   `bus_derived_segment_scale_percent` (default 290, `config/via.toml`) is applied at build, so rebuild after changing it.
4. `deploy/orbstack.sh data <new-build-id>`.

`via-data build` repoints `data/build/.via-static-latest.json` to the new build.

## Troubleshooting

- Pod `CreateContainerConfigError`: the `tdx-credentials` Secret is missing.
- Server not Ready: `kubectl --context orbstack -n via logs deploy/server --tail=50`; usually the build directory has no `.via-static.json` or Valhalla is not ready.
- Logs never contain request coordinates; do not add any.
