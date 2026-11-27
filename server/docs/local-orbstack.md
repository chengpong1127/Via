# Local stack on OrbStack

Runs the whole backend (server, TDX gateway, Valhalla) and the weekly data refresh on this Mac's OrbStack Kubernetes cluster, for hand testing from the iOS simulator or a phone. Local only: no AWS. For production see [deploy-aws.md](deploy-aws.md).

## One-time setup

1. OrbStack running with Kubernetes enabled (`kubectl --context orbstack get nodes`).
2. Valhalla tiles at `server/var/valhalla` (or set `VIA_TILES_DIR`) and a first static build under `server/data/build/<build-id>/` (see "First build"). After that the weekly job keeps the build fresh.
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
    deploy/orbstack.sh refresh         # run the weekly data refresh now and follow its log (see Weekly refresh)
    deploy/orbstack.sh lan             # Wi-Fi access for a phone (foreground, port 3001, NO AUTH: see Reaching it)
    deploy/orbstack.sh down            # delete deployments and services; keeps the namespace, Secret, data and tiles

`up` is also the update path: pull `main`, run `up`. It builds the `via-server:dev` and `via-refresh:dev` images from the current checkout, writes the git-ignored `overlays/orbstack/volumes.yaml` (hostPath of the tiles, the chosen build and the data directory), and restarts the pods. Without a build id it uses the build `data/build/.via-static-latest.json` points to. The server needs the build id directory to hold `.via-static.json`.

The TDX request budget is per key and shared by every process that uses it (the key allows 5 requests per second; see the TDX member center for the plan): the server, the gateway, the weekly job and any `via-data fetch` or research run all draw from it.

The overlay runs one `server` and one `valhalla` replica (`replicas:` in `overlays/orbstack/kustomization.yaml`; the base and the aws overlay keep 2). Valhalla pods restart often on a loaded Mac, so after any scale change or restart check the pod and, during a data build, the port-forward (`curl -s http://127.0.0.1:8002/status`). While a pod restarts, requests fail until it is Ready again.

## Reaching it

The overlay makes the `server` Service a LoadBalancer on port 3000. OrbStack publishes it on the Mac's `127.0.0.1` only, not on the Wi-Fi address (verified: the LAN IP refuses connections).

