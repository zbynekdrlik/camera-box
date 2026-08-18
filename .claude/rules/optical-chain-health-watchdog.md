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

## #1117 — false PAINTER-DEAD page during a live E2E + owner-facing page-text doctrine

Two independent false-page gaps (live 2026-08-18T22:59:57): the watchdog paged `alert:PAINTER-DEAD`
while a Full-path E2E run legitimately `systemctl stop cam2-painter.service` (issue-872 stop+restore)
and ran its own transient `frame-probe --paint-only` — the pass even measured `optical=OK`.

**E2E-window signal — REUSE `rig_heartbeat_active`, never invent a busy detector.**
`scripts/lib/rig-heartbeat.sh`'s `rig_heartbeat_active` (the FRESH #281 rig-active heartbeat that
`recording-e2e.sh` starts + refreshes every 30s and whose refresher removes it the instant the
harness dies) IS the dev1-side "a live gate/TEST harness is coordinating the rig RIGHT NOW" signal.
`obs-burn-reconcile-watchdog.sh` (#1060) already reuses it as "defer, never fight a live harness".
The optical-chain watchdog now sources it and passes `rig_busy` as the 4th arg of
`optical_chain_alert_condition <expected> <alive> <optical> [rig_busy]` (default 0 → 3-arg callers
unchanged). Do NOT use a transient-frame-probe ssh probe or the rig-lease as a second detector.
NB: the #281 heartbeat is stale-after-10-min BY DESIGN — correct here (an ACTIVE run keeps it fresh),
but it is the WRONG gate for an IDLE standing TEST painter (that is the pidfile/service `painter_expected`
signal, above). A genuinely dead standing painter OUTSIDE a run (rig_busy=0, optical≠OK) still pages.

**Outcome veto is the generalizable fix, not blanket E2E silencing.** `optical=OK` on a painter-dead
pass → `log-only:PAINTER-DEAD-optical-ok` (the monitored cam2→cam1 hop is provably readable, so
whatever paints the monitor works). This ALONE would have stopped the live page. `log-only:*` verdicts
route to `clear_throttle` like a healthy pass, with a distinct suppression log line.

**When to add E2E-window suppression to ANOTHER dev1 watchdog (the #1117 gap-b audit rule):** ONLY
where the E2E harness DELIBERATELY creates the exact condition the watchdog pages on. optical-chain
(painter stopped by design) qualifies; splitter-port already reports its `systemctl stop camera-box`
E2E-stop as report-only `NO_CAPTURE`. cadence / mv-fps / network-reach / avsync-heartbeat /
bundle-state / obs-liveness / imag-obs / imag-power-envelope page on conditions the E2E does NOT
deliberately create — suppressing them during a run would MASK a real fault in the exact window it
matters, so they get NO suppression.

**Owner-facing PAGE text doctrine (`notify --body` only; internal log lines stay English):** plain
Slovak, outcome-first (what it means for the rig), with EXPLICIT ownership — agent-recoverable →
"Rieši Claude automaticky, ty nemusíš nič robiť"; a genuinely physical fault (HDMI splitter cable,
dead NIC / box off, cooling) → an honest "Potrebný fyzický zásah — …" human step; report-only /
operator-domain → "len INFO …". Never a false "Claude rieši" for something Claude can't auto-fix. The
owner must never wonder "co mam akoze ja s tym robit". Guarded by
`tests/harness_optical_chain_watchdog_1117.rs` (static anchors on the Slovak markers + the dropped
English "Confirmed over …" phrase). The airuleset#546 machine-channel ROUTING of agent-recoverable
pages is an `airuleset.py notify` feature (out of camera-box scope) — here only the page TEXT changed.
