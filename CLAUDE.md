# Claude Code Guidelines for camera-box

Rust app for embedded NDI cameras (CAM1-4): multi-camera NDI streaming with software genlock + intercom/sidetone audio. Built locally, deployed to the camera devices over SSH.

<!-- Global rules inherited from ~/.claude/CLAUDE.md (managed by airuleset) -->
<!-- PR merge policy, CI monitoring, TDD, two-branch git workflow, test strictness, security, comprehensive logging apply automatically. This file holds ONLY camera-box-specific context — do not duplicate global rules here. -->

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