- Mac and iOS simulator: `http://127.0.0.1:3000`. The Debug build already defaults to this (`ios/Config/Debug.xcconfig`); nothing to change.
- Real iPhone on the same Wi-Fi: run `deploy/orbstack.sh lan` (a `kubectl port-forward` bound to the Mac's `en0` address, port 3001; set `VIA_LAN_IFACE` for another interface; keep it running) and use `http://<mac-lan-ip>:3001` (`ipconfig getifaddr en0`). Put it in the git-ignored `ios/Config/Local.xcconfig`:

      VIA_API_BASE_URL = http:/$()/192.168.1.23:3001

  The `/$()/` keeps xcconfig from reading `//` as a comment. Debug builds allow plain HTTP (`ViaServer.validatedURL`); Release builds require HTTPS and are not changed. App Transport Security does not apply to IP-address hosts, so no Info.plist exception is needed. iOS is expected to ask for Local Network permission (untested on a device; if no prompt appears, the app may need an `NSLocalNetworkUsageDescription` key). The Mac firewall must allow incoming connections to OrbStack.

**Security:** `lan` has no authentication and is reachable by every device on that network. Anyone there can run plans, spend the shared per-key TDX budget and see your queries. Use only a trusted network and press Ctrl-C when done (`pkill -f 'port-forward svc/server'` if it was backgrounded).

Check: `curl -s http://127.0.0.1:3000/health/ready`.

## Weekly refresh

A CronJob, `weekly-refresh` (namespace `via`), refreshes all static data once a week: Sunday 23:00 Asia/Taipei. One job does everything, staged in `overlays/orbstack/refresh-static.sh`:

1. **Fetch**: one `via-data fetch` downloads the GTFS zip and every TDX file the build needs (Bike stations, Bus routes, Bus schedules), paced at `VIA_FETCH_REQUESTS_PER_MINUTE` (60, so 1 request per second, a fifth of the key's 5 per second, which leaves room for the live server). The job fails here if TDX gave a transient error and `via-data` fell back to an older copy of a file (`reused_sources` in the snapshot manifest): a partial refresh is never built.
2. **Build**: `via-data build` against Valhalla through the in-cluster Service (`http://valhalla:8002/`; no port-forward). About 30 minutes, and it loads Valhalla for that time, so plans from the server are slower meanwhile.
3. **Validate**: `via-data validate` loads the new artifact.
4. **Activate**: the job patches the `static` hostPath of `deploy/server` to the new build directory. The one-replica server rolls over with a surge pod, so the old pod keeps serving until the new one passes `/health/ready`. If the new pod is not Ready within `ROLLOUT_TIMEOUT` (600 s), the job runs `kubectl rollout undo` and fails. A rolling replacement, not an in-place reload, is the activation because the server reads its artifact only at start and the rollout gives readiness gating and rollback for free.

If any step fails, the Job exits non-zero (its pod and log are kept; three failed Jobs are retained), the server stays on the build it already uses, and `data/build/.via-static-latest.json` (moved by `via-data build`) is put back where it was. The job never deletes anything under `server/data`: new builds and snapshots accumulate (about 0.5 GB each per week); prune by hand only builds that no pod mounts and no research run uses.

All settings are in `overlays/orbstack/refresh.yaml`: `schedule` and `timeZone` (the zone is explicit; the cluster rejects an unknown one, and a Taipei-time test CronJob was verified to fire at the Taipei minute), `VIA_FETCH_REQUESTS_PER_MINUTE`, `ROLLOUT_TIMEOUT`, and `startingDeadlineSeconds` (a run missed while the Mac sleeps still starts if the Mac wakes within 6 hours). The credentials come from the existing `tdx-credentials` Secret; the script never prints them. The job's ServiceAccount may only get, list, watch and patch `deploy/server` and read ReplicaSets.

Run it by hand, from `server/`:

    deploy/orbstack.sh refresh

or `kubectl --context orbstack -n via create job --from=cronjob/weekly-refresh my-refresh`. `orbstack.sh up` installs or updates the CronJob (and builds the `via-refresh:dev` image: `via-server:dev` plus `kubectl`). `kubectl --context orbstack -n via get cronjob,jobs` shows the schedule and past runs; `kubectl --context orbstack -n via logs job/<name>` the log, ending with a `tdx usage:` block. Use `kubectl ... patch cronjob weekly-refresh -p '{"spec":{"suspend":true}}'` to pause it.

**TDX budget of one run**: about 50 calls and 0.5 GB. The snapshot of 2026-11-25 had 41 files (GTFS 468 MB, 12 Bike catalogs, 22 Bus route files, 6 Bus schedule files, 503 MB in all), so one run is roughly 50 calls (Bike catalogs may take a second page) and 3.5 points on the Basic category (`calls / 1500 + bytes / 150 MB`, see [operations.md](operations.md), "TDX points per day"), about 15 to 17 points in a month of 4 to 5 runs. This is an estimate from file sizes; the `tdx usage:` block at the end of a job log has the measured numbers (calls by status and response bytes by dataset), and the first run on the real cluster should replace this paragraph's figures.

Edit `refresh-static.sh`, `refresh.yaml` or the other files in the overlay and run `overlays/orbstack/tests/run.sh` (bash, jq, kubectl; no cluster): fake `via-data`/`kubectl` tests of every failure path plus a rendered-manifest golden file (`tests/render.sh --update` after an intended change to the manifests).

## First build

A fresh Mac has no build, so the server cannot start and the job has nothing to update. Make the first one by hand after `up` has built `via-server:dev` (or from the host with `cargo run --bin via-data`): a bare `via-data fetch` is now paced and safe,

    kubectl --context orbstack -n via port-forward svc/valhalla 8002:8002 --address 127.0.0.1 &
    docker run --rm --entrypoint via-data -e VIA_VALHALLA_URL=http://host.docker.internal:8002/ \
      -v "$PWD/data:/build/server/data" via-server:dev fetch
    docker run --rm --entrypoint via-data -e VIA_VALHALLA_URL=http://host.docker.internal:8002/ \
      -v "$PWD/data:/build/server/data" via-server:dev build
    pkill -f 'port-forward svc/valhalla'

(the credentials come from `-e TDX_CLIENT_ID -e TDX_CLIENT_SECRET` after `set -a; . ./.env; set +a`; do not echo them), then `deploy/orbstack.sh up`. `bus_derived_segment_scale_percent` (default 290, `config/via.toml`) is applied at build, so changing it needs a rebuild: run the job.

To serve an older or experimental build, `deploy/orbstack.sh data <build-id>`. The next weekly run replaces it with a fresh build.

Never `rm -rf` anything under `server/data/build` or `server/data/fetch` by hand: builds are shared by all sessions, and the running pods mount the active one. `via-data build` repoints `data/build/.via-static-latest.json` to the new build.

## Troubleshooting

- Pod `CreateContainerConfigError`: the `tdx-credentials` Secret is missing.
- Server not Ready: `kubectl --context orbstack -n via logs deploy/server --tail=50`; usually the build directory has no `.via-static.json` or Valhalla is not ready.
- Logs never contain request coordinates; do not add any.

## Auto-deploy

A GitHub Actions self-hosted runner on this Mac deploys `main` to the stack after the `Rust server` CI is green. Workflow: `.github/workflows/deploy-orbstack.yml`; script: `deploy/orbstack-deploy.sh`. Nothing happens until you register the runner (below). Auto-deploy never builds data: it only reads `VIA_DATA_DIR` and `VIA_TILES_DIR` and runs `orbstack.sh up` for the commit, which uses the build `data/build/.via-static-latest.json` points to at that moment. The weekly data refresh is separate.

### What it does

1. `workflow_run` fires when `Rust server` completes on `main`. The job runs only if the conclusion is `success`, the triggering event is `push` and the run is from this repository. (The event check matters: a fork pull request can have a branch named `main`.) `workflow_dispatch` is also allowed, on `main` only; the script then asks the GitHub API for a green `Rust server` push run for the latest commit at or before it that touches `server/` or `server.yml` (a docs or iOS commit has no run of its own).
2. It checks out exactly that commit in the runner workspace (`~/actions-runner-via/_work/Via/Via`, the dedicated directory) and the script refuses to go on unless `HEAD` is that commit, the commit is on `origin/main`, and `VIA_DATA_DIR` / `VIA_TILES_DIR` are outside the checkout (a checkout clean must never reach shared data). It skips a commit older than the one already deployed, and skips (exit 0, logged) while any Job is active in namespace `via`, such as a data refresh.
3. It runs `orbstack.sh up`, then polls `http://127.0.0.1:3000/health/ready` (up to 120 s, `VIA_READY_TIMEOUT`), then records the commit on the `server` Deployment (annotation `via.local/deployed-sha`).
4. If the build, apply, rollout or readiness check fails, the job fails (red run, pod list and server log tail in the log, outcome in the run summary) after rolling every Deployment back to its previous revision and restoring the previous image (`via-server:previous` is tagged before each deploy). `server` and `valhalla` roll out with surge, so their old pods keep serving during a failed rollout. `tdx-gateway` uses `Recreate`: it is down while it restarts, and so are plans that need live data. A run that is cancelled or times out gets no automatic rollback; use the manual restore under "Roll back".
5. One deploy at a time (`concurrency` group, running deploys are never cancelled), a 60 minute timeout, `permissions: contents: read`, no `secrets.*`. The TDX Secret exists only in the cluster.

A change to `deploy-orbstack.yml` alone does not run `Rust server`, so deploy it with Actions > Deploy to OrbStack > Run workflow.

### Pause switch

    touch ~/.via-no-deploy     # pause: the job exits successfully, log line "paused: ... not deploying"
    rm ~/.via-no-deploy        # resume

Use it while you test by hand and during a data build. Anyone (tester, engineer) who runs `orbstack.sh up` or `data` from a worktree must `touch ~/.via-no-deploy` first, or a deploy can replace their build. A forgotten pause file skips every deploy while the runs show green, so check for it when a merge does not show up (`ls ~/.via-no-deploy`). It only stops deploys that have not started; a running deploy finishes. Set `VIA_NO_DEPLOY_FILE` in the runner `.env` to use another path.

### Register the runner

**First apply the fork approval setting below, before the runner exists.** Then do this on the repo (Settings > Actions > Runners > New self-hosted runner, macOS, ARM64). The page shows the download commands and a one-time registration token; do not paste the token anywhere else.

    mkdir ~/actions-runner-via && cd ~/actions-runner-via
    # download and extract as shown on the GitHub page, then:
    ./config.sh --url https://github.com/chengpong1127/Via --token <token from the page> --labels via-orbstack --name via-orbstack
    printf 'VIA_DATA_DIR=%s\nVIA_TILES_DIR=%s\n' "$HOME/Documents/Via/server/data" "$HOME/Documents/Via/server/var/valhalla" >> .env
    echo "$PATH" > .path          # run in a shell where kubectl, docker, jq, curl and git work
    ./svc.sh install && ./svc.sh start

Use the real absolute paths of your shared data and tiles directories. A launchd job may be blocked by macOS privacy (TCC) from reading `~/Documents`; on the first run check the log for "Operation not permitted" and, if so, grant Full Disk Access to the runner's `bin/runsvc.sh` / `node` in System Settings > Privacy & Security, or move the data dirs. `svc.sh` installs a launchd agent for your user, so it runs while you are logged in, with OrbStack running. The runner needs `kubectl`, `docker`, `jq`, `curl` and `git` in `.path`.

### Public repo risks and settings

The repo is public and the runner executes code as your user, with Docker and cluster-admin `kubectl` (so it can read the TDX Secret and your files). Treat this as remote code execution on your Mac:

- The workflow never triggers on `pull_request`, `pull_request_target`, `issue_comment` or anything else a fork can start; `server/deploy/tests/deploy-workflow-test.sh` fails CI if that changes. Never add another workflow with `runs-on: [self-hosted, ...]`.
- Whoever can push to `main` or run workflows can run code on this Mac, so keep write access to people you trust and protect `main` (require a pull request).
- **Before registering the runner**, set the fork pull request approval policy to all external contributors (Settings > Actions > General > "Fork pull request workflows from outside collaborators" > "Require approval for all external contributors"). The default, `first_time_contributors`, is not enough: a second PR from the same account would run without approval. By command:

      gh api -X PUT repos/chengpong1127/Via/actions/permissions/fork-pr-contributor-approval -f approval_policy=all_external_contributors
      gh api repos/chengpong1127/Via/actions/permissions/fork-pr-contributor-approval   # check

  **Approving a fork run executes that PR's workflow files on your Mac. Never approve a fork run whose diff touches `.github/`.** (Today no workflow in the repo uses the self-hosted label, and the lint test keeps it so; the setting covers a PR that adds one.) Also set Workflow permissions = Read repository contents. If the repo belongs to an organization, restrict the runner group to this repository (Settings > Actions > Runner groups). A repo-level runner is already limited to this repo.
- Do not put runner or repo secrets in the workflow or `.env`; the cluster Secret is enough.

### Roll back

A failed deploy rolls back by itself; a cancelled or timed-out run does not, so after one run the quick restore below and check `orbstack.sh status`. To go back after a deploy that passed, revert the commit on `main` (it deploys like any other), or by hand, from a checkout of the good commit: `server/deploy/orbstack.sh up`. While you are at it, pause deploys first. Quick restore of the last image: `docker tag via-server:previous via-server:dev && kubectl --context orbstack -n via rollout undo deploy/server deploy/tdx-gateway`.

### Confirm the first deploy

After registering: the runner shows Idle with labels `self-hosted, macOS, ARM64, via-orbstack`. With deploys paused (`touch ~/.via-no-deploy`), run the workflow (Actions > Deploy to OrbStack > Run workflow, branch `main`): it should succeed and log `paused`. Then `rm ~/.via-no-deploy`, run it again and watch: build, rollout, `deployed <sha>`. Check `kubectl --context orbstack -n via get deploy server -o jsonpath='{.metadata.annotations.via\.local/deployed-sha}'` equals `git rev-parse origin/main`.

### Remove it

    cd ~/actions-runner-via && ./svc.sh stop && ./svc.sh uninstall
    ./config.sh remove --token <removal token from Settings > Actions > Runners>
    cd ~ && rm -rf ~/actions-runner-via ~/.via-no-deploy

Then delete `.github/workflows/deploy-orbstack.yml` (and `orbstack-deploy-checks.yml`) in a PR. This removes only the runner directory; shared data and tiles are not under it. The stack keeps running.
