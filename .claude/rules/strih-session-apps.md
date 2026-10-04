---
paths:
  - "scripts/strih_browser_keeper.py"
  - "scripts/bkshading_panel_app.py"
  - "scripts/lib/strih-session-apps.sh"
  - "scripts/lib/strih-obs-collection.sh"
  - "systemd/strih-browser-keeper.service"
  - "systemd/bkshading-panel-app.service"
  - "tests/python/test_strih_browser_keeper_1399.py"
  - "tests/python/test_strih_session_apps_1399.py"
---

# strih-lx session apps: the browser-source keeper + the shading panel window (issue 1399)

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
  explicit port wins.
- **Probes are bounded.** One daemon thread per distinct `host:port`, joined against ONE deadline per
  pass (probe timeout 5 s + 1). `getaddrinfo` ignores the socket timeout, so a hung name lookup reads
  unreachable and its target is not probed again until that thread ends: at most one thread per
  target.
- **The decision is the pure `decide(state, reachable, epoch)`:**
  - unreachable -> no refresh, remember it is down;
  - reachable and not yet refreshed in THIS connect epoch -> refresh (`connect`);
  - reachable, refreshed this epoch, and down at the last probe -> refresh (`recovered`);
  - otherwise nothing. A working page is never refreshed periodically (a refresh blanks the source
    for a moment).
  The new state is committed only after `PressInputPropertiesButton` succeeded; a failed press is
  retried next pass and logged once per distinct error.
- **The connect epoch** goes up on every obs-websocket (re)connect. An OBS restart closes the
  connection, so every source is refreshed once more as soon as its server answers. A keeper restart
  (Restart=always, or a deploy) is a new connection too and refreshes each source once: the design's
  "once per connect" cannot tell it from an OBS restart. Expect one short blank per source on a keeper
  restart.
- **The WS client** is a small inlined obs-websocket 5 client in the obs_phase2 `_conn`/`_rpc` shape:
  Identify with `eventSubscriptions: 0` (the event-flood lesson) and a hard deadline per request.
  It does not import obs_phase2.py, which reaches the box only with a GH_TOKEN. strih-lx's WS has no
  password; `OBS_PASSWORD` is honoured if one is ever set.
- **Logs:** one line per refresh, per reachability transition, and per connection change. A quiet
  pass logs nothing. "not reachable" is logged once per outage, not every 5 s.
- **State file** `%t/strih-browser-keeper.json` (`/run/user/<uid>/`), written atomically every pass,
  also while OBS is down (`connected: false`). `--check-state FILE` grades it for verify-strih:
  last pass within 60 s AND connected.

## The panel app (`scripts/bkshading_panel_app.py`, `bkshading-panel-app.service`)

- ONE Gtk 3 window titled **"Shading"**, 1280x980, holding a WebKit2 4.1 WebView on
  `http://127.0.0.1:8770/`. No browser chrome, no context menu (`context-menu` returns True), no new
  windows (`create` returns None, `javascript_can_open_windows_automatically` False), no developer
  extras.
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
  `WM_CLASS = "bkshading-panel-app", "Bkshading-panel-app"`.
- The toolkit modules are arguments of `PanelWindow`, so the pytest drives the real wiring with
  stand-in Gtk / WebKit2 / GLib (CI has no display and no gi).
- **Local smoke under Xvfb** (dev1 has python3-gi + WebKit2 4.1 + Xvfb): run the app with `--url` on
  a closed port for 8 s (three `reloading ... in 3 s` lines, the window already up), then serve a page
  (one `GET /` 200), then `xwininfo -root -tree | grep '"Shading"'` + `xprop WM_CLASS`.

## Start path: the kiosk autostart, not default.target

Both units are `WantedBy=graphical-session.target` like `strih-obs.service`, and like it they are
STARTED by the kiosk openbox autostart (`strih_openbox_autostart_text` calls
`strih_session_apps_autostart_lines`, after strih-obs + the bundle-state server, before Companion).
- openbox never reaches graphical-session.target, so the WantedBy only makes `enable` meaningful.
- A `default.target` unit would also start on a user-manager start with no X session (an ssh login,
  linger), where the panel cannot open a window. ONE start path, the one OBS uses.
