---
paths:
  - "scripts/strih_browser_keeper.py"
  - "scripts/bkshading_panel_app.py"
  - "scripts/strih_satellite_watch.py"
  - "scripts/lib/strih-session-apps.sh"
  - "scripts/lib/strih-obs-collection.sh"
  - "systemd/strih-browser-keeper.service"
  - "systemd/bkshading-panel-app.service"
  - "systemd/companion-satellite.service"
  - "systemd/strih-satellite-watch.service"
  - "systemd/strih-satellite-watch.timer"
  - "tests/python/test_strih_browser_keeper_1399.py"
  - "tests/python/test_strih_browser_keeper_obs_run_1399.py"
  - "tests/python/strih_keeper_fakes_1399.py"
  - "tests/python/test_strih_session_apps_1399.py"
  - "tests/python/strih_session_apps_fakes_1399.py"
  - "tests/python/test_strih_companion_satellite_1399.py"
  - "tests/python/test_strih_satellite_watch_1399.py"
  - "tests/python/test_strih_lx_deploy_session_apps_1399.py"
---

# strih-lx session apps: the browser-source keeper, the shading panel window, Companion Satellite + its watch (issue 1399)

Owner, 4.10.2026, at the Sunday production: "tie browser sceny maju byt vzdy nacitane", "toto je
produkcny pocitac vsetko ma bezat vzdy a stale", "na strih nb ma po starte bezat shading appka",
"nevie to byt pekna pwa appka lebo tam v prehliadacoch je ta hnusna horna lista". Design comment
5977447339 (Approach 1).

## Why the browser scenes went empty

- obs-browser loads a browser source's page ONCE, when the source is created at OBS start, and never
  retries a failed first load.
- strih-lx booted at 04:58 UTC on 4.10.2026; the OBS log read `Could not resolve host` at 04:58:17.
  presenter.lan / fohabl.lan did not answer yet.
- So `Browser camera crew`, `Odpocet`, `CG-presenter` (presenter.lan) and `Browser Ableset`
  (`fohabl.lan`, no scheme) stayed empty until a manual `PressInputPropertiesButton refreshnocache`.

## The keeper (`scripts/strih_browser_keeper.py`, `strih-browser-keeper.service`)

- **Lists the sources over obs-websocket every 5 s** (`GetInputList`, `unversionedInputKind ==
  browser_source`, then `GetInputSettings`). Nothing is hard-coded; a source added later is picked up.
  `GetInputSettings` returns non-defaults only, so a missing `url` is obs-browser's own default
  (pinned against the vendored plugin by a test). A local-file source and a non-network URL are
  logged once and never touched.
- **URL -> probe target:** no scheme = http (`fohabl.lan` -> `fohabl.lan:80`), https = 443, an
  explicit port wins. A scheme counts only at the START of the URL (`fohabl.lan/?next=http://x` is
  http to fohabl.lan).
- **Probes are bounded.** One daemon thread per distinct `host:port`, joined against ONE deadline per
  pass (probe timeout 5 s + 1). `getaddrinfo` ignores the socket timeout, so a hung name lookup must
  not stall the loop: a probe still running at the deadline reads `None` (no information) and its
  target is not probed again until that thread ends, so at most one thread per target. Once it has
  finished, its LATE result is the target's result on the next pass (used once): with slow DNS plus a
  black-holed connect a down server must still read down eventually, or it never gets its recovered
  refresh (review round 2).
- **Debounce (`debounce()`):** a page server is DOWN only after `DOWN_AFTER` (2) failed probes in a row
  (~10 s); a good probe is up at once; an unfinished probe (`None`) changes nothing. Without it, one lost
  TCP connect or one slow DNS answer read as an outage, and the next good probe reloaded a working page
  on air (review round 1, reproduced).
