# mv-fps-alert-watchdog — install note (#1083, continuation of #771)

The Multiview-fps alert watchdog (`scripts/mv-fps-alert-watchdog.sh`) closes the LIVE half of #771:
#771 shipped the observability CORE — vendored libobs `render_display()` emits
`multiview-audit: monitor=N divisor=D rendered_fps=X target=Z floor=F cx=.. cy=..` ~every 5 s per
throttleable Multiview projector, `src/mv_audit.rs` parses+gates it, and `mv-fps-gate` is an
E2E-preflight / drift-guard consumer — but **nothing read it LIVE**, so a MV render collapse went
unalarmed unless someone ran `mv-fps-gate` by hand. This dev1 `--user` timer reads each OBS box's
newest log over ssh (a session-agnostic FILE read, allowed for a headless dev1 watchdog per
`win-ssh-vs-mcp`), runs `mv-fps-gate` over the latest samples, and — once a below-floor collapse is
**confirmed over 2 consecutive passes** — pages Discord. Same dev1-side topology as
`frozen-input-alert-watchdog` (#1052) / `network-reach-alert-watchdog` (#1001) /
`imag-obs-alert-watchdog` (#882). **NEVER reboots** (memory rule `imag-no-reboot-root-cause`):
detect + alert only, agent-driven recovery.

## It ships DISABLED by default — on purpose

These units are committed but **NOT installed and NOT enabled** by this repo. Before this watchdog
runs unattended, the **SUPERVISOR** must install it, **live-verify** it (a genuine collapse alerts;
a healthy box never false-positives), and only then enable the timer. Do **not** enable it as part
of merging the PR.

## Conservative gates (why it won't spam false alerts)

- Requires **2 consecutive confirmations** (`MV_FPS_ALERT_CONFIRM_THRESHOLD`, default 2) before
  paging — one below-floor read / transient dip is observe-only.
- A **PASS** (healthy) or **UNKNOWN** (unreadable box / no audit line) pass resets that box's
  confirm counter to 0.
- **Autostart-aware:** a fresh OBS start writes a NEW log file; the watchdog tracks each box's
  OBS-log identity and RESETS the confirm streak on a change, so a fresh instance's warm-up
  below-floor read never inherits a stale confirmation (the #391/#799 "counter reset → never page"
  fail-safe). Combined with the 2-pass confirm at the 5-min cadence, a warm-up never false-pages.
- **No-double-page:** if #1001 has a box CONFIRMED unreachable, reachability owns the page and
  MV-fps skips it; a failed log read or a missing audit line → UNKNOWN, never a page.
- **Fail-loud, never silent-unknown:** a box that is READABLE but emits no `multiview-audit:` line
  for `MV_FPS_ALERT_TAP_BLIND_THRESHOLD` consecutive passes (a pre-#771 OBS build, or the audit
  stopped) fires ONE "tap blind" WARN.
- Once a collapse is confirmed, repeat alerts for the same failing-monitor set are throttled to
  once every `MV_FPS_ALERT_THROTTLE_PASSES` passes; a recovery ("back above floor") ping fires once
  when a box we paged for is PASS again.

## The floor — calibrated against live-measured healthy state (#1083)

The floor emitted in each audit line and re-derived by the gate is `obs_multiview_floor_fps(canvas)
= canvas/2 − MULTIVIEW_AUDIT_FLOOR_TOLERANCE_FPS` (tolerance **2.0**, `obs-display-budget.h` mirrored
in `src/mv_audit.rs`). The #771 placeholder was **validated by measurement** (2026-08-17, ~45 min
per box; full distribution in issue #1083's validation comment):

| Box | Projector | Healthy rendered_fps | Floor | Observed collapse |
|---|---|---|---|---|
| imag | 1080p, `divisor=2` (canvas 60) | 29.4–30.1 (min 29.0) | **28.0** | monitor-3 → ~12fps for 5 min |
| strih | 4K, `divisor=1` (canvas 30) | 30.0 (uncontended) | **13.0** | → 9–11fps under an app stealing GPU/CPU |

Both healthy states stay cleanly above their floor, and BOTH observed collapses (imag 12fps, strih
9–11fps) fall below it — the watchdog would have correctly paged on each while never touching a
healthy sample. The **tightest point is imag's ~1–2fps clearance** (floor 28 vs healthy ≥29.0); if
live production load ever pushes imag's healthy MV toward 28, bump `MULTIVIEW_AUDIT_FLOOR_TOLERANCE_FPS`
(one constant, in `obs-display-budget.h` + `src/mv_audit.rs`, redeploy the OBS build) — the current
data does not require it.

## Supervisor install + live-verify procedure

```bash
# 0. The gate binary: dev1 needs mv-fps-gate (default features) from the probe-tools CI artifact.
#    Point MV_FPS_GATE_BIN at it (or drop it at ./target/release/mv-fps-gate — never a local Tier-0
#    release build). Verify: printf '' | $MV_FPS_GATE_BIN ; echo $?   # 2 = no samples (expected)

# 1. Dry-run a single pass — measure + decide + LOG only, NEVER alert:
MV_FPS_GATE_BIN=/path/to/mv-fps-gate scripts/mv-fps-alert-watchdog.sh --dry-run
#    Inspect the per-box `gate exit=… -> PASS/BELOW/UNKNOWN` + decision lines. With both boxes
#    healthy this must show PASS and NO "WOULD alert".

# 2. Install the --user units (dev1):
mkdir -p ~/.config/systemd/user
cp systemd/mv-fps-alert-watchdog.service ~/.config/systemd/user/
cp systemd/mv-fps-alert-watchdog.timer   ~/.config/systemd/user/
# Point at the mv-fps-gate CI artifact (and any non-default creds/addresses):
#   mkdir -p ~/.config/environment.d
#   printf 'MV_FPS_GATE_BIN=/path/to/mv-fps-gate\n' > ~/.config/environment.d/mv-fps-alert-watchdog.conf
systemctl --user daemon-reload

# 3. Live-verify BEFORE enabling the timer:
#    a) both boxes healthy -> a manual pass must NOT page:
systemctl --user start mv-fps-alert-watchdog.service ; journalctl --user -u mv-fps-alert-watchdog -n 50
#    b) simulate a collapse: feed the gate a synthetic below-floor log for ONE box via
#       MV_FPS_PROBE_CMD (a command printing `MVFPS_LOGID:x` + a `rendered_fps=9 … floor=13` line),
#       run TWO passes -> the second must log "WOULD alert" (dry-run) with the right box + monitor.

# 4. Only after both checks pass, enable the recurring timer:
systemctl --user enable --now mv-fps-alert-watchdog.timer
systemctl --user list-timers | grep mv-fps-alert-watchdog

# Disable later:
systemctl --user disable --now mv-fps-alert-watchdog.timer
```

## Live-verified at authoring (2026-08-17)

- **imag** end-to-end: real ssh log read → `mv-fps-gate` → `gate exit=0 -> PASS` (no false-page on
  the healthy box). The ssh read returned the log identity + 211 `multiview-audit:` lines.
- **strih** read shape: the `powershell -Command "$f=(gci …|last); if($f){ 'MVFPS_LOGID:'+$f.Name;
  gc $f.FullName -Tail N }"` command returns the log id + audit lines (`rendered_fps=30.0 floor=13.0`).
  The ssh transport is the same `sshpass … ssh` pattern `frozen-input-alert-watchdog` uses live.

## Tunables (env, override in the unit or environment.d)

| Var | Default | Meaning |
|---|---|---|
| `MV_FPS_BOXES` | `imag\|10.77.9.182\|linux strih\|10.77.9.202\|win` | boxes to watch: `name\|ip\|os` (os = linux\|win) |
| `MV_FPS_GATE_BIN` | `<repo>/target/release/mv-fps-gate` | the `mv-fps-gate` decision-engine binary |
| `MV_FPS_ALERT_CONFIRM_THRESHOLD` | `2` | consecutive below-floor reads before paging |
| `MV_FPS_ALERT_THROTTLE_PASSES` | `12` | passes between repeat alerts for the same collapse (~1h) |
| `MV_FPS_ALERT_TAP_BLIND_THRESHOLD` | `24` | readable-but-blind passes before the "tap blind" WARN (~2h) |
| `MV_FPS_OBS_LOG_TAIL` | `2000` | OBS-log tail lines read per box per pass |
| `MV_FPS_SSH_USER` / `MV_FPS_SSH_PW` | `newlevel` / `newlevel` | ssh creds (both boxes) |
| `MV_FPS_PROBE_CMD` | (unset) | full override for the per-box read (`<ip> <os>`) — tests / dry-run |
| `MV_FPS_ALERT_STATE_FILE` | `$XDG_RUNTIME_DIR/camera-box-mv-fps-alert.state` | per-box confirm/throttle/logid state |
| `MV_FPS_NETREACH_STATE_FILE` | `$XDG_RUNTIME_DIR/camera-box-network-reach-alert.state` | #1001 state read for the no-double-page guard |

## Follow-up (not in this PR)

The #771 point-3 wiring of the MV-fps floor read into `scripts/recording-e2e.sh`'s preflight is a
SEPARATE surface (a gate-time synchronous check vs this always-on alarm) and touches that file's
static-anchor minefield; `mv-fps-gate` already exists for it. Filed separately.
