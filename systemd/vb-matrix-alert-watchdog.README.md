# vb-matrix-alert-watchdog — install note (#1227)

The dev1-side alert watchdog (`scripts/vb-matrix-alert-watchdog.sh`) closes the detection gap behind
the **2026-08-30 → 09-02 VB-Matrix outage**: VB-Audio Matrix (`VBAudioMatrix_x64.exe`) was **NOT
running** on the stream box from the 2026-08-30 10:45 reboot until 2026-09-02 14:01, because the
Scheduled Task `StartVBMatrix` has only a stale **one-shot TIME trigger, no AtLogon trigger**. Its
virtual **"VB-Matrix VASIO-8"** ASIO driver therefore had no host, so **both** stream OBS inputs
bound to it (`ASIO Input Capture` route 6/7, `test-audio` route 4/5) starved for 3+ days
(`asrc: … starved_blocks≈2940/min`) while `mbc` (Dante VSC route 0/1) stayed healthy — and **nothing
alarmed**. The `#1023` asio-starve watchdog ships DISABLED and needs a healthy-sibling discriminator;
the `#1226` audio-lag watchdog reads `ts_lag_ms`, not process presence. This watchdog reads process
presence directly.

It reads the **`vb_matrix_running` facet** `bundle_state_gather` (#1227) now exposes on each box's
`:8899/bundle-state.json` (composed on-box from a native `tasklist` process check paired with a disk
install-present gate — so "installed but dead" reads `"0"` and "not installed", e.g. imag, omits the
facet), polling both boxes from a **dev1 systemd --user timer** with `curl` (the ops-SKILL-mandated
method — an MCP-side `Invoke-WebRequest` hangs even when the server logs a prompt 200). It is a
sibling of `audio-lag-alert-watchdog` (#1226), `bundle-state-alert-watchdog` (#732) and
`network-reach-alert-watchdog` (#1001).

## DETECTION ONLY — no auto-action (unlike bundle-state's auto-restart)

The cure is **`schtasks /run /tn StartVBMatrix`** on the box (start VB-Matrix into the interactive
`newlevel` session) — an **owner/supervisor step**, not a headless-timer action (a dev1 timer also
has no session-aware win-* MCP to drive a GUI app). So this watchdog is **alert-only**: on a
confirmed down it pages (throttled), and **recovery is log-only / machine-channel** (never a phone
ping — `.claude/rules/watchdog-notify-dedup.md` #1206; the alert carries a stable `--dedup-key
vb-matrix-<box>` so a persistent outage EDITS the one card instead of re-pinging).

## Separation of concerns — it never false-pages, and never double-pages a down box

The **only** page condition is a SUCCESSFULLY-FETCHED `running="0"` reading, so:

- A box whose `:8899` is not fetchable this pass (box down, or `:8899` down) classifies **SKIP** —
  deferred to `bundle-state-alert-watchdog` (#732) / `network-reach-alert-watchdog` (#1001). No
  VB-Matrix page for a down box, no duplicate.
- A box that is up but reports **no `vb_matrix_running` facet** (a box with no VB-Matrix install,
  e.g. imag, or an old bundle-state-server not serving the facet yet) classifies **UNKNOWN** — no
  reading to judge, no page (never a false negative on a non-VB-Matrix box).
- Because a page requires a fetched positive reading, a **dev1-side path outage** makes every fetch
  fail → SKIP → no page. So this watchdog needs **no** reference-anchor/outage guard (unlike
  bundle-state #732, which restarts tasks and pages on a DOWN box and therefore does need one).

## It ships DISABLED by default — on purpose

These units are committed but **NOT installed and NOT enabled** by this repo / this PR. Before it
runs unattended, the **SUPERVISOR** installs it, live-verifies it (below), and only then enables the
timer. No box-side change is made by this ticket.

## Conservative gates (why it won't thrash or spam)

- Requires **2 consecutive confirmations** (`VB_MATRIX_CONFIRM_THRESHOLD`, default 2) before it
  alerts — one blipped reading is observe-only. A RUNNING read resets the counter; a SKIP/UNKNOWN
  pass HOLDS the counter (an unmeasured pass neither advances nor resets it).
- Repeat alerts are throttled to once every `VB_MATRIX_ALERT_THROTTLE_PASSES` passes (default 12 ≈
  1h) while the same box stays down.
- A recovery ("running again") line is logged once (machine-channel) when a box we paged for returns
  to running.

## Supervisor install + live-verify procedure

```bash
# 1. Dry-run a single pass — fetch + decide + LOG only, NEVER alert:
scripts/vb-matrix-alert-watchdog.sh --dry-run           # inspect the per-box verdict + decision

# 2. Install the --user units (dev1):
mkdir -p ~/.config/systemd/user
cp systemd/vb-matrix-alert-watchdog.service ~/.config/systemd/user/
cp systemd/vb-matrix-alert-watchdog.timer   ~/.config/systemd/user/
systemctl --user daemon-reload

# 3. Live-verify BEFORE enabling the timer:
#    a) with VB-Matrix running on both boxes -> a manual pass must report RUNNING, take NO action:
systemctl --user start vb-matrix-alert-watchdog.service ; journalctl --user -u vb-matrix-alert-watchdog -n 50
#    b) simulate a down VB-Matrix: serve a crafted bundle-state (vb_matrix_running="0"), confirm two
#       consecutive passes page with the correct box, then confirm the recovery line is logged
#       (machine-channel) once it reads running again.

# 4. Only after both checks pass, enable the recurring timer:
systemctl --user enable --now vb-matrix-alert-watchdog.timer
systemctl --user list-timers | grep vb-matrix-alert-watchdog

# Disable later:
systemctl --user disable --now vb-matrix-alert-watchdog.timer
```

## Tunables (env, override in the unit or environment.d)

| Var | Default | Meaning |
|---|---|---|
| `VB_MATRIX_CONFIRM_THRESHOLD` | `2` | consecutive DOWN readings before paging |
| `VB_MATRIX_ALERT_THROTTLE_PASSES` | `12` | passes between repeat alerts (≈1h at the 5-min cadence) |
| `VB_MATRIX_BOXES` | `strih\|10.77.9.202 stream\|10.77.9.204` | `name\|ip` pairs to watch |
| `VB_MATRIX_CURL_TIMEOUT` | `10` | `:8899` HTTP fetch timeout (s) |
| `VB_MATRIX_ALERT_STATE_FILE` | `$XDG_RUNTIME_DIR/camera-box-vb-matrix-alert.state` | per-box confirm/throttle/recovery state |

## What this does NOT do

- It makes **no box-side change** and takes **no auto-action** — the cure (`schtasks /run /tn
  StartVBMatrix`, and the durable fix of adding an AtLogon trigger to that task) is an owner/
  supervisor step; this watchdog only detects and alerts.
- It does not diagnose **why** VB-Matrix stopped — it detects that the process is absent while its
  install is present. The durable fix (an AtLogon trigger on `StartVBMatrix`, and checking that
  task's `ExecutionTimeLimit` is not a default 3-day cap that would kill VB-Matrix after 72 h) is
  tracked as a supervisor/owner step on #1227.
