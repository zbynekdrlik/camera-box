---
paths:
  - "scripts/bisect-smoothness.sh"
  - "scripts/lib/bisect-smoothness.sh"
  - "scripts/bisect-smoothness-points.tsv"
  - "scripts/deploy-fleet.sh"
  - "scripts/camera-box-version-gate.sh"
---
# Deploying a HISTORICAL camera-box build (bisect / rollback) — verified 2026-08-20 (#1150)

Deploying an OLD camera-box binary to a subset of the fleet (a smoothness bisect, a rollback probe)
has three moving parts that all fight you by default. All facts below verified from code/CI.

## 1. Find the run-id for a historical build
Every push to `dev`/`main` builds the `camera-box-linux-amd64` artifact via `ci.yml` (retention **90
days**, so ~last 3 months are reachable). Map a version/SHA to its CI run:
```bash
gh api "repos/zbynekdrlik/camera-box/actions/artifacts?per_page=100&name=camera-box-linux-amd64" --paginate \
  --jq '.artifacts[]|"\(.workflow_run.head_sha)\t\(.workflow_run.id)\t\(.created_at)\t\(.expired)"'
```
Then `git show <head_sha>:Cargo.toml` gives that run's version. A round-merge into `dev` is the push
tip (its own commit is not a build head), so `gh run list --commit <green-sha>` is usually EMPTY —
resolve via the artifact's `head_sha`, or ancestry-check the green commit against candidate head_shas.

## 2. Deploy a pinned artifact to a SUBSET
`scripts/deploy-fleet.sh --run <run-id>` downloads that artifact and does a full byte-verified deploy
(stop → remount,rw → scp → start → remount,ro → sha256 byte-verify → version read-back → genlock-emit
check). Restrict the boxes with `CAMERA_SET="cam1 cam2"` (default is `$CAMERA_ACTIVE_SET`). For an
EXPIRED artifact, `--run` fails — rebuild the SHA on CI and use `deploy-fleet.sh --binary <camera-box>`.

## 3. The version-parity gate REFUSES a mixed fleet — neutralize it deliberately
`recording-e2e.sh` `[0/8] camera-box version-parity gate` (`scripts/camera-box-version-gate.sh`)
reads each active box's REAL `camera-box --version` and refuses (exit **20**) unless every active box
matches ONE pin (origin/main, or `--candidate-pin` when the WHOLE fleet is uniformly on it). It is
UNCONDITIONAL in recording-e2e.sh (no SKIP env). A deliberately-mixed fleet (old on cam1/cam2, current
on cam3) will be refused. Real neutralization seams:
- `CAMERA_ACTIVE_SET="cam1 cam2"` + `CAMERA_BOX_VERSION_GATE_MAIN_PIN=<old version>` → gate sees a
  uniform sub-fleet and passes with HONEST reads (measure the control box in a separate run). Best.
- per-node `CAMERA_BOX_VERSION_GATE_VERSION_<NAME>=<file-with-version>` fixtures + `MAIN_PIN` → passes a
  single genuinely-mixed run, but the gate stops verifying reality (ok only under deterministic deploys).
- `CAMERA_BOX_VERSION_GATE_MAIN_PIN` ALONE does NOT pass a mixed fleet (a box off the pin → exit 20).

## 4. Run the E2E LOCALLY, and stop the auto-deploy from clobbering you
- Local E2E (not the PR gate): `E2E_EXECUTE_VERDICT=1 WIN_VERDICT_EXE_LOCAL=<recording-verdict.exe>`
  (`recording-e2e.sh:4253-4264`) decodes strih+stream over ssh on dev1. `full-path-e2e.yml` is
  push-triggered and pins to main, so it can't run an old mixed fleet — the local path is mandatory.
- `ci.yml` has a `deploy-fleet` job (`if: github.ref=='refs/heads/main' && push`) that AUTO-DEPLOYS
  the active fleet to main's build, honoring `rig-fleet.txt` offline-acks. During a historical-build
  window, **ack the staged boxes offline in `rig-fleet.txt`** or a dev→main merge redeploys over them.