- **The decision is the pure `decide(state, verdict, epoch)`:**
  - no verdict yet -> nothing;
  - down -> no refresh, remember it is down;
  - up and not yet refreshed in THIS OBS run -> refresh (`obs-run`);
  - up, refreshed this epoch, and down before -> refresh (`recovered`);
  - otherwise nothing. A working page is never refreshed periodically (a refresh blanks the source
    for a moment).
- **A refresh OBS refuses is pressed again.** The source's state is committed only after
  `PressInputPropertiesButton` succeeded; on a refusal the previous state stays, so the next pass decides
  the same refresh (obs-run OR recovered). The first draft committed "up" on a failed recovered press
  and so forgot it. The reachability log keeps its own memory (`_logged`), so a retry never repeats a
  transition line; the refusal is logged once per distinct error.
- **The OBS run epoch** (main ROZHODNUTE 5978724027) goes up ONLY when OBS itself (re)started. A
  keeper restart (a crash, setup-strih's try-restart after a code change) or a WS reconnect (a 10 s
  request timeout, a dropped socket) under the same OBS refreshes NOTHING: a refresh blanks a graphic
  on air. The first cut bumped on every (re)connect and could not tell these apart.
  - **The identity**, read on every connect: `process` = `<boot id>:<start ticks>` of the OLDEST
    local process with comm `obs` owned by the keeper's uid (`/proc/<pid>/stat` field 22, split after
    the LAST `)`; `local_obs_process_identity`), and `frames` = GetStats `renderTotalFrames`, updated
    every pass (a restart after a long run can come back with more frames than the old run had at ITS
    connect). `obs_restarted(stored, current)`: both process ids known -> new run iff they differ
    (exact); else both frame counts known -> new run iff the count went backwards; nothing stored or
    nothing comparable -> new run.
  - **The oldest, not the newest:** OBS is single-instance, so a second `obs` is a short-lived stray
    (the "already running" dialog of a desktop launch). Newest read it as a new run, a refresh round on
    air, and another one when it exited (review round 6).
  - **/proc is read as bytes.** A comm is cut at 15 bytes and can split a multi-byte character. Read as
    text it raised `UnicodeDecodeError` (a ValueError, not an OSError) on every connect, and the keeper
    crash-looped (review round 6).
  - **Persisted in the /run state file** with `obs_epoch` and per source the memory `decide` needs:
    `refreshed_epoch` + `remembered_reachable`, kept apart from the pass's verdict `reachable`. A keeper
    restarted while its verdict is still unknown (its probe history is fresh: down needs 2 failed
    probes) must not forget that the server was down, or the outage's recovered refresh is lost.
  - **The state file's `sources` are `Keeper.rows()`**: the last finished pass's rows (its verdicts)
    with the CURRENT committed memory, plus every remembered source no finished pass listed yet (a
    restored one, or one first committed in a pass that was cut short). A pass cut short
    by a lost connection may already have refreshed a source; with the last finished pass's rows the
    next keeper refreshed it a second time in the same OBS run (review round 6). A keeper restarted
    while OBS is down writes the restored memory back the same way.
  - `/run/user/<uid>` is tmpfs: after a boot nothing is stored, so OBS just started = a new run. A new
    epoch is set past every restored `refreshed_epoch`, so each source is refreshed exactly once.
  - A remote OBS (`--host` not loopback) has no process identity; the frame count decides.
    `--obs-process-name` (default `obs`) names the process. `--check-state` (verify item 37) ends with
    `OBS run identified by the obs process start time` or `... by the frame count only (no local obs
    process seen)`; on strih-lx the second one means the /proc read found no OBS of the keeper's uid.
  - Tests: `tests/python/test_strih_browser_keeper_obs_run_1399.py` runs the real loop against the real
    obs-websocket fake (`strih_keeper_fakes_1399.py`: `restart()` = new process + frames from 0 or a
    given start count + dropped socket, `drop()` = socket only), one `k.run` per keeper process over one state file.
- **TCP reachability is not HTTP health.** A page server behind a proxy that listens before its
  backend is ready serves an error page to the connect refresh, and the keeper never reloads it
  (TCP stayed up). The design chose the TCP probe; an HTTP-status probe is the next step if it bites.
