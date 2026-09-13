# dantesync-clock-alert-watchdog — install note (#1307)

The dev1-side alert watchdog (`scripts/dantesync-clock-alert-watchdog.sh`) closes the detection gap
behind the **2026-09-13** silent fleet-clock failure: the Yamaha AIC128-D PTP grandmaster's DHCP
lease moved off `10.77.9.184`, every node's `gm_allowlist` stopped matching, the **whole fleet**
silently ran NTP-only for hours (`mode=ACQ is_locked=false`; strih as NTP master stepping 165×/h)
and **nobody noticed** until the release E2E gate failed. The camboxes cam1–7 are **HEADLESS** (no
desktop, no operator) — their only path to a human is **dev1 → Discord**. dantesync#114 adds a
per-box LOUD local WARN; this watchdog is the dev1 half that turns that into a phone page.

Owner ruling (verbatim, ROZHODNUTÉ on #1307, 2026-09-13): *„aj ntp aj ostatne veci bez ktorych nevie
produkcia bezat spravne musi notifikovat … nech kazdu minutu chodia notifikacie ze nemaju dante
clock … byt o tom dokolecka notifikovany"*. So this is a **production-critical** watchdog: while a
fault persists it **re-pings repeatedly**, not once per incident (see the time-bucketed dedup key
below). It is the dante-clock sibling of `audio-lag-alert-watchdog` (#1226), `bundle-state-alert-
watchdog` (#732) and `network-reach-alert-watchdog` (#1001), and is the first member of the
production-critical class tracked under the umbrella **#1308**.

It reads each dantesync node's **`http://<ip>:8898/status`** directly (dantesync#47's network status
endpoint, the SAME signal the E2E gate `scripts/dantesync-gate.sh` grades), with `curl`, from a
**dev1 systemd --user timer** at a **60 s** cadence. The per-node verdict is decided by the PURE
`scripts/dantesync_clock_decision.py` (Tier-0 pytest, the #1199 python-mirror pattern), mirroring
`scripts/clock-offset-guard.sh`'s field semantics (`is_locked` + `mode` in NANO/LOCK,
`gm_source_ip` vs the rig grandmaster, `ntp_step_storm`).

## What it pages on

| Condition | Reason class | Notes |
|---|---|---|
| node reachable but not PTP-locked | `not_locked` | `is_locked` false OR `mode` ∉ {NANO, LOCK} |
| node locked to a **foreign** grandmaster | `wrong_gm` | `gm_source_ip` present and ≠ the resolved grandmaster (#834 class) |
| node in an NTP **step-storm** | `storm` | dantesync's own `ntp_step_storm=true` (its 120/h alarm — NOT a re-hardcoded camera-box literal); `ntp_steps_last_hour` carried in the reason |
| dantesync#114 clock alarm | `clock_alarm` | forward-compat: `clock_alarm.active` is authoritative when present; the derived checks fold in as the cross-check |
| grandmaster DNS won't resolve | `dante-clock-dns` | `rig_grandmaster_ip` fails on `video-clock.lan` — the exact silent-failure class the owner banned |
| grandmaster IP **moved** between passes | `dante-clock-gm-change` | persisted last IP → a change pages "A → B" (today's „ip sa zmenila"), even if every node follows it |

**UNREACHABLE = SKIP**: a node whose `:8898/status` is not fetchable this pass is deferred to the
`network-reach-alert-watchdog` (#1001) — an OFF box (e.g. a retired-but-powered cam that got
unplugged) reads UNREACHABLE and **never** pages. **UNKNOWN**: a reachable node whose payload carries
none of the clock fields (a non-dantesync answer / partial payload) is held, never a fabricated page.

## Production-critical time-bucketed re-ping (owner ruling — the one deviation from #1206)

Every page uses `airuleset.py notify --dedup-key`, but the key is **time-bucketed**:
`dante-clock-<box>-<floor(now/REPING_INTERVAL_S)>` (`REPING_INTERVAL_S` default **600 s**, floored
at 60 s). Within one bucket an identical state **edits** the card (no re-ping); every new bucket is a
**fresh ping** while the fault persists. This is the deliberate exception to
`.claude/rules/watchdog-notify-dedup.md`'s one-ping-per-incident rule, **for this
production-critical class only** (documented there + allowlisted in
`tests/python/test_notify_dedup_key_sweep_1206.py`). **Recovery is machine-channel / log-only**
(never a phone ping), unchanged.

## Node roster (single sources of truth, no second literals)

- **cam1–7** — resolved via `scripts/camera-set.sh`'s `camera_resolve` (the fleet IP map). The
  default roster is the **whole powered** cam fleet cam1–7, **not** `CAMERA_ACTIVE_SET` — cam5–7 are
  retired from the active set but are still powered and still running dantesync, so they can silently
  lose the clock too.
- **strih / stream / imag / resolume** — resolved via `scripts/lib/obs-fleet.sh`'s `obs_fleet_host`.
  **resolume** is the traveling CG box: paged **only** while `obs_fleet_is_home resolume` holds;
  away → skipped entirely (never a false page against a box that is simply not here).

## It ships DISABLED by default — on purpose

These units are committed but **NOT installed and NOT enabled** by this repo / this PR. Before it
runs unattended, the **SUPERVISOR** installs it, live-verifies it (below), and only then enables the
timer. No box-side change is made by this ticket.

## Supervisor install + live-verify procedure

```bash
# 1. Dry-run a single pass — fetch + decide + LOG only, NEVER alert (needs the rig reachable):
scripts/dantesync-clock-alert-watchdog.sh --dry-run       # inspect the per-node verdict + decisions

# 2. Install the --user units (dev1):
mkdir -p ~/.config/systemd/user
cp systemd/dantesync-clock-alert-watchdog.service ~/.config/systemd/user/
cp systemd/dantesync-clock-alert-watchdog.timer   ~/.config/systemd/user/
systemctl --user daemon-reload

# 3. Live-verify BEFORE enabling the timer:
#    a) with the fleet healthy -> a manual pass reports OK for every reachable node, no action:
systemctl --user start dantesync-clock-alert-watchdog.service
journalctl --user -u dantesync-clock-alert-watchdog -n 80
#    b) simulate a loss with the fetch seam against a crafted status (no live box touched):
DANTE_CLOCK_FETCH_CMD=/path/to/stub DANTE_CLOCK_NODES="cam1|10.77.9.61|always" \
  scripts/dantesync-clock-alert-watchdog.sh --dry-run   # confirm two passes -> a bucketed alert

# 4. Only after both checks pass, enable the recurring timer:
systemctl --user enable --now dantesync-clock-alert-watchdog.timer
systemctl --user list-timers | grep dantesync-clock-alert-watchdog

# Disable later:
systemctl --user disable --now dantesync-clock-alert-watchdog.timer
```

Note: on dev1 the systemd `--user` hardening directives are live-verified INERT no-ops under the
box's unprivileged-userns kernel policy (`.claude/rules/dev1-systemd-user-unit-hardening.md`), so
none are declared here — the sibling units carry none either.

## Tunables (env, override in the unit or environment.d)

| Var | Default | Meaning |
|---|---|---|
| `DANTE_CLOCK_REPING_INTERVAL_S` | `600` | re-ping bucket size (s); floored at 60 in the pure module |
| `DANTE_CLOCK_CONFIRM_THRESHOLD` | `2` | consecutive fault readings before the first page |
| `DANTE_CLOCK_CAM_NODES` | `cam1 … cam7` | cam names to watch (resolved via camera-set.sh) |
| `DANTE_CLOCK_OBS_NODES` | `strih stream imag resolume` | OBS-box names (resolved via obs-fleet.sh) |
| `DANTE_CLOCK_NODES` | *(unset)* | full `name\|ip[\|homegate]` roster override (wins over the two above) |
| `DANTE_CLOCK_STATUS_PORT` | `8898` | dantesync network status port |
| `DANTE_CLOCK_CURL_TIMEOUT` | `10` | `:8898` HTTP fetch timeout (s) |
| `DANTE_CLOCK_FETCH_CMD` | *(unset)* | Tier-0 seam: `<cmd> <ip>` stdout replaces the curl fetch |
| `RIG_GRANDMASTER_IP` / `RIG_GRANDMASTER_HOST` | *(unset)* / `video-clock.lan` | grandmaster resolution (rig-grandmaster.sh) |
| `DANTE_CLOCK_ALERT_STATE_FILE` | `$XDG_RUNTIME_DIR/camera-box-dantesync-clock-alert.state` | per-key confirm/latch/last-gm state |

## What this does NOT do

- It makes **no box-side change** and takes **no auto-action** — the cure (fix the grandmaster
  DNS/DHCP, restart dantesync, re-provision an allowlist) is a rig-ops call; this watchdog detects
  and alerts.
- It does not replace dantesync#114's **on-box** per-minute LOUD local notification — that is the
  headless-box-local half; this is the dev1 → Discord half.
- The broader production-critical class (NTP master, dantesync alive/version, genlock LOCK, an audit
  of the rest of the dev1 watchdog family) is tracked under the umbrella **#1308**.
