---
paths:
  - "scripts/optical-chain-alert-watchdog.sh"
  - "scripts/lib/optical-chain-health.sh"
  - "scripts/lib/optical-chain-preflight.sh"
  - "scripts/lib/cambox-parallel-restore.sh"
  - "systemd/optical-chain-alert-watchdog.*"
---

# Cam2 optical-injection-leg health: dead-painter / optical-black detection, alert, fail-fast (#860)

The cam2 optical injection leg = a `frame-probe --paint-only` painter draws a dual-QR on cam2's
physical monitor → the cam1 camera films that monitor → strih OBS renders it. A dead painter (dark
monitor) silently poisons consecutive gate runs (live incident 2026-08-14: chain of failed E2E
cleanups left the painter dead, next gate reported the optical hop UNAVAILABLE, NO alert fired).

## Adding/maintaining a dev1-side alert watchdog — reuse the established framework

Do NOT invent a new alert mechanism. The fleet's dev1-side alert watchdogs all share one shape
(`imag-power-envelope-alert-watchdog.sh` #1040, `imag-obs-alert-watchdog.sh` #882,
`obs-liveness-watchdog.sh` #391): a systemd `--user` timer runs a `set -uo pipefail` (NOT `-e`)
script that measures over ssh/OBS-WS, runs a PURE decision lib, and pages via `airuleset.py notify`.
Shared pieces you MUST reuse, never re-implement:
- `scripts/lib/obs-watchdog-decision.sh`: `obs_watchdog_confirm <prev> <is_incident> <threshold>`
  (page only after N consecutive incident passes — guards against a transient) + 
  `obs_watchdog_alert_throttle <sig> <prior_sig> <prior_passes> <n>` (re-alert cadence).
- A state file with `read_state_field`/`write_state_field`; reset `confirm`/`alert_sig`/`alert_passes`
  on every healthy/skip pass (a NEW episode with the same signature must page fresh).
- Connectivity failure (empty ssh probe / WS unreachable) = "nothing to decide this pass", NEVER a
  false alert.
- The systemd `.service` documents secret env via `EnvironmentFile=-%h/.config/...` (leading `-` =
  optional); the secret (OBS WS password) is NEVER committed.

## The TEST/EVENT painter discriminator (durable, non-staling — DO NOT use the #281 heartbeat)

Whether a dark monitor is an incident depends on whether a painter is EXPECTED. The durable signal
is the state `rig-mode.sh` already maintains — NOT the #281 rig-heartbeat (that is stale-after-10-min
by design, the WRONG gate for a standing 2-h TEST painter):
- `painter_expected` = `/run/rig-painter.pid` present OR `cam2-painter.service` is-enabled.
  rig-mode.sh TEST writes the pidfile (+ may enable the service); EVENT mode REMOVES the pidfile
  (`painter_stop_remote`) AND disables the service (#892). So EVENT mode → expected=0 → no false page.
- `painter_alive` = pidfile PID alive (`kill -0`) OR `cam2-painter.service` is-active.
- A present-but-dead pidfile (crashed frame-probe — the pidfile is shell-written, frame-probe never
  removes it) = expected&&!alive = the incident.
- The harness's OWN painter (recording-e2e.sh) is pgrep-tracked, NOT `/run/rig-painter.pid` — that
  pidfile is the STANDING rig-mode painter only.

## Live optical proof — reuse assert-program-nonblack, never a new black-check

`obs_phase2.py assert-program-nonblack --host <strih>` (#901) is the read-only optical proof
(process-alive is NOT QR-on-screen, #754). Classify its rc+output: rc 0 = OK; non-zero with
"renders BLACK" = BLACK; any other = UNKNOWN (nothing to decide). It needs the strih OBS WS password.

## Fail-fast in the [0/8] preflight — NEVER false-abort a CI gate

The user's hardest constraint. The `optical_chain_preflight_assert` (sourced-lib #675 pattern, no
anchored recording-e2e.sh line edited) HARD-ABORTS (`exit 1`, plain statement so it propagates — NOT
in a `$()`/pipeline) ONLY on painter-EXPECTED-but-DEAD (cam2 ssh only, no OBS dependency), with a
grace re-probe first (cam2-painter.service is Restart=always/RestartSec=2, so a single read can land
in a transient restart window). A strih-BLACK read is a WARN, never an abort (a program legitimately
not yet showing a camera would false-abort). No standing painter / ssh failure = skip (the harness
launches + liveness-checks its own painter, and prod-scene has its own non-black self-check).

## The #712 cleanup restore-failure must SURFACE, not stay a buried stderr WARNING

`cambox_parallel_wait_and_report` records failed labels into `CAMBOX_PARALLEL_FAILED_LABELS` (reset
at the START of each call — the caller reads it AFTER); `cambox_parallel_surface_painter_failure`
turns a `cam2/painter` failure into a GitHub `::error::` annotation (other boxes `::warning::`).
NEVER `exit` from this lib — cleanup()'s EXIT trap (`set +e`) must always run to completion.
