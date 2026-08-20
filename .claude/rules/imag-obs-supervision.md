---
paths:
  - "scripts/imag-obs-start.sh"
  - "scripts/imag-obs-stop.sh"
  - "scripts/imag_scenes.py"
  - "scripts/imag-wallpaper-refresh.sh"
  - "scripts/imag-obs-alert-watchdog.sh"
  - "scripts/lib/imag-obs-reachability.sh"
  - "scripts/lib/imag-obs-restart-storm.sh"
  - "scripts/setup-imag.sh"
  - "scripts/lib/obs-watchdog-decision.sh"
  - "systemd/imag-obs*"
  - "systemd/imag-wallpaper-refresh*"
---

# imag-nb OBS runtime supervision + alerting (#882)

Runtime architecture for keeping imag-nb's OBS process alive and audible when it isn't — distinct
from `.claude/rules/imag-nb-provisioning.md` (install/provisioning steps for a fresh box).

## imag-nb has no airuleset checkout and no Discord credentials — alerting MUST run from dev1

**Live-confirmed (2026-07-30, #882):** a first design tried to fire `airuleset.py notify` directly
from `scripts/imag-wallpaper-refresh.sh` (which runs ON imag-nb via a systemd --user timer). It
failed every time — `airuleset.py notify failed (non-fatal)` — because imag-nb is a remote
appliance box with no `~/devel/airuleset` checkout and no Discord webhook/token anywhere on it.

**The fix, and the pattern for any future imag-nb alert:** detection stays ON-BOX (cheap, no
credentials needed — `pgrep -x obs`, a log line), but the actual alert fires from a **DEV1-side**
systemd --user timer that polls the box over SSH and calls `airuleset.py notify` locally on dev1
(where the checkout + credentials live). This mirrors `scripts/obs-liveness-watchdog.sh`'s own
#391 topology exactly (a dev1 timer polling strih/stream over OBS WebSocket) — `scripts/
imag-obs-alert-watchdog.sh` is the same shape, just polling imag-nb over SSH + the reachability
probe below instead of OBS WebSocket `GetStats`. **Never design an alert path that assumes a
remote appliance box can fire its own Discord notification** — check whether the box has the
airuleset checkout before assuming `python3 $NOTIFY notify` will work there at all.

## A standalone script deployed to imag-nb is a single flat file — sourcing a sibling `scripts/lib/*.sh` breaks unless you ALSO deploy that lib file

`setup-imag.sh` fetches scripts like `imag-obs-start.sh`/`imag_scenes.py` onto the box at a FIXED
path (`/usr/local/bin/...`) with **no** `scripts/lib/` directory alongside them — confirmed by the
step-12 comment in `setup-imag.sh` ("this script is copied to the box standalone... no sibling
scripts/... files exist here at runtime"). **Live-confirmed (2026-07-30):** adding `. "$HERE/lib/
obs-watchdog-decision.sh"` to `imag-wallpaper-refresh.sh` (with `HERE` resolving to `/usr/local/
bin` on the deployed box) failed with `No such file or directory` the first time it was deployed —
the lib simply wasn't there. If a script destined for standalone on-box deployment needs a shared
lib, either (a) deploy that lib file too (a new `/usr/local/bin/lib/` dir, `chmod 644`), wiring the
fetch into `setup-imag.sh`'s provisioning, or (b) don't source it at all — keep the on-box script's
own logic self-contained and put the SHARED decision logic in a DEV1-side script instead (the
`imag-obs-alert-watchdog.sh` design above sidesteps this entirely: it runs on dev1, which has the
full repo checkout, so sourcing `scripts/lib/*.sh` there just works).

## systemd `Restart=on-failure` (never `always`) + route every deliberate stop through `systemctl stop`

The historical `imag-obs-watchdog.py` (stood down 2026-07-16, issue 788) fought the operator: it
treated ANY dead OBS process — including a deliberate manual quit — as a crash and relaunched it,
producing "auto-relaunch loops + false crashed alarms". The `imag-obs.service` unit (#882) avoids
this with two coupled pieces, BOTH required:

1. **`Restart=on-failure`, never `Restart=always`.** `on-failure` restarts only on an abnormal
   death (non-zero exit, or killed by a signal — a segfault is `code=exited, status=139`, i.e.
   `128+SIGSEGV`) — a clean `exit(0)` is left alone. `Restart=always` restarts on EVERY exit
   including a clean one, which is exactly the operator-fighting shape.
2. **Every deliberate stop must go through `systemctl --user stop`, never a raw `pkill`/`kill`.**
   systemd only suppresses `Restart=` for a stop IT initiated. An external kill of the tracked
   process — even a "graceful" `wmctrl -c` close followed by `pkill -TERM` — looks to systemd like
   an unexpected crash and STILL triggers `Restart=on-failure`, reintroducing the exact issue-788
   bug for any caller that bypasses systemctl. `imag-obs-stop.sh` therefore checks
   `systemctl --user is-active --quiet imag-obs.service` FIRST and delegates to
   `systemctl --user stop imag-obs.service` when the unit is active — its own graceful-close/
   SIGTERM/SIGKILL ladder becomes the unit's `ExecStop=` handler (invoked with `--exec-stop`,
   which skips the delegation check to avoid recursing back into `systemctl stop` mid-stop).

## Invoking the unit over ssh — it is a USER unit (issue 998 deploy, 2026-08-06)

`imag-obs.service` is a **user** unit, not a system one. Over ssh, `sudo systemctl start imag-obs`
fails with `Unit imag-obs.service not found` — the system manager has never heard of it. The
working invocation from a plain ssh session:

```bash
export XDG_RUNTIME_DIR=/run/user/$(id -u)
systemctl --user start imag-obs   # / stop / restart / is-active
```

(without the `XDG_RUNTIME_DIR` export a non-graphical ssh session can't reach the user bus:
`Failed to connect to bus`). And NEVER start obs directly with `setsid`/`nohup` "to get it up
quickly" — that puts it OUTSIDE systemd supervision entirely (no `Restart=on-failure`, `ExecStop=`
ladder bypassed), so a later `systemctl --user stop` won't own it. Kill any such stray and restart
through the unit.

## Making a BACKGROUNDED child the unit's tracked "main process" (`Type=simple`)

`imag-obs-start.sh` backgrounds `obs` with `&` and does post-launch setup (seed scenes, open
projectors) afterward — if the wrapper script then just exits, systemd (`Type=simple`) sees the
WRAPPER as the main process, and a later crash of the backgrounded `obs` is invisible to it (no
`Restart=` fires). The fix: capture the PID right after backgrounding (`OBS_PID=$!`), do the
existing post-launch steps unchanged, then at the very end **`wait "$OBS_PID"`** and
**`exit "$OBS_EXIT"`** (propagating obs's own exit status). The wrapper now blocks for as long as
obs runs, so systemd's tracked process genuinely reflects obs's lifetime — `Restart=on-failure`
fires the instant `wait` returns non-zero from an abnormal death.

## Live end-to-end proof (2026-07-30, on the real 10.77.9.182 box)

`kill -SEGV <obs_pid>` → `systemd-coredump` captured a real 88 MB core
(`coredumpctl list` showed `SIGSEGV present /usr/bin/obs`) → `journalctl --user -u imag-obs.service`
showed `Main process exited, code=exited, status=139/n/a` then `Scheduled restart job, restart
counter is at 1` → a fresh `obs` process was up and re-seeded within ~2 seconds. Immediately after,
running `imag-obs-stop.sh` (no flags) correctly delegated to `systemctl --user stop` and OBS
stayed down — no auto-relaunch fight. Re-triggering `systemctl --user start imag-obs.service` while
already active is a safe no-op (still exactly one `obs` process) — confirms the openbox autostart's
one-time hand-switch to `systemctl --user start imag-obs.service` (in place of calling
`imag-obs-start.sh` directly) cannot race two OBS instances at boot.

**Core dumps require BOTH `LimitCORE=infinity` on the unit AND `systemd-coredump` installed** — the
kernel's own `core_pattern` on a fresh box is a bare, non-piped `core` (writes to CWD, which a
systemd unit usually can't write to), not the piped `|/usr/lib/systemd/systemd-coredump ...` form;
installing the package rewrites `core_pattern` to the piped form automatically.

## A CORRECT unit can still sit unsupervised — the RECOVERY INSTRUCTIONS themselves must never offer the direct-launch bypass as the primary path (#1015, 2026-08-13)

The unit above genuinely works when actually invoked through systemctl — that was never the
defect. What went wrong live: `scripts/lib/imag-obs-reachability.sh`'s `imag_obs_reachability_message()`
(what `recording-e2e.sh`'s preflight prints on a dead-OBS failure) told the operator/agent to run
`/usr/local/bin/imag-obs-start.sh` DIRECTLY as its PRIMARY instruction, with the supervised
`systemctl --user start imag-obs` form mentioned only as a parenthetical "once supervised" aside.
Every actual recovery followed the primary instruction, which launches OBS entirely outside the
unit's cgroup — so `Restart=on-failure` had nothing to supervise, for weeks, on a box whose
provisioning had installed the unit correctly the whole time. Fixed by leading with the systemctl
command and explicitly warning against the direct call.

**The general lesson: correctly provisioning something (installing+enabling a supervision unit)
and correctly DOCUMENTING how to recover it are two separate claims — verify both.** A "this box's
supervision is broken" report is worth checking the unit's OWN state first (`systemctl --user
is-enabled`/`is-active`, per the section above), but is equally worth checking every recovery path
a human or an automated preflight would actually be told to follow — an accurate unit with an
inaccurate (or merely de-prioritized) recovery instruction produces the identical unsupervised
symptom on the ground. `scripts/verify-imag.sh`'s acceptance gate now additionally reads the LIVE
obs PID's own `/proc/<pid>/cgroup` and requires an `imag-obs.service` path component — proof the
RUNNING process is the supervised one, independent of what systemd's own is-enabled/is-active
bookkeeping claims (that bookkeeping can be entirely correct while the actual running process,
launched by a bypassed recovery instruction, sits outside the unit).

## Anything run on the imag OBS START path (`imag_scenes.py --bootstrap`) must NEVER be able to abort — a SystemExit / uncaught exception there restart-LOOPS OBS on the live projection (#866)

`imag-obs-start.sh` is `imag-obs.service`'s `ExecStart`, runs under `set -euo pipefail`, and calls
`python3 /usr/local/bin/imag_scenes.py --bootstrap` on every fresh OBS instance (boot autostart,
operator "Spustit OBS", and the systemd `Restart=on-failure` relaunch). So a bootstrap step that
`sys.exit()`s or lets a WebSocket error (a #328 socket-timeout/closed-connection RAISE, not just an
error-result) propagate uncaught → `imag_scenes.py` exits non-zero → `set -e` aborts the start
script → systemd sees ExecStart fail → `Restart=on-failure` relaunches → the SAME transient fails
again → **OBS flaps up/down forever on the LIVE IMAG projection**, the worst possible outcome (worse
than the thing the step was trying to fix). Any WS-touching bootstrap addition must be **best-effort:
wrap the whole WS body in `try/except Exception` → print a LOUD warning (captured to
`/tmp/imag-obs-start.log`) → return**, leaving OBS up. Mirror `obs_burn_filter.py::_all_ndi_inputs`'
own try/except. This is the imag-start counterpart of the #788 "watchdog fought the operator" lesson
already in this rule: never let a self-heal/enforcement step take the live box down.

## A measurement burn (`genlock_burn`) PERSISTS to disk in the saved scene collection — it resurrects ON after ANY OBS restart, so force it OFF at OBS START, not just at gate cleanup (#866)

The per-source `genlock_burn` bool is written into OBS's saved scene collection
(`~/.config/obs-studio/basic/scenes/*.json`); turning it OFF is only ever a RUNTIME WebSocket change
(the gate cleanup / `obs_burn_filter.py remove`), and OBS writes the collection to disk only on a
clean exit. So an OBS crash/reboot/manual restart reloads `genlock_burn=true` and RENDERS the QR
measurement burn onto the live IMAG projection. The `[0/8]` exhaustive `sweep-off` only runs during a
dev1 gate run — NOT on the box's own restart — so it does not cover the unattended-restart window.
`imag_scenes.py`'s `clear_measurement_burns()` (called on the `--bootstrap` path) forces every
`ndi_source` input's burn OFF at every fresh instance (enumerated from `GetInputList`, never a
static/CAMS list — the burn-target-enumeration rule). A burn is never legitimate operator state, so
clearing it at bootstrap never fights the operator (unlike the #785/#783 bindings/transforms, which
this same file's `seed()` deliberately preserves). The general reflex: any state that OBS PERSISTS
and that a measurement/gate turns on at runtime must be reset at OBS START, or it survives the next
restart. **Follow-up (#1057):** strih (Windows OBS) has the identical resurrection on a different
start path, and the broader "verify whole runtime state at start + report drift LOUD" (burns OFF +
latency pins) is filed separately — not solved here.

## The dev1-side ALERT path must distinguish a DELIBERATE operator quit from a crash — read the UNIT STATE, not the OBS log (#788)

The RELAUNCH half of #788 is solved by `imag-obs.service Restart=on-failure` above. The ALERT half
(the dev1-side `scripts/imag-obs-alert-watchdog.sh`) had a residual: it fired "OBS is DOWN" on ANY
`OBS_PROCESS_ABSENT` with no crash-vs-quit discrimination, so an operator quitting OBS on purpose
(to test latency) still paged the crew. The AUTHORITATIVE discriminator is systemd's OWN
`Restart=on-failure` verdict — **read the unit state, do NOT parse the OBS log** (the ticket's
original log-marker idea predates the #882 systemd model and is fragile/redundant beside it):

- A clean quit / operator `systemctl --user stop` → `LoadState=loaded ActiveState=inactive
  Result=success`. A crash-loop that exhausted `Restart=` → `ActiveState=failed`. A restart in
  progress → `activating`/`exit-code`. So `loaded + inactive + success` == deliberate → SUPPRESS;
  everything else → ALARM (fail-safe: "bez clean markera = pád → alarm").
- **LIVE-CONFIRMED systemd quirk — you MUST require `LoadState=loaded`:** `systemctl --user show
  <not-found>.service --property=LoadState,ActiveState,Result` returns `LoadState=not-found
  ActiveState=inactive Result=success` for a unit that does not exist. Without the `loaded` check, a
  missing/uninstalled unit would be misread as a clean quit and a genuinely-down OBS would never
  alarm.
- **Reading the USER unit over a non-login ssh needs `export XDG_RUNTIME_DIR=/run/user/$(id -u)`**
  first (issue 998), or `systemctl --user` can't reach the user bus.

Implementation shape (reused, not a new mechanism): two PURE seams in
`scripts/lib/imag-obs-reachability.sh` — `imag_obs_deliberate_down_probe_cmd [pause_file]
[pause_window_s]` (a remote-snippet builder, same always-exit-0 pattern as
`imag_obs_reachability_probe_cmd`) and `imag_obs_down_is_deliberate` (a pure token classifier;
`while-read` over a herestring so it is safe under the sourced libs' `set -euo pipefail` — a `sed |
head` would SIGPIPE-abort under pipefail). The watchdog does a SECOND ssh only on
`OBS_PROCESS_ABSENT` (an `OBS_PORT_NOT_LISTENING` = process up, never a quit → still alarms); on
deliberate it clears the full alert throttle (confirm/alert_sig/alert_passes, symmetric with the
healthy-pass branch) and returns without paging. A time-bounded operator override file
(`/tmp/imag-watchdog-pause`, default 60 min from its mtime, so a forgotten pause can never mask a
real crash forever) is the explicit escape hatch. The `imag_obs_reachability_probe_cmd`/`_message`
functions the [0/8] preflight uses are UNTOUCHED — the preflight legitimately fails on any
OBS-absent, deliberate or not; the discriminator is ONLY for the alert path.

## An imported Python sibling must ride the SAME setup-imag.sh install list as its importer, or a deploy seed-loops OBS (#1156)

The flat-file deploy gotcha above ("a standalone on-box script that sources a sibling `scripts/lib/*.sh` breaks unless you ALSO deploy the lib") has a Python twin. `setup-imag.sh` fetches on-box python by an EXPLICIT `gh api …contents/scripts/<file>.py?ref=dev` list — it does NOT walk imports. So when a lane adds `import <sibling>` to `imag_scenes.py` (the #1143 lane added `import imag_record_encoder`) WITHOUT adding the sibling to setup-imag.sh's fetch list, a deploy pushes the importer WITHOUT the imported module → every `imag-obs-start.sh` seed dies on `ModuleNotFoundError` → `imag-obs.service` `Restart=on-failure` relaunches the whole cgroup (OBS included) → an unbounded crash-loop (the live incident: 1737 restarts / 8.5 h, zero alert).

- **Rule:** any NEW `import <local sibling>` in an on-box-installed python (`imag_scenes.py`, `imag-obs-watchdog.py`, …) MUST get a matching `<VAR>="/usr/local/bin/<sibling>.py"` + `gh api …contents/scripts/<sibling>.py?ref=dev` + `chmod 755` block in setup-imag.sh, ALONGSIDE the importer's own block. Audit the whole chain before assuming one block is enough: `grep -nE '^import|^from|import imag' scripts/imag_scenes.py` AND the sibling itself (imag_record_encoder.py imports only stdlib, so the chain ends there).
- **Fail-fast guard (imag-obs-start.sh):** an `import` preflight — `python3 -c "import sys; sys.path.insert(0,'/usr/local/bin'); import imag_scenes"` — runs BEFORE the `taskset … obs … &` launch (and after the idempotent `pgrep -x obs` early-exit). A missing sibling then fails the unit cleanly (named `FAIL: imag_scenes import preflight` + `exit 1`) instead of launching-then-seed-failing, which is what loops a HEALTHY OBS 1700×. `import imag_scenes` is side-effect-free (it has an `if __name__=="__main__"` guard; `create_connection` is only called inside a method), so the preflight is cheap and also transitively validates the sibling + the `websocket` dep.

## dev1-side restart-STORM alert for imag-obs.service (#1156)

`scripts/imag-obs-alert-watchdog.sh` also detects a restart STORM: each pass it reads `imag-obs.service`'s `NRestarts` counter (`systemctl --user show imag-obs.service -p NRestarts --value`, over ssh with `XDG_RUNTIME_DIR` for the user bus, issue 998) and pages ONE deduped Discord alert when it storms — the alert the 8.5 h incident lacked. Reusable design points for any counter-based storm detector:

- **Fold into the EXISTING dev1 prober — never a second prober.** The pure seam is `scripts/lib/imag-obs-restart-storm.sh` (`imag_obs_restart_counter_probe_cmd` builder + a time-windowed `imag_obs_restart_storm_classify`), and it reuses `obs_watchdog_alert_throttle` for dedup (sig `imag-restart-storm`, ~1 h reminder, cleared on any non-storm pass — the SAME direct `write_state_field alert_sig ""` the down-alert path already uses at ~lines 280/313; this watchdog does NOT use `obs_watchdog_clear_hysteresis`, so a storm-only hysteresis would just diverge from the sibling clears).
- **Ships DISABLED via `IMAG_OBS_RESTART_STORM_ENABLE` (default 0).** Folding into an already-live watchdog can't ship disabled by a not-enabled timer, so the "ships disabled" convention is an env gate the supervisor flips with `Environment=IMAG_OBS_RESTART_STORM_ENABLE=1` on the `.service`.
- **Fail-safe = never false-page:** unreadable counter → preserve the anchor (no page); `cur<prev` → re-baseline (reboot/`reset-failed`/reinstall); storm only on a real `delta≥threshold` within `window_s`. The probe seams `IMAG_OBS_RESTART_PROBE_CMD` (fixture cat) + `IMAG_OBS_RESTART_NOW` (clock) make the whole composition Tier-0 testable by SOURCING the watchdog (never running `main()`) under the caller's real `set -euo pipefail`.
- **GOTCHA — force base-10 in bash arithmetic on a counter value.** A bare `$(( cur - prev ))` reads a leading-zero value as OCTAL, and under the watchdog's inherited `set -e` an `08`/`09` aborts the WHOLE pass (`value too great for base`, error token `08`) — which, since the storm check runs first in `main()`, also skips the down-alert. Coerce every parsed numeric with `10#$x` (e.g. `x=$((10#$x))` right after the all-digit validation) before any `$(( ))`. `[ -lt ]`/`[ -ge ]` are already base-10; only `$(( ))` is octal-hazardous.