- imag's autostart is a separate renderer and is untouched.
- Both units set `StartLimitIntervalSec=0`: never give up.

## Provisioning + grading (`scripts/lib/strih-session-apps.sh`)

- **setup-strih step 16d** (lettered, TOTAL_STEPS stays 17) = `strih_session_apps_install`:
  - packages `python3-gi gir1.2-webkit2-4.1 python3-websocket` (only the missing ones), then an
    import preflight of websocket + Gtk 3 + WebKit2 4.1 with the system python3. A failure of either
    FAILs the step before any file is written.
  - both programs into `/usr/local/bin` (0755), both units into `~/.config/systemd/user` (0644);
  - the 4.10.2026 stopgap migrated: the unit file is overwritten under the SAME name, its
    `default.target.wants` link and its `~/.local/bin/bkshading-panel-app` copy are removed;
  - `systemctl --user enable` both (as root through `sudo -u <user> XDG_RUNTIME_DIR=/run/user/<uid>`);
    no user bus = a WARN, the autostart starts them regardless. Never a start: enable-only.
- **verify-strih items 37 + 38** = `strih_session_apps_grade_report` (one PASS/FAIL row each):
  - `(browser-keeper)` / `(shading-app)`: the unit and its program byte-identical to the checkout,
    the `graphical-session.target.wants` link, the autostart start line, `is-active` = active
    (`strih_session_app_unit_verdict`, fail-closed order, remediation per token);
  - `(browser-keeper-pass)`: `strih_browser_keeper.py --check-state` on the runtime state file;
  - `(shading-app-window)`: a window titled exactly "Shading" in `wmctrl -l` on :0 (as root through
    `sudo -u` with the operator's Xauthority). An empty list = X not answering, named as such.
- Test seams: `STRIH_SESSION_APPS_BIN_DIR`, `_RUNTIME_DIR`, `_EUID`, `_PYTHON`, `_WMCTRL`; the pytest
  runs the real install + grader with fake `dpkg-query` / `apt-get` / `systemctl` / `sudo` / `id` /
  `wmctrl` on PATH, under the callers' `set -euo pipefail`.

## verify-strih line budget: the item-28 split

verify-strih.sh sat at 1002 lines. Adding items 37/38 required a split first: item 28's collection
reads (find the active collection JSON, count `shader_filter` + `scripts-tool`) moved verbatim into
`scripts/lib/strih-obs-collection.sh` (`strih_active_collection_json`, `strih_collection_hygiene_counts`).
Item 28's verdict and NOTE lines stay in verify-strih (the Rust anchor pins them). verify-strih is now
~970 lines; the next item there again needs a lib.

## Enable-only means the first deploy needs a start

setup-strih never starts the units (enable-only). verify-strih items 37/38 grade them ACTIVE, so a
box where they were never started FAILs those items: setup-strih's own step-17 gate, and the strih-lx
genlock deploy's acceptance step (exit 5, "installed + running, the whole-box gate not clear"). The
deploy restarts only strih-obs; a running keeper reconnects on its own and keeps running across
later deploys. So start both once (live step 2 below) or reboot, and every later deploy passes.

## Live steps (supervisor, after integration)

1. Re-run `setup-strih.sh --box strih-lx` (the next genlock deploy does): step 16d logs the stopgap
   link + copy removed and both units enabled.
2. `systemctl --user daemon-reload; systemctl --user restart bkshading-panel-app.service; systemctl
   --user start strih-browser-keeper.service` as the operator (or reboot the box).
3. `journalctl --user -u strih-browser-keeper -n 30`: `connected to obs-websocket 127.0.0.1:4455
   (connect epoch 1)`, one `refreshed browser source ...` line per browser source.
4. `verify-strih.sh --box strih-lx`: items 37 + 38 PASS.
5. Acceptance: restart strih OBS while presenter.lan is up -> every browser scene renders again
   without a hand refresh; stop the presenter page server, start it again -> its sources refresh once.
