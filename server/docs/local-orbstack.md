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

The overlay runs one `server` and one `valhalla` replica (`replicas:` in `overlays/orbstack/kustomization.yaml`; the base and the aws overlay keep 2). Valhalla pods restart often on a loaded Mac, so after any scale change or restart check the pod and, during a data build, the port-forward (`curl -s http://127.0.0.1:8002/status`). While a pod restarts, requests fail until it is Ready again.

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

## Auto-deploy

A GitHub Actions self-hosted runner on this Mac deploys `main` to the stack after the `Rust server` CI is green. Workflow: `.github/workflows/deploy-orbstack.yml`; script: `deploy/orbstack-deploy.sh`. Nothing happens until you register the runner (below). Auto-deploy never builds data: it only reads `VIA_DATA_DIR` and `VIA_TILES_DIR` and runs `orbstack.sh up` for the commit, which uses the build `data/build/.via-static-latest.json` points to at that moment. The weekly data refresh is separate.

### What it does

1. `workflow_run` fires when `Rust server` completes on `main`. The job runs only if the conclusion is `success`, the triggering event is `push` and the run is from this repository. (The event check matters: a fork pull request can have a branch named `main`.) `workflow_dispatch` is also allowed, on `main` only; the script then asks the GitHub API for a green `Rust server` push run for that commit.
2. It checks out exactly that commit in the runner workspace (`~/actions-runner-via/_work/Via/Via`, the dedicated directory) and the script refuses to go on unless `HEAD` is that commit, the commit is on `origin/main`, and `VIA_DATA_DIR` / `VIA_TILES_DIR` are outside the checkout (a checkout clean must never reach shared data). It skips a commit older than the one already deployed.
3. It runs `orbstack.sh up`, then polls `http://127.0.0.1:3000/health/ready` (up to 120 s, `VIA_READY_TIMEOUT`), then records the commit on the `server` Deployment (annotation `via.local/deployed-sha`).
4. If the build, apply, rollout or readiness check fails, the job fails (red run, pod list and server log tail in the log, outcome in the run summary) after rolling every Deployment back to its previous revision and restoring the previous image (`via-server:previous` is tagged before each deploy). The old pods keep serving during a failed rollout, so a failure usually means no downtime.
5. One deploy at a time (`concurrency` group, running deploys are never cancelled), a 60 minute timeout, `permissions: contents: read`, no `secrets.*`. The TDX Secret exists only in the cluster.

A change to `deploy-orbstack.yml` alone does not run `Rust server`, so deploy it with Actions > Deploy to OrbStack > Run workflow.

### Pause switch

    touch ~/.via-no-deploy     # pause: the job exits successfully, log line "paused: ... not deploying"
    rm ~/.via-no-deploy        # resume

Use it while you test by hand and during a data build. It only stops deploys that have not started; a running deploy finishes. Set `VIA_NO_DEPLOY_FILE` in the runner `.env` to use another path.

### Register the runner

Do this on the repo (Settings > Actions > Runners > New self-hosted runner, macOS, ARM64). The page shows the download commands and a one-time registration token; do not paste the token anywhere else.

    mkdir ~/actions-runner-via && cd ~/actions-runner-via
    # download and extract as shown on the GitHub page, then:
    ./config.sh --url https://github.com/chengpong1127/Via --token <token from the page> --labels via-orbstack --name via-orbstack
    printf 'VIA_DATA_DIR=%s\nVIA_TILES_DIR=%s\n' "$HOME/Documents/Via/server/data" "$HOME/Documents/Via/server/var/valhalla" > .env
    echo "$PATH" > .path          # run in a shell where kubectl, docker, jq, curl and git work
    ./svc.sh install && ./svc.sh start

Use the real absolute paths of your shared data and tiles directories. `svc.sh` installs a launchd agent for your user, so it runs while you are logged in, with OrbStack running. The runner needs `kubectl`, `docker`, `jq`, `curl` and `git` in `.path`.

### Public repo risks and settings

The repo is public and the runner executes code as your user, with Docker and cluster-admin `kubectl` (so it can read the TDX Secret and your files). Treat this as remote code execution on your Mac:

- The workflow never triggers on `pull_request`, `pull_request_target`, `issue_comment` or anything else a fork can start; `server/deploy/tests/deploy-workflow-test.sh` fails CI if that changes. Never add another workflow with `runs-on: [self-hosted, ...]`.
- Whoever can push to `main` or run workflows can run code on this Mac, so keep write access to people you trust and protect `main` (require a pull request).
- Settings > Actions > General: "Fork pull request workflows from outside collaborators" = Require approval for all outside collaborators; Workflow permissions = Read repository contents. If the repo belongs to an organization, restrict the runner group to this repository (Settings > Actions > Runner groups). A repo-level runner is already limited to this repo.
- Do not put runner or repo secrets in the workflow or `.env`; the cluster Secret is enough.

### Roll back

A failed deploy rolls back by itself. To go back after a deploy that passed, revert the commit on `main` (it deploys like any other), or by hand, from a checkout of the good commit: `server/deploy/orbstack.sh up`. While you are at it, pause deploys first. Quick restore of the last image: `docker tag via-server:previous via-server:dev && kubectl --context orbstack -n via rollout undo deploy/server deploy/tdx-gateway`.

### Confirm the first deploy

After registering: the runner shows Idle with labels `self-hosted, macOS, ARM64, via-orbstack`. With deploys paused (`touch ~/.via-no-deploy`), run the workflow (Actions > Deploy to OrbStack > Run workflow, branch `main`): it should succeed and log `paused`. Then `rm ~/.via-no-deploy`, run it again and watch: build, rollout, `deployed <sha>`. Check `kubectl --context orbstack -n via get deploy server -o jsonpath='{.metadata.annotations.via\.local/deployed-sha}'` equals `git rev-parse origin/main`.

### Remove it

    cd ~/actions-runner-via && ./svc.sh stop && ./svc.sh uninstall
    ./config.sh remove --token <removal token from Settings > Actions > Runners>
    cd ~ && rm -rf ~/actions-runner-via ~/.via-no-deploy

Then delete `.github/workflows/deploy-orbstack.yml` (and `orbstack-deploy-checks.yml`) in a PR. This removes only the runner directory; shared data and tiles are not under it. The stack keeps running.
