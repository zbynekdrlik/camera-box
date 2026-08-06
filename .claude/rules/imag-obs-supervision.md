---
paths:
  - "scripts/imag-obs-start.sh"
  - "scripts/imag-obs-stop.sh"
  - "scripts/imag-wallpaper-refresh.sh"
  - "scripts/imag-obs-alert-watchdog.sh"
  - "scripts/lib/imag-obs-reachability.sh"
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
