---
paths:
  - "scripts/lib/camera-box-parity-align.sh"
  - "scripts/camera-box-version-gate.sh"
  - "scripts/deploy-fleet.sh"
  - "tests/harness_camera_box_parity_align_1202.rs"
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
ordering fact is already documented for the sibling frame-probe report (recording-e2e.sh's
`#1138 frame-probe` comment: "that gate runs BEFORE this build").

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
