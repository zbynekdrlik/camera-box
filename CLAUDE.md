# Claude Code Guidelines for camera-box

Rust app for embedded NDI cameras (CAM1-4): multi-camera NDI streaming with software genlock + intercom/sidetone audio. Built locally, deployed to the camera devices over SSH.

<!-- Global rules inherited from ~/.claude/CLAUDE.md (managed by airuleset) -->
<!-- PR merge policy, CI monitoring, TDD, two-branch git workflow, test strictness, security, comprehensive logging apply automatically. This file holds ONLY camera-box-specific context — do not duplicate global rules here. -->

## Playbook router

- recording-verdict QR decode path (fast/robust gate, per-recording burn sets, #186 fixtures) → load `.claude/skills/recording-decode`

## Branch model

Two branches: `main` (production) + `dev`. Work on `dev`, PR to `main`. Standard global two-branch workflow.

## DO NOT DELETE These Files

**NEVER delete `targets.md`** - it contains IP addresses for all deployment targets (Windows and cameras). This file has been accidentally deleted multiple times during PR cleanup. DO NOT remove it.

## Script Failure Policy

**IMPORTANT:** When a setup script or automation script fails:

1. **DO NOT** manually run commands to complete the failed steps
2. **DO** fix the script to handle the failure case
3. **DO** re-run the fixed script from the beginning
4. **DO** commit the script fix before proceeding

This ensures:
- Scripts are always complete and self-contained
- Future runs will succeed without manual intervention
- No undocumented manual steps exist in the deployment process

## Device Setup

When setting up a new camera device:
- Use `scripts/setup.sh` - it handles everything
- NDI library must be copied manually (licensing restriction)
- Device registry is in `SETUP.md`

## Configuration Reference

| Setting | Correct Value | Example Result |
|---------|---------------|----------------|
| `ndi_name` | `"usb"` | CAM2 (usb) |
| `hostname` | Device name | CAM2 |
| `intercom.stream` | Lowercase device | cam2 |

## IP Assignment

| Device | IP Address |
|--------|------------|
| CAM1 | 10.77.9.61 |
| CAM2 | 10.77.9.62 |
| CAM3 | 10.77.9.63 |
| CAM4 | 10.77.9.64 |

## Build & Deploy

The release binary is built by **CI on GitHub free `ubuntu` runners** — never locally.
The cameras run x86_64 Ubuntu, the same architecture as the runners, so the CI artifact
runs directly on the devices (no cross-compilation).

Every push to `dev`/`main` runs `ci.yml`, whose `build` job uploads the
`camera-box-linux-amd64` artifact (`target/release/camera-box`). Tagged releases
(`v*`) also publish a tarball via `release.yml`.

**Probe/verdict tooling (#192) is ALSO built by CI** — never locally (the local
`--features probe` build was the root of the 14GB `target/`, the OOM-on-build, and
the disk drain). A separate CI step builds the probe binaries with `--features probe`
and uploads them as the `probe-tools-linux-amd64` artifact (`recording-verdict`,
`frame-probe`, and the probe-featured `camera-box`). The E2E / proof flow DOWNLOADS
that artifact for the commit under test and runs it on dev1 — it never compiles
locally:

```bash
gh run download --repo zbynekdrlik/camera-box -n probe-tools-linux-amd64 --dir ./probe-bins
# gh run download strips the +x bit — re-add it to every probe binary you use
# (recording-verdict, frame-probe, camera-box-probe).
chmod +x ./probe-bins/*
```

**Recording analysis runs ON stream.lan, NOT on dev1 (#193).** OBS records the 0.7–6 GB
program file on the powerful Windows **stream box (10.77.9.204** — strong CPU, lots of RAM,
fast disks). The OLD flow DOWNLOADED that multi-GB file over the LAN to slow dev1 (a PC meant
only to run Claude) and decoded + rqrr'd it there — the root of the slow transfers, the dev1
OOM (#187), the 14 GB+ disk fill, and the repeated stalls. **Decode WHERE THE VIDEO IS:** run
`recording-verdict` on the stream box against the LOCAL recording and bring back ONLY the small
verdict JSON (+ a few pixel-proof PNGs). dev1 holds NOTHING big.

CI's `windows-probe` job builds `recording-verdict.exe` (probe-featured) on a `windows-2022`
runner and uploads it as the **`probe-tools-windows-amd64`** artifact. (The Linux-only
appliance/hardware crates — v4l/alsa/cpal/evdev/drm/libc-ioctl — are confined to
`cfg(target_os="linux")` so the pure-Rust verdict cross-builds clean.) ffmpeg/ffprobe are
already installed on stream.lan (winget; ffmpeg 8.0.1 on PATH) — no bundling needed.

The win-stream-snv MCP drives the on-box run (scp/ssh to Windows is DENIED on this rig):

```bash
# 1. Download the CI-built Windows verdict for the commit under test (NEVER build on dev1).
gh run download --repo zbynekdrlik/camera-box -n probe-tools-windows-amd64 --dir ./probe-win
# 2. Upload it to the stream box ONCE via the win-stream-snv MCP FileUpload:
#      path='C:\camera-box\recording-verdict.exe'  <- ./probe-win/recording-verdict.exe
# 3. Run it THERE against the LOCAL recording (NO download to dev1) via win-stream-snv Shell.
#    scripts/recording-verdict-on-stream.sh PRINTS the exact PowerShell command + the
#    upload/pull-back plan (paths are the box-local Windows paths):
scripts/recording-verdict-on-stream.sh \
  --verdict-exe 'C:\camera-box\recording-verdict.exe' --out-dir 'C:\camera-box\verdict-out' \
  --stream-rec 'C:\path\on\box\stream-REC.mp4' \
  -- --strih 'C:\path\on\box\strih-REC.mkv' --stream 'C:\path\on\box\stream-REC.mp4' \
     --min-secs 300 --json 'C:\camera-box\verdict-out\verdict.json' \
     --out-dir 'C:\camera-box\verdict-out\pixel-proof'
# 4. Pull back ONLY the small results via win-stream-snv FileDownload: the verdict JSON +
#    the handful of pixel-proof PNGs under C:\camera-box\verdict-out. The recording stays on
#    the box and is NEVER copied to dev1.
```

The recording E2E harness (`scripts/recording-e2e.sh`) DEFAULTS to this on-stream flow
(`VERDICT_ON_STREAM=1`): it does NOT download the recordings to dev1 and emits the on-stream
plan at step [8/8]. `VERDICT_ON_STREAM=0` selects the legacy decode-on-dev1 path (still present
for a box with no uploaded verdict.exe). **Never decode a multi-GB recording on dev1.**

**IMPORTANT:** Use IP addresses, not hostnames (`.lan` DNS may not resolve):

```bash
# 1. Download the CI-built binary for the commit you want to deploy.
#    Latest run on the current branch:
gh run download --repo zbynekdrlik/camera-box -n camera-box-linux-amd64 --dir ./dist
#    …or pin a specific run: gh run download <run-id> -n camera-box-linux-amd64 --dir ./dist
chmod +x ./dist/camera-box

# 2. Deploy to device (use IP from table above, password: newlevel)
sshpass -p 'newlevel' ssh root@10.77.9.6X "mount -o remount,rw / && systemctl stop camera-box"
sshpass -p 'newlevel' scp ./dist/camera-box root@10.77.9.6X:/usr/local/bin/
sshpass -p 'newlevel' ssh root@10.77.9.6X "systemctl start camera-box && mount -o remount,ro / 2>/dev/null; true"
```

Note: `rw-mode`/`ro-mode` scripts may not exist on all devices. Use `mount -o remount,rw /` instead.
Deploy only CI artifacts from a committed, pushed ref (per `deploy-from-clean-tree.md`) — never a locally built binary.

## Local Build Policy

**Tier 0 (default) — CI builds the deployable binary; local checkouts run cheap checks only.**

The cameras (CAM1-4) are x86_64 Ubuntu, identical to the GitHub free `ubuntu` runners, so
CI produces the exact binary that ships to the devices — there is no need for a local
release build. Deploy the CI artifact (see Build & Deploy above).

Run locally before every push:

```bash
cargo fmt --all --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --no-run
```

Heavy builds run in CI only: `cargo build --release`, running `cargo test`, `cargo bench`.
Purge `target/` when stale — CI rebuilds it.