- **The WS client** is a small inlined obs-websocket 5 client in the obs_phase2 `_conn`/`_rpc` shape:
  Identify with `eventSubscriptions: 0` (the event-flood lesson) and a hard deadline per request.
  It does not import obs_phase2.py, which reaches the box only with a GH_TOKEN. strih-lx's WS has no
  password; `OBS_PASSWORD` is honoured (tested against a fake server that demands auth), so a box with
  a WS password can reuse it.
- **Logs:** one line per refresh, per reachability transition, and per connection change. A quiet
  pass logs nothing. "not reachable" is logged once per outage, not every 5 s.
- **State file** `%t/strih-browser-keeper.json` (`/run/user/<uid>/`, version 2), written atomically
  every pass, also while OBS is down (`connected: false`): `obs_epoch`, `obs_identity`, `refreshes`,
  `sources[]`, `last_error`. `--check-state FILE` grades it for verify-strih: last pass within 60 s AND
  connected. An unreadable file is logged and treated as after a boot.

## The panel app (`scripts/bkshading_panel_app.py`, `bkshading-panel-app.service`)

- ONE Gtk 3 window titled **"Shading"**, 1280x980, holding a WebKit2 4.1 WebView on
  `http://127.0.0.1:8770/` (the port is pinned to `bkshading/service/src/config.rs` `default_bind` by a
  test). No browser chrome, no context menu (`context-menu` returns True), no new windows (`create`
  returns None, `javascript_can_open_windows_automatically` False), no developer extras.
- **A failed load** (`load-failed`) reloads after 3 s and returns True, so no WebKit error page is
  shown; a cancelled load (a newer load started) is not retried. A crashed web process
  (`web-process-terminated`) reloads after 3 s. One pending reload at a time.
- **An outage is logged once, not every 3 s:** one line when a failure streak starts or its reason
  changes, and `loaded ... after N failed attempt(s)` when a load finishes again (`load-changed`
  FINISHED of a load that did not fail; a FINISHED follows a failed load too). A service down all
  day would otherwise write ~29 000 journal lines.
- **Closing the window exits the app**; the unit's `Restart=always` (3 s) brings it back.
- **It is a NORMAL window.** Never keep-above / keep-below and no special type hint: the live
  4.10.2026 trial first put it `below` the multiview and openbox Alt+Tab could not reach it.
- **WM_CLASS** comes from `GLib.set_prgname("bkshading-panel-app")`, called BEFORE `from
  gi.repository import Gtk`: PyGObject initialises GDK on that import and takes the class from the
  program name it sees then (`Gtk.Window.set_wmclass` is deprecated). Read live under Xvfb:
  `WM_CLASS = "bkshading-panel-app", "Bkshading-panel-app"`; with the set_prgname after the Gtk import
  the class read `Bkshading_panel_app.py`. `STRIH_PANEL_WM_CLASS` in the lib is pinned to `PRGNAME`.
- The toolkit modules are arguments of `PanelWindow`, so the pytest drives the real wiring with
  stand-in Gtk / WebKit2 / GLib (CI has no display and no gi).
- **Local smoke under Xvfb** (dev1 has python3-gi + WebKit2 4.1 + Xvfb, no wmctrl): run the app with
  `--url` on a closed port for 8 s (ONE `... reloading ... every 3 s until it loads` line, the window
  already up), then serve a page (one `GET /` 200 and `loaded ... after N failed attempt(s)`), then
  `xwininfo -root -tree | grep '"Shading"'` + `xprop WM_CLASS`.

## Companion Satellite (`companion-satellite.service`) + its watch (`strih-satellite-watch.timer`)

Owner, 4.10.2026: "streamdeck tam nejako crashol teraz na strih". The strih Stream Deck XL is driven
by Companion Satellite (the Electron desktop build setup-strih step 16 installs to
`/opt/companion-satellite/companion-satellite`), connected to the venue Companion 10.77.9.205:16622.
Design comment 5979157737 (Approach 1); finding + live stopgap 5979039607.

