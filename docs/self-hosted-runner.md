# Self-hosted GitHub Actions runner (dev1 · `camera-lan`)

The hardware frame-loss E2E (`.github/workflows/loopback-e2e.yml`) drives cam2 over the
`10.77.9.0/24` LAN, which GitHub-hosted runners cannot reach. It therefore runs on a
**self-hosted runner on dev1 (develbox, `10.77.9.21`)** — already the documented build
host. This doc lets a cold session re-register the runner without guessing.

## What's registered

- **Runner name:** `dev1-camera-lan`
- **Labels:** `self-hosted, linux, x64, camera-lan` (the workflow targets `[self-hosted, linux, camera-lan]`)
- **Install dir:** `~/actions-runner-camera-box` on dev1
- **Service:** `actions.runner.zbynekdrlik-camera-box.dev1-camera-lan.service` (systemd, runs as `newlevel`, starts on boot)

Prereqs already present on dev1 (the build host): Rust toolchain + `cargo`, `sshpass`,
`scp`, `python3`, `libv4l-dev`, `libasound2-dev`, the NDI runtime.

## Re-register from scratch

```bash
cd ~
mkdir -p actions-runner-camera-box && cd actions-runner-camera-box
# Reuse the runner tarball from the existing dev1 runner, or download v2.333.1:
cp ~/actions-runner-spinbike/actions-runner-linux-x64-2.333.1.tar.gz . 2>/dev/null \
  || curl -sLo runner.tar.gz https://github.com/actions/runner/releases/download/v2.333.1/actions-runner-linux-x64-2.333.1.tar.gz
tar xzf actions-runner-linux-x64-2.333.1.tar.gz 2>/dev/null || tar xzf runner.tar.gz
cp ~/actions-runner-spinbike/svc.sh ./ && chmod +x svc.sh   # svc.sh ships with the package; copy if missing

# Short-lived registration token (needs `gh auth` with repo admin):
TOKEN=$(gh api -X POST repos/zbynekdrlik/camera-box/actions/runners/registration-token --jq '.token')
./config.sh --url https://github.com/zbynekdrlik/camera-box --token "$TOKEN" \
  --name dev1-camera-lan --labels self-hosted,linux,camera-lan --unattended --replace

sudo ./svc.sh install newlevel
sudo ./svc.sh start
```

## Health / operations (self-hosted = our responsibility)

```bash
# Status
gh api repos/zbynekdrlik/camera-box/actions/runners --jq '.runners[] | {name,status}'
systemctl status actions.runner.zbynekdrlik-camera-box.dev1-camera-lan.service

# Restart / stop / start the service
cd ~/actions-runner-camera-box && sudo ./svc.sh {stop|start}

# Remove the runner (deregister) — needs a remove token
TOKEN=$(gh api -X POST repos/zbynekdrlik/camera-box/actions/runners/remove-token --jq '.token')
sudo ./svc.sh uninstall && ./config.sh remove --token "$TOKEN"
```

## Safety

`loopback-e2e.yml` is **manual-dispatch only** (no `push`/`pull_request`) — the run takes
cam2 off-air, so an operator dispatches it only when that camera is free. It touches **only
cam2** (the off-air rig), never the production strih/stream OBS boxes. The script's
`trap cleanup EXIT HUP INT TERM` restores `camera-box` on cam2 even on failure or job
cancel; the workflow's final `if: always()` step re-asserts the service is active.
