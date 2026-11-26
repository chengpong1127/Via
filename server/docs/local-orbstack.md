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
    deploy/orbstack.sh lan             # Wi-Fi access for a phone (foreground, port 3001, NO AUTH: see Reaching it)
    deploy/orbstack.sh down            # delete deployments and services; keeps the namespace, Secret, data and tiles

`up` is also the update path: pull `main`, run `up`. It builds the image from the current checkout, writes the git-ignored `overlays/orbstack/volumes.yaml` (hostPath of the tiles and the chosen build), and restarts the pods. Without a build id it uses the build `data/build/.via-static-latest.json` points to. The server needs the build id directory to hold `.via-static.json`.

Only one TDX consumer may run: the request budget is per key (the free tier is 5 requests per minute), so do not run `via-data fetch` or a second server with the same credentials while testing.

## Reaching it

The overlay makes the `server` Service a LoadBalancer on port 3000. OrbStack publishes it on the Mac's `127.0.0.1` only, not on the Wi-Fi address (verified: the LAN IP refuses connections).

- Mac and iOS simulator: `http://127.0.0.1:3000`. The Debug build already defaults to this (`ios/Config/Debug.xcconfig`); nothing to change.
- Real iPhone on the same Wi-Fi: run `deploy/orbstack.sh lan` (a `kubectl port-forward` bound to the Mac's `en0` address, port 3001; set `VIA_LAN_IFACE` for another interface; keep it running) and use `http://<mac-lan-ip>:3001` (`ipconfig getifaddr en0`). Put it in the git-ignored `ios/Config/Local.xcconfig`:

      VIA_API_BASE_URL = http:/$()/192.168.1.23:3001

  The `/$()/` keeps xcconfig from reading `//` as a comment. Debug builds allow plain HTTP (`ViaServer.validatedURL`); Release builds require HTTPS and are not changed. App Transport Security does not apply to IP-address hosts, so no Info.plist exception is needed. iOS is expected to ask for Local Network permission (untested on a device; if no prompt appears, the app may need an `NSLocalNetworkUsageDescription` key). The Mac firewall must allow incoming connections to OrbStack.

**Security:** `lan` has no authentication and is reachable by every device on that network. Anyone there can run plans, spend the shared per-key TDX budget and see your queries. Use only a trusted network and press Ctrl-C when done (`pkill -f 'port-forward svc/server'` if it was backgrounded).

Check: `curl -s http://127.0.0.1:3000/health/ready`.

## Refresh data

The static artifact is built on the host (or in the image) against Valhalla, with the TDX budget in mind.

1. Download only the GTFS zip (one TDX call) into a new `server/data/fetch/<timestamp>/`, copy the previous snapshot's `bike-stations-*.json` and `bus-*.json`, and update `manifest.json`. Do not run a bare `via-data fetch`: it bursts past the free-tier limit of 5 requests per minute.
2. Port-forward Valhalla: `kubectl --context orbstack -n via port-forward svc/valhalla 8002:8002 --address 127.0.0.1` (the build container reaches it through `host.docker.internal`; verified). Do not bind it to 0.0.0.0: Valhalla has no authentication. Stop it after the build (`pkill -f 'port-forward svc/valhalla'`).
3. Build (about 30 minutes on this Mac), with the image so the code matches the running server:

       docker run --rm --entrypoint via-data \
         -e VIA_VALHALLA_URL=http://host.docker.internal:8002/ \
         -v "$PWD/data:/build/server/data" via-server:dev \
         build --input /build/server/data/fetch/<timestamp>

   The build wrote the artifact and repointed `data/build/.via-static-latest.json`, but the container then did not exit once. When that link has moved and `.via-static.json` ends in valid JSON (`jq -e . <file>`), run `docker stop` on the container.

   `bus_derived_segment_scale_percent` (default 290, `config/via.toml`) is applied at build, so rebuild after changing it.
4. `deploy/orbstack.sh data <new-build-id>`.

Never `rm -rf` anything under `server/data/build` or `server/data/fetch` by hand: builds are shared by all sessions, and the running pods mount the active one.

`via-data build` repoints `data/build/.via-static-latest.json` to the new build.

## Troubleshooting

- Pod `CreateContainerConfigError`: the `tdx-credentials` Secret is missing.
- Server not Ready: `kubectl --context orbstack -n via logs deploy/server --tail=50`; usually the build directory has no `.via-static.json` or Valhalla is not ready.
- Logs never contain request coordinates; do not add any.