- **Before:** a bare kiosk autostart line, `... companion-satellite >/dev/null 2>&1 &`. Nothing restarted
  it and its output was discarded, so a crash left the deck dead with no log until the next login.
- **The unit:** `DISPLAY=:0`, `Restart=always`, `RestartSec=3`, `StartLimitIntervalSec=0`,
  `TimeoutStopSec=10` (a hung Electron app that ignores SIGTERM must not hold a restart 90 s), journal
  output. `ExecStart` is the /opt binary; its ONE source is `STRIH_COMPANION_SATELLITE_BIN` in the lib,
  printed by `strih_companion_satellite_bin` (pinned by a test). The Electron ELF IS that file (read from
  the pinned tarball, not a wrapper script). Its argv is NOT evidence: Chromium rewrites the browser
  process title, so the grader reads `/proc/<MainPID>/exe` instead (below).
- **The bare launch line and its helper are gone.** The autostart starts the unit like every session
  app, and verify-strih's (companion) item greps `strih_session_app_autostart_line
  "$STRIH_COMPANION_SATELLITE_UNIT"`.
- **The stopgap takeover:** the main's live stopgap is a user unit under the SAME name
  (`DISPLAY=:0`, `Restart=always`, enabled). Step 16d (any run of setup-strih with this code, by hand or
  in a deploy) overwrites the file and removes any `default.target.wants/<entry>` link (generalized from
  the panel migration to every entry). It then runs `daemon-reload` and try-restarts the running
  stopgap onto the provisioned unit.
- **A Satellite started OUTSIDE the unit is a fault this code does not grade.** The main reported that a
  second launch exits on the Electron singleton lock. So if a stray instance holds the lock (a hand
  launch, an old bare autostart line), the unit's own instance exits and `Restart=always` with
  `StartLimitIntervalSec=0` relaunches it every 3 s, forever. The watch then reads the STRAY instance's
  REST, and its try-restart cannot reach the stray process. After any hand launch, check: exactly one
  main process, and it is the unit's `MainPID` (live step 7).

### The watch (`scripts/strih_satellite_watch.py`, a 30 s timer)

- **One pass per run.** `strih-satellite-watch.timer` (`OnActiveSec=5s`, `OnUnitActiveSec=30s`,
  `AccuracySec=1s`: OnUnitActiveSec alone never fires before the service ran once, and a user timer's
  default accuracy is 1 min) runs the oneshot `strih-satellite-watch.service` (`TimeoutStartSec=20`).
- **A pass reads:**
  - the Satellite REST (127.0.0.1:9999; v3.4.0 source `satellite/src/rest.ts`): `/api/status`
    `connected`, `/api/surfaces` (an array of the open surfaces), `/api/config` (`protocol`,
    `host`, `port`: the Companion target the Satellite itself uses, so no per-box fact in a
    committed unit);
  - a TCP connect to that host:port, ONLY when the Satellite is not connected (review round 1). A live
    session already proves the port answers. A bare connect every 30 s would show up as a client
    session on the venue Companion, about 2880 a day. The probe never raises (an IDNA-invalid host is a
    `ValueError`, caught);
  - the Stream Deck on USB from sysfs `idVendor:idProduct`, which is what lsusb reads, with no
    usbutils dependency. The default is `0fd9:008f` (the strih XL); `--usb-id` adds others. Never
    match the bare Elgato vendor 0fd9: it also covers capture devices, which would read as "the deck
    is plugged in" forever and restart the Satellite every minute.
- **The pure `decide(since, obs, now)`** keeps a start time per fault:
  - `not-connected` = `connected` false while the Companion port answers;
  - `no-surfaces` = the surfaces list empty while the Stream Deck is on USB.
  A fault held 60 s (`SUSTAIN_S`) restarts the unit and every window starts over. No fault exists
  while the Companion port is down or unknown (`faults()` returns nothing), so nothing restarts while
  Companion itself is down. A silent REST, unreadable surfaces or unreadable USB are no information:
  the window starts over.
- **Back-off (review round 1):** a fault a restart cannot cure would otherwise restart the Electron app
  every 60-90 s forever on the production box. Examples: a Companion refusing the client's API version,
  or the Stream Deck plugin disabled. `effective_sustain(sustain, unhealed)` counts restarts with no
  healthy pass between them. After 3 (`BACKOFF_AFTER`), the next restart needs 120 s of fault, then
  240 s, 480 s, and 900 s (`BACKOFF_MAX_S`, the cap). An `ok` pass resets the count. The back-off is
  logged once, and the watch never gives up.
- **The window clock:** `now` (the boot clock) is read BEFORE the slow REST/TCP reads, so a slow read
  never shortens or stretches a window. More than 75 s between two passes (`MAX_PASS_GAP_S`, 2.5 timer
  periods: a stopped timer, or a suspended notebook, since `CLOCK_BOOTTIME` counts suspend) is no
  evidence the fault held in between. Every window starts over then, and the gap is logged.
- **The restart** is `systemctl --user --no-block try-restart companion-satellite.service`. try-restart
  never starts a stopped unit (a deliberately stopped Satellite stays stopped), and `--no-block` keeps a
  slow stop out of the oneshot's timeout. Each restart is logged with its fault and the held seconds. A
  pass is bounded: 3 REST reads + 1 TCP connect (3 s each) + systemctl (5 s) = 17 s, within
  `TimeoutStartSec=20`, pinned by a test.
- **State file** `%t/strih-satellite-watch.json` (version 1, atomic write): `since` on the boot clock
  (`CLOCK_BOOTTIME`, so no wall-clock step fakes a window), `condition`, `observation`, `restarts`,
  `unhealed_restarts`, `effective_sustain_s`, `last_restart`, `last_ok_epoch_s`. `--check-state` (verify
  item 40's pass row) needs a pass within 90 s (three timer periods), and names the current condition
  plus any fault in progress with its (backed-off) restart time. While a FAULT holds (`restarted` or
  `fault:*`), it FAILs when a restart newer than the last healthy pass failed (no user bus), or when the
  watch backed off: a watch that runs but cannot cure the fault is a red, never a quiet OK. Companion
  down or a silent REST is not such a fault, and an older failed restart is history (review round 2).
- **The grader's process facts** are `exe|start|cmdline`: the free-form cmdline LAST, so a `|` inside
  it never shifts the fixed fields (review round 2).
- **Logs:** one line when the condition changes (rest-silent / companion-down / fault:<ids> / ok) and
  one per restart. A quiet pass logs nothing. The oneshot service sets `SyslogLevel=notice` +
  `LogLevelMax=notice`: the user manager's every-30-s "Starting"/"Finished" lines are info and are
  dropped, while the watch's lines and any failure stay. This was checked on dev1's systemd 255 with
  `systemd-run --user -p LogLevelMax=notice`.
- **THE HONEST LIMIT:** the 4.10.2026 fault had NO REST signature (`connected=true`, the XL listed,
  while the deck did not respond). The watch catches only the classes the REST shows. A hang it cannot
  see still needs a report, and the unit's own `Restart=always` only catches a Satellite that exits.

## Start path: the kiosk autostart, not default.target

Every entry is `WantedBy=graphical-session.target` like `strih-obs.service`, and like it STARTED by the
kiosk openbox autostart (`strih_openbox_autostart_text` calls `strih_session_apps_autostart_lines`,
after strih-obs + the bundle-state server, in `STRIH_SESSION_APP_UNITS` order: keeper, panel, Satellite,
watch timer).
- openbox never reaches graphical-session.target, so the WantedBy only makes `enable` meaningful.
- A `default.target` unit would also start on a user-manager start with no X session (an ssh login,
  linger), where the panel or the Satellite tray cannot open a window. ONE login start path, the one OBS
  uses. The strih-lx genlock deploy also starts them after strih-obs, into the running session (below).
- imag's autostart is a separate renderer and is untouched.
- Every unit sets `StartLimitIntervalSec=0`: never give up.

## Provisioning + grading (`scripts/lib/strih-session-apps.sh`)

- **setup-strih step 16d** (lettered, TOTAL_STEPS stays 17) = `strih_session_apps_install`:
  - packages `python3-gi gir1.2-webkit2-4.1 python3-websocket` (only the missing ones), then an
    import preflight of websocket + Gtk 3 + WebKit2 4.1 with the system python3. A failure of either
    FAILs the step before any file is written.
  - every entry's repo program into `/usr/local/bin` (0755) and every unit FILE into
    `~/.config/systemd/user` (0644), each written ONLY when its bytes or mode differ
    (`strih_session_apps_put`): an unchanged file keeps its mtime, which verify's process check compares
    against. Per entry: `strih_session_app_script` (the repo program; EMPTY for the Satellite, whose /opt
    binary step 16 installs), `strih_session_app_program` (the path the box runs),
    `strih_session_app_unit_files` (a timer brings its `.service`) and `strih_session_app_run_unit`.
    A missing /opt Satellite binary only WARNs (step 16 fails loud itself);
  - the 4.10.2026 stopgaps migrated: a unit file is overwritten under the SAME name, every entry's
    `default.target.wants` link and the panel's `~/.local/bin/bkshading-panel-app` copy are removed;
  - `systemctl --user enable` every entry (never the watch's oneshot `.service`, which has no
    `[Install]`: its timer starts it; as root through `sudo -u <user> XDG_RUNTIME_DIR=/run/user/<uid>`),
    then an explicit `daemon-reload` when a unit FILE was written, then `try-restart` the units whose
    files changed. try-restart restarts only a RUNNING unit (the stopgap panel on the first deploy, an
    older keeper/panel later) and never starts a stopped one, so this stays enable-only. In the genlock
    deploy OBS is stopped at that moment, so a keeper restart costs no extra refresh. A failed enable
    (no user bus = nothing runs) skips the reload and the restart; every failure only WARNs, with
    systemctl's own error. `NeedDaemonReload` is deliberately NOT graded: systemd keeps it manager-wide
    (the dantesync finding in `strih-linux-provisioning.md`), so another unit's change would FAIL this
    item; the explicit reload before the restart + the process start-vs-mtime check cover it.
- **verify-strih items 37-40** = `strih_session_apps_grade_report` (one PASS/FAIL row each; the
  report FAILs unless all `strih_session_apps_row_count` rows came back, units + 3):
  - `(browser-keeper)` / `(shading-app)`: the unit and its program byte-identical to the checkout,
    the `graphical-session.target.wants` link, the autostart start line, `is-active` = active, and the
    unit's MAIN PROCESS: `/proc/<MainPID>/cmdline` must run the installed program and
    `ExecMainStartTimestamp` (`--timestamp=unix`) must be at or after the newest mtime of the installed
    program + unit (`strih_session_app_process_state`: `stale` / `wrong-program` / `unreadable`). Files
    on disk alone would read OK while the old process runs (the stopgap, or code from before a deploy).
    The /proc read wraps the redirect in a group with `2>/dev/null` (a bare `< file 2>/dev/null` prints
    the open error before the command's own redirect applies), so a main process that exits between
    `show` and the read is a quiet `unreadable`.
  - `(companion-satellite)`: the unit byte-identical, the /opt binary present (executable; there is
    no checkout copy), enabled, the autostart line, active, and the main process:
    `readlink /proc/<MainPID>/exe` must equal `readlink -f <binary>` (`strih_session_app_binary_state`;
    a binary replaced under the running process reads `... (deleted)` = wrong-program), started at or
    after the newest mtime of the unit + binary. Its argv is never read (Chromium rewrites the title).
    The python entries keep the argv1 check. A missing binary points at step 16.
  - `(satellite-watch)`: BOTH unit files (timer + service) and the program byte-identical, the timer
    enabled + autostarted + active, and instead of a main process (a oneshot has none between passes) the
    service's `ExecMainStartTimestamp` (its last run; `strih_session_app_last_run_state`: `never` /
    `stale`) at or after the newest mtime. `LastTriggerUSec` was rejected: `--timestamp=unix` does not
    format it on systemd 255 (it printed a local date on dev1).
  - `(browser-keeper-pass)` / `(satellite-watch-pass)`: the program's own `--check-state` on its runtime
    state file (`strih_session_app_pass_row`);
  - `(shading-app-window)`: a window with the panel's own WM_CLASS AND the title "Shading" in
    `wmctrl -lx` on :0 (field 3 = `res_name.res_class`, field 5 on = the title; as root through
    `sudo -u` with the operator's Xauthority). An empty list = X not answering, named as such. Read the
    first live `wmctrl -lx` line on strih-lx when grading this the first time (dev1 has no wmctrl).
- Test seams: `STRIH_SESSION_APPS_BIN_DIR`, `_RUNTIME_DIR`, `_EUID`, `_PYTHON`, `_WMCTRL`, `_PROC`,
  `_SATELLITE_BIN`; the fakes live in `tests/python/strih_session_apps_fakes_1399.py` (shared by the
  keeper/panel and the Satellite tests). The
  pytest runs the real install + grader with fake `dpkg-query` / `apt-get` / `systemctl` / `sudo` /
  `id` / `wmctrl` on PATH and a fake `/proc`, under the callers' `set -euo pipefail`. The fake `sudo`
  drops `-u USER` and EXECS the rest (`env VAR=val cmd ...`), so a grade test with `_EUID=0` runs the
  root path the deploy's verify takes, not just a logged line.

## verify-strih line budget: the item-28 split

verify-strih.sh sat at 1002 lines. Adding items 37/38 required a split first: item 28's collection
reads (find the active collection JSON, count `shader_filter` + `scripts-tool`) moved verbatim into
`scripts/lib/strih-obs-collection.sh` (`strih_active_collection_json`, `strih_collection_hygiene_counts`).
Item 28's verdict and NOTE lines stay in verify-strih (the Rust anchor pins them). verify-strih is now
~970 lines; the next item there again needs a lib. Items 39/40 cost verify-strih no lines: they are rows
of the same lib grader.

## The genlock deploy starts them; setup-strih stays enable-only

setup-strih never starts a stopped unit, and items 37-40 grade them ACTIVE. So the strih-lx genlock
deploy's start step (`strih_lx_remote_start_cmd`, `scripts/lib/strih-lx-deploy.sh`) starts
`STRIH_SESSION_APP_UNITS` right after strih-obs.service, the kiosk autostart's order (main ROZHODNUTE
5978724027). The first deploy that installs them therefore needs no manual start, and its acceptance
step (verify-strih) sees them active.
- The remote command's rc stays OBS's (the deploy exits 4 on it). An app that does not start is a
  named `WARNING: [strih-lx start] the session apps (...) did not start` on stderr; items 37-40 in the
  acceptance step then name why. An OBS start failure still tries the apps.
- `start` is a no-op on a running unit. A keeper that ran through the deploy reconnects to the new OBS
  by itself: a new OBS process, so one refresh round, which is the point.
- The same command is the failure path's best-effort start and the `--plan` STEP 7 line.
- It must stay ONE line and carry none of the other steps' texts: the Rust exec test's ssh stub
  dispatches on them, first match wins. `tests/python/test_strih_lx_deploy_session_apps_1399.py` runs
  the command under bash with stub `systemctl`/`id`.
- Outside a deploy (setup-strih run by hand, a fresh box), a stopped unit stays stopped until the next
  kiosk login or a `systemctl --user start`.

## Live steps (supervisor, after integration)

1. The next strih-lx genlock deploy (`deploy-genlock-fleet.sh --run-id <id> --boxes strih-lx`) runs
   setup-strih: step 16d logs the stopgap links + copy removed, every entry enabled, and `try-restart
   ... bkshading-panel-app.service ...`. Its start step then starts strih-obs and every app; no WARNING
   line from `[strih-lx start]`. No manual start.
2. `journalctl --user -u strih-browser-keeper -n 30`: `connected to obs-websocket 127.0.0.1:4455: a new
   OBS run (process <boot>:<ticks>, N frames) -> OBS run epoch 1`, the process NOT `None`, then one
   `refreshed browser source ...` line per browser source.
3. The deploy's acceptance step (verify-strih) PASSes items 37 + 38; the window row names the WM_CLASS
   and the keeper row ends `OBS run identified by the obs process start time`.
   Read the first live `wmctrl -lx` line once.
4. Keeper restart, same OBS: `systemctl --user restart strih-browser-keeper` -> `resumed from ...` and
   `the same OBS run as before (OBS run epoch 1) -- no refresh round`, and no `refreshed` line (no
   blink on air).
5. Acceptance: restart strih OBS while presenter.lan is up -> `a new OBS run ... -> OBS run epoch 2`
   and every browser scene renders again without a hand refresh; stop the presenter page server for
   > 10 s, start it again -> its sources refresh once; Alt+Tab reaches the "Shading" window.

### Companion Satellite + its watch (design 5979157737)

6. The deploy's step 16d logs `removed the 4.10.2026 stopgap link
   ~/.config/systemd/user/default.target.wants/companion-satellite.service` (only if the stopgap was
   enabled that way), `installed companion-satellite.service ... unit written`, and a `try-restart` naming
   `companion-satellite.service strih-satellite-watch.timer`. The start step logs no WARNING.
7. Read back on strih-lx (as newlevel, `XDG_RUNTIME_DIR=/run/user/$(id -u)`):
   - `diff ~/.config/systemd/user/companion-satellite.service <checkout>/systemd/companion-satellite.service`
     is empty, and `ls ~/.config/systemd/user/default.target.wants/` has no companion-satellite link;
   - `systemctl --user show -p MainPID,ExecMainStartTimestamp,NRestarts companion-satellite.service`:
     the start is after the unit file's mtime, `NRestarts` is not climbing (a climbing count = a stray
     instance holds the singleton lock), and `readlink /proc/<MainPID>/exe` =
     `/opt/companion-satellite/companion-satellite`;
   - the main processes of that binary are exactly one, the unit's MainPID: every process whose
     `/proc/<pid>/exe` is the binary and whose cmdline has no `--type=` argument (the Chromium child
     processes carry one); read the first live `tr '\0' ' ' < /proc/<MainPID>/cmdline` once too;
   - `curl -s 127.0.0.1:9999/api/status` reads `"connected":true`, and `/api/surfaces` lists the XL;
   - `journalctl --user -u companion-satellite -n 20` now shows the Satellite's own output;
   - `systemctl --user list-timers strih-satellite-watch.timer` shows a next run within 30 s;
     `journalctl --user -u strih-satellite-watch -n 5` shows ONE `watching companion-satellite.service:
     Satellite connected to Companion 10.77.9.205:16622, 1 surface(s) (...)` line and no
     Starting/Finished lines.
8. verify-strih PASSes items 39 + 40: `(companion-satellite)`, `(satellite-watch)`,
   `(satellite-watch-pass) last pass N s ago: ...; 0 restart(s)`, and `(companion) ... ok-connected`.
9. No live fault drill: both fault classes need a Satellite that is broken while it runs; the logic is
   pinned by the pytest tables. A wrong port in the Satellite's config reads as `companion-down` (the
   watch probes the target the Satellite itself uses), so it never restarts, by design.
