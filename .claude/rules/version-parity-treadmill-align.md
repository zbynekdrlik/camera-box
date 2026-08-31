---
paths:
  - "scripts/lib/camera-box-parity-align.sh"
  - "scripts/lib/frame-probe-parity-align.sh"
  - "scripts/camera-box-version-gate.sh"
  - "scripts/deploy-fleet.sh"
  - "tests/harness_camera_box_parity_align_1202.rs"
  - "tests/frame_probe_parity_align_1138.rs"
---

# camera-box version-parity treadmill + the pre-[0/8] auto-align (issue 1202)

## The treadmill (why the [0/8] camera-box gate refuses "for no reason")

`camera-box-version-gate.sh` (#875/#1136) reads each active box's `/usr/local/bin/camera-box
--version` and PINS it to `origin/main`'s Cargo.toml, with a `--candidate-pin` ACCEPT that passes
only when the whole active fleet is uniformly on THIS run's candidate. **During active dev,
`origin/main` lags `dev` by dozens of builds** (live: pin `dev.481` while the fleet ran `dev.550`),
so the main-pin can never match and the **candidate-pin accept is the ONLY passing path** — but each
dev commit bumps the candidate, leaving the fleet one build behind (`candidate-1`). The gate then
REFUSES every E2E until a manual `deploy-fleet` (live killed runs 32883434208 / 32892551674). That
is not a bug in the gate — it is a missing deploy step.

## The fix: `cambox_parity_align_before_gate` runs BEFORE the gate (scripts/lib/camera-box-parity-align.sh)

Wired into `recording-e2e.sh` right before the `[0/8]` gate call, on the SAME node list. It reads
each active box's version and, **only when the fleet is uniformly on ONE build != candidate (every
active box read)**, deploys the candidate to `/usr/local/bin/camera-box` fleet-wide so the gate's
own candidate-pin accept then passes. The gate is UNTOUCHED (still the single authority); the align
is best-effort. `cambox_align_action` verdicts: `NOCANDIDATE|UNKNOWN|MIXED|NOACTIVE|OK|ALIGN` (only
`ALIGN` deploys; `UNKNOWN` > `MIXED`; honours `CAMBOX_OFFLINE_ACK`). Mixed-between-boxes / any unread
box → NO deploy → the gate refuses exactly as before. Skipped under `CAMERA_BOX_VERSION_GATE_NO_MAIN_PIN=1`
(operator soak — never auto-realign over a deliberately-deployed build).

## THE BUILD-ORDER TRAP (cost a review round, issue 1202) — a [0/8] step CANNOT use `$PROBE_BIN_DIR`

`$PROBE_BIN_DIR/camera-box` (the harness's own candidate binary) is **built/normalized at `[1/8]`,
AFTER the `[0/8]` gate** (recording-e2e.sh: `PROBE_BIN_DIR` assigned + the CI `camera-box-probe`→
`camera-box` rename + the local `cargo build` all live in the `[1/8]` block). The workflow sets no
`PROBE_BIN_DIR`/`USE_PREBUILT_PROBE_DIR` before the harness. So at `[0/8]` that path is `target/release/
camera-box` = **STALE (a prior run's build) or ABSENT** — a `[0/8]` step that sources it silently
ships a stale build or no-ops.

**Rule: any `[0/8]`-time step needing the run's candidate camera-box binary must source the CLEAN
`camera-box-linux-amd64` CI artifact** (downloaded from the newest successful `ci.yml` run on the
candidate branch via `gh run download` → `deploy-fleet.sh --binary`), NEVER `$PROBE_BIN_DIR`. That
artifact EXISTS at `[0/8]` (ci.yml built it on the dev push) and is the correct clean production
binary (not the probe-featured one). **Always version-GUARD it: if the newest published build !=
the candidate (its own ci.yml not done yet), do NOT deploy** — never ship a stale build to "align";
the gate refuses and self-heals once ci.yml publishes the candidate. The same `[0/8]`-before-`[1/8]`
ordering fact is why the sibling frame-probe align (below) ALSO fetches the CLEAN CI artifact via
`gh run download` at `[0/8]`, never `$PROBE_BIN_DIR` (which is built at `[1/8]`).

## The frame-probe (cam2 painter) SIBLING align (issue 1138)

`scripts/lib/frame-probe-parity-align.sh` is the frame-probe twin of this camera-box align, same
shape and same `[0/8]` placement, for a DIFFERENT binary: cam2's steady-state painter
(`/usr/local/bin/frame-probe`, run by `cam2-painter.service`). WHY it was needed: frame-probe is
auto-deployed ONLY at dev→main merge (ci.yml `deploy-fleet` is main-only), so between merges the
deployed painter silently LAGGED the current build (the live 2026-08-29 incident — an uncompensated
QPSK A/V marker + a dark issue-1196 aux tick until a MANUAL redeploy). `frame_probe_parity_align_before_gate`
deploys the candidate painter to cam2 every E2E run so pin+deploy advance together (orphan-PROOF).

Key differences from the camera-box align (do NOT copy them across blindly):

- **Source of truth = the clean `probe-tools-linux-amd64` CI artifact** (fetched via `gh run download`,
  the frame-probe binary inside it), NOT the dev1 local `$PROBE_BIN_DIR` build. full-path-e2e.yml
  does NOT set `USE_PREBUILT_PROBE_DIR`, so `[1/8]` builds frame-probe LOCALLY on dev1 — a
  byte-different sha for the same source. Pinning against the CI artifact makes the sha compare
  EXACT (both sides = the artifact bytes) and matches what ci.yml deploy-fleet actually ships.
- **Version-guard has no `--version` on frame-probe** — guard via the co-located `camera-box-probe
  --version` in the SAME probe-tools artifact == the Cargo.toml candidate; an UNRESOLVABLE candidate
  ("") REFUSES (never align blindly — the align decision keys on the SHA, so unlike camera-box an
  empty candidate would otherwise disable the guard entirely).
- **Deploy path = a NEW `deploy-fleet.sh` frame-probe-ONLY mode** (`--frame-probe` WITHOUT
  `--binary`/`--run`): deploys ONLY the cam2 painter (the issue-892 enable-state-preserving
  lifecycle, `frame_probe_restore_enable_decision`), NEVER a camera-box fleet deploy. It FAILS LOUD
  if cam2 is not in `CAMERA_SET` (its only job is that deploy — a skip must not report false success).
- **cam2-only + unconditional** (cam2 is the painter regardless of active-set membership); honours
  `CAMBOX_OFFLINE_ACK`; exports `FRAME_PROBE_ALIGN_CI_BIN` so recording-e2e's `[1/8]` report-only
  pin verifies the just-deployed painter against the SAME artifact bytes.
- **The gh-downloaded artifact dir is age-swept at the orchestrator's entry** (the mktemp runs in a
  `$(...)` subshell, so its path can't be reclaimed by the caller — a >2h age-bounded sweep of
  `frame-probe-align-ci.*` bounds the /tmp leak without racing a concurrent run).
- **Still REPORT-ONLY** (the pin never exits non-zero): the exit-code hard-gate flip is the
  supervisor's #758 two-step follow-up, only after the auto-align is rig-proven — unlike camera-box
  (whose gate hard-refuses), there is NO hard gate behind this pin yet, so that follow-up must be
  tracked or the "orphan SCREAMS" becomes a new dormant log line.

Same Tier-0 seams idea as camera-box (`.claude/rules/*` + `tests/frame_probe_parity_align_1138.rs`):
`FRAME_PROBE_ALIGN_ARTIFACT_DIR` (pre-fetched dir, skip gh), `FRAME_PROBE_ALIGN_SKIP_VERSION_GUARD`,
`FRAME_PROBE_GATE_SHA_<NAME>` (deployed-sha read seam, shared with the gate's own report),
`FRAME_PROBE_ALIGN_DEPLOY_CMD`/`_DEPLOY_FLEET` (deploy override), `FRAME_PROBE_ALIGN_CANDIDATE`/
`_CARGO_TOML` (candidate version).

## Acked-offline boxes: exclude from the DEPLOY scope, not just the decision

`deploy-fleet.sh` does NOT consult `CAMBOX_OFFLINE_ACK`/`rig-fleet.txt`, so an acked box passed in
`CAMERA_SET` gets a (failed, or worse — clobbering an intentionally-staged box) deploy attempt. The
decision (`cambox_align_action`) excludes acked boxes AND the orchestrator must exclude them when
building the deploy `names` (`cambox_offline_ack_is_acked "$name" && continue` before `names+=`) —
mirror the gate.

## Tier-0 verification (zero cargo, #557) — seams make the impure path testable

The lib is source-only (`# airuleset:script-ok`, no top-level `set -euo pipefail`). Verify without
cargo: `bash -n` + `shellcheck -S warning`; source the lib under `set -euo pipefail` and drive the
decision matrix + the orchestrator + the real deploy path via the seams — `CAMBOX_ALIGN_DEPLOY_CMD`
(full deploy override), `CAMBOX_ALIGN_CANDIDATE_BIN` (a pre-fetched binary, skips gh),
`CAMBOX_ALIGN_DEPLOY_FLEET` (a fake deploy-fleet recording its `CAMERA_SET`+`--binary`), and the
gate's own `CAMERA_BOX_VERSION_GATE_VERSION_<NAME>` read seam (a file of raw `--version` output). A
green bash-level RED→GREEN predicts the `tests/harness_camera_box_parity_align_1202.rs` pass at CI;
`cargo fmt --all --check` is the only local Rust parse check (CI is the first type-check).

## Artifact resolution is COMMIT-SCOPED, never "newest ci.yml run on a branch" (issue 1245)

`frame_probe_align_resolve_ci_bin` (frame-probe-parity-align.sh) used to resolve the candidate
`probe-tools-linux-amd64` artifact via "newest successful `ci.yml` run on branch `dev`" — the
IDENTICAL pattern issue 1244 found broken in `cambox_align_deploy` (camera-box-parity-align.sh):
inside the self-hosted E2E runner job that resolution repeatedly returned ANCIENT runs even while
the candidate's own `ci.yml` was already `success` (the runner's own environment/token, not a
reproducible general `gh` bug — a plain interactive shell resolved the correct newest run). The
fix, mirrored per-file (frame-probe here, camera-box on its own ticket): resolve **by the run's
own candidate COMMIT** via `frame_probe_align_candidate_sha` (`gh run list --commit "$sha"`,
never `--branch`) — deterministic regardless of the anomaly, since "no run for this exact commit
yet" IS "the candidate genuinely not published yet" (self-heals once ci.yml completes).

**Resolution order (`frame_probe_align_candidate_sha`): `FRAME_PROBE_ALIGN_CANDIDATE_SHA` (explicit
seam) → `GITHUB_SHA` (Actions' auto-set var) → `git rev-parse HEAD` (local/non-CI, anchored via
`$_FPPA_HERE` so it's cwd-independent).** The explicit seam exists because of a critical trap: on a
`pull_request` event, `GITHUB_SHA` is the SYNTHETIC MERGE COMMIT the runner builds to test the
merge, NOT the PR's head commit — but `ci.yml` only ever runs (`push: [dev, main]`) and publishes
for the REAL head commit, so a bare `GITHUB_SHA` on `pull_request` is a PERMANENT false-refuse, not
merely stale-until-retry. `full-path-e2e.yml` (the only `pull_request`-event caller of this lib)
wires `FRAME_PROBE_ALIGN_CANDIDATE_SHA: github.event.pull_request.head.sha` on that event — the
SAME #703 pattern its "Fetch the matching Windows recording-verdict.exe" step already uses;
push/workflow_dispatch runs resolve the seam's ternary to plain `github.sha` there instead (the
workflow always SETS the seam — never literally unset — but that resolves to the same value
`GITHUB_SHA` would, since both are the real head on those events).
`FRAME_PROBE_ALIGN_CI_BRANCH` (the old branch-selection seam) is REMOVED — nothing else consumed it.
