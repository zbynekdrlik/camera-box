# bundle-state-alert-watchdog — install note (#732)

The dev1-side active health-check watchdog (`scripts/bundle-state-alert-watchdog.sh`) is the fix for
#732: the strih/stream `BundleStateServer` Scheduled Task (`:8899/bundle-state.json`, the
version-integrity E2E gate's input) has died and stayed dead **four documented times**
(2026-07-12/13, 2026-07-30, 2026-08-10 — silent for **3 days**, 2026-08-13 — twice in one
afternoon). Its `SCHED_S_TASK_TERMINATED` (`0x40010004`) death class is an informational/SUCCESS
result, so Windows Task Scheduler's restart-on-failure (`RestartCount=999`) never engages; a
cold-start-after-reboot can also simply never fire. A passive Task-Scheduler policy can never cover a
non-failure termination — this needs an ACTIVE external prober.

It polls both boxes' `:8899/bundle-state.json` from a **dev1 systemd --user timer** with `curl` (HTTP
200 + a JSON body — catches a wedged-but-listening server too, and is the method the ops-SKILL note
mandates: an MCP-side `Invoke-WebRequest` hangs even when the server logs a prompt 200). It is the
`:8899`-service sibling of `obs-liveness-watchdog` (#391) and `network-reach-alert-watchdog` (#1001).

## Auto-restart design decision — DETECT + ALERT **and** AUTO-RESTART (unlike obs-liveness)

`obs-liveness-watchdog` is deliberately alert-only from dev1 because OBS recovery needs a GUI
relaunch a headless timer cannot safely drive. This watchdog is different on both counts, so it
**auto-restarts**:

1. The remedy is a pure **`schtasks /run /tn BundleStateServer`** over ssh — **session-agnostic**
   (verified live: the task action is `powershell … -WindowStyle Hidden -File
   run-bundle-state-server.ps1`, a HIDDEN, headless HTTP-server supervisor loop; `Logon Mode:
   Interactive only`). Starting a hidden background task from ssh is exactly the sanctioned headless
   case in `.claude/rules/win-ssh-vs-mcp.md` — NEVER the `/it` interactive form (a documented DEAD
   END on these boxes, and a desktop-session op a headless timer must never issue).
2. The bundle-state server is **pure infra that is never deliberately stopped**, so auto-restarting
   it can never fight the operator (contrast the #788 "watchdog fought the operator" OBS lesson).
3. **Graceful degradation:** if ssh creds are absent or the restart fails, the Discord **alert still
   fires** (primary value preserved). Set `BUNDLE_STATE_AUTO_RESTART=0` for pure alert-only.

## Separation of concerns — it does NOT double-page a fully-dead box

A box that is UP (ping OR OBS-WS `:4455`) but whose `:8899` does not serve is this watchdog's job. A
box that is FULLY unreachable (ping + `:4455` + `:8899` all down) yields `BOX_UNREACHABLE` and is
**deferred to `network-reach-alert-watchdog` (#1001)** — no pointless restart against a dark box, no
duplicate page. A dev1-side-outage anchor (no reference rig node — cam1/cam2/imag-nb — AND no watched
box reachable) makes the whole pass "nothing to decide", so a dev1 uplink flap never false-acts.

## It ships DISABLED by default — on purpose

These units are committed but **NOT installed and NOT enabled** by this repo / this PR. Before it
runs unattended, the **SUPERVISOR** installs it, live-verifies it, and only then enables the timer.
No Windows-side change is made by this ticket.

## Conservative gates (why it won't thrash or spam)

- Requires **2 consecutive confirmations** (`BUNDLE_STATE_CONFIRM_THRESHOLD`, default 2) before it
  restarts OR alerts — one slow/blipped probe is observe-only. A HEALTHY read resets the counter.
- Once confirmed, the restart is attempted **every** 5-min pass until it recovers (idempotent —
  `schtasks /run` on an already-running task is a Task-Scheduler no-op), while repeat **alerts** are
  throttled to once every `BUNDLE_STATE_ALERT_THROTTLE_PASSES` passes (default 12 ≈ 1h).
- A recovery ("serving again") ping fires once when a box we paged for returns.

## Supervisor install + live-verify procedure

```bash
# 1. Dry-run a single pass — measure + decide + LOG only, NEVER restart/alert:
scripts/bundle-state-alert-watchdog.sh --dry-run          # inspect the per-box verdict + decision

# 2. Install the --user units (dev1):
mkdir -p ~/.config/systemd/user
cp systemd/bundle-state-alert-watchdog.service ~/.config/systemd/user/
cp systemd/bundle-state-alert-watchdog.timer   ~/.config/systemd/user/
# ssh creds default to newlevel/newlevel (targets.md). Override out-of-band ONLY if they differ:
#   mkdir -p ~/.config/environment.d
#   printf 'STRIH_PW=...\nSTREAM_PW=...\n' > ~/.config/environment.d/bundle-state-alert-watchdog.conf
systemctl --user daemon-reload

# 3. Live-verify BEFORE enabling the timer:
#    a) with BOTH boxes healthy -> a manual pass must report HEALTHY and take NO action:
systemctl --user start bundle-state-alert-watchdog.service ; journalctl --user -u bundle-state-alert-watchdog -n 50
#    b) simulate a :8899 death (on a box: `schtasks /end /tn BundleStateServer` + stop the python) ->
#       two consecutive passes must auto-restart it (:8899 back to 200) and, if it does not recover,
#       alert with the correct box + signal breakdown. Then confirm the recovery ("serving again")
#       ping fires once it is back.

# 4. Only after both checks pass, enable the recurring timer:
systemctl --user enable --now bundle-state-alert-watchdog.timer
systemctl --user list-timers | grep bundle-state-alert-watchdog

# Disable later:
systemctl --user disable --now bundle-state-alert-watchdog.timer
```

## Tunables (env, override in the unit or environment.d)

| Var | Default | Meaning |
|---|---|---|
| `BUNDLE_STATE_AUTO_RESTART` | `1` | `1` = auto-restart on confirmed down; `0` = alert-only |
| `BUNDLE_STATE_CONFIRM_THRESHOLD` | `2` | consecutive DOWN readings before acting |
| `BUNDLE_STATE_ALERT_THROTTLE_PASSES` | `12` | passes between repeat alerts (≈1h at the 5-min cadence) |
| `BUNDLE_STATE_BOXES` | `strih\|10.77.9.202 stream\|10.77.9.204` | `name\|ip` pairs to watch |
| `BUNDLE_STATE_REFERENCE_HOSTS` | `10.77.9.61 10.77.9.62 10.77.9.182` | cam1/cam2/imag-nb — the dev1-side-outage anchor |
| `BUNDLE_STATE_CURL_TIMEOUT` | `10` | `:8899` HTTP fetch timeout (s) — server seen answering in ~6.6 s |
| `BUNDLE_STATE_SSH_TIMEOUT` | `25` | whole `schtasks /run` ssh restart timeout (s) |
| `STRIH_PW` / `STREAM_PW` | `newlevel` | per-box ssh password (targets.md; NOT committed if overridden) |
| `STRIH_USER` / `STREAM_USER` | `newlevel` | per-box ssh user |
| `BUNDLE_STATE_ALERT_STATE_FILE` | `$XDG_RUNTIME_DIR/camera-box-bundle-state-alert.state` | per-box confirm/throttle/recovery state |

## What this does NOT do

- It makes **no Windows-side change** and creates/edits no Scheduled Task — it only *invokes*
  `schtasks /run` on the existing `BundleStateServer` task. The deeper "recreate the task from a
  committed setup script + re-apply the RestartCount hardening on re-provision" work (there is still
  no repo script that CREATES the task, per #650's history) is separate and out of scope here.
- It does not detect **stale deployed code** (a healthy task serving an out-of-date
  `bundle-state-server.py` payload) — that is the separate deploy-drift concern in
  `.claude/skills/ops` / `rig-standing-services.md`.
