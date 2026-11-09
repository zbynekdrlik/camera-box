---
paths:
  - "scripts/lib/obs-box-xfce.sh"
  - "scripts/obs_box_xfconf.py"
  - "scripts/lib/obs-box-panel.sh"
  - "scripts/lib/obs-box-kiosk.sh"
  - "scripts/strih-boxes/*.env"
  - "tests/python/test_obs_box_xfce_1466.py"
---

# The operator desktop is a box fact: openbox or XFCE (issue 1466)

Owner ROZHODNUTÉ 6737530836 (9.11.2026): "schvalujem postupny prechod na xfce". Design 6737548111. The
owner's complaints were window management, mouse and touchpad behaviour on the openbox kiosk; XFCE
(xfwm4 + xfce4-panel + xfce4-settings) is the Windows-like session. The move is GRADUAL: one box at a
time, reversible.

## The fact

`STRIH_DESKTOP` = `openbox` | `xfce` in every `scripts/strih-boxes/*.env` (the last key; accessor
`strih_box_desktop`). stream-snv is the first `xfce` box; strih-snv, strih-pp and stream-pp stay
`openbox`. Moving a box either way = that line + setup-strih + a reboot. The lightdm session name is the
same word (`/usr/share/xsessions/xfce.desktop`, read on strih-snv: xfce4 4.20).

## What setup does

- **Step 11 (kiosk):** `obs_box_kiosk ... "$(strih_box_desktop)"` (4th argument, default openbox, imag
  passes none). An xfce box installs `xfce4 --no-install-recommends` (no power manager, screensaver or
  indexer come with it), checks the session file, and writes `autologin-session=xfce` /
  `user-session=xfce`. openbox stays installed on every box. The baseline grader takes the same session
  as the 4th argument of `obs_box_baseline_gather_snippet` (verify-strih passes the fact).
- **Step 15 (`obs_box_session_install`):** the ONE session start
  `/usr/local/bin/obs-box-session-start` = `strih_session_start_text <desktop>` on every box.
  - `strih_openbox_autostart_text` IS `strih_session_start_text openbox`, and step 15 still writes it
    as `~/.config/openbox/autostart` on EVERY box. Reason: the shared baseline `autostart` row (imag +
    strih) greps that file, and a box moved back to openbox needs nothing else. Openbox boxes are
    byte-identical (the 1361 golden did not move).
  - The xfce form differs by ONE line: no tint2 start (`obs-box-panel.service`).
  - An xfce box also gets: `~/.config/autostart/obs-box-session.desktop` (runs the script,
    `OnlyShowIn=XFCE;`); `Hidden=true` overrides in the USER's `~/.config/autostart/` (never
    `/etc/xdg/autostart`, package conffiles) for localsearch-3 (133 % CPU, measured), tracker,
    update-notifier, xfce4-power-manager, xfce4-screensaver, light-locker; `localsearch-3.service`
    masked for the user (a `/dev/null` link, no user bus needed); xfwm4 `general/use_compositing=false`;
    xfce4-session `general/SaveOnExit=false`; the keys; the polkit rule
    `/etc/polkit-1/rules.d/50-obs-box-power.rules` (power-off + reboot, incl. `-multiple-sessions`, for the
    desktop user only; logind answered `challenge` in the XFCE session).
  - An openbox box loses the XDG entry and the polkit rule.
- **Step 16h (`obs_box_panel_install`):** on an xfce box tint2's files stay but its unit is DISABLED,
  then `obs_box_xfce_panel_install`: the xfce4-pulseaudio-plugin on the stream role, each launcher's
  `.desktop` in its plugin folder `~/.config/xfce4/panel/launcher-<id>/obs-box-panel-<id>.desktop` (the
  panel reads it there and never copies it), the panel channel.

## The panel and the keys (one source with tint2 / openbox)

- `obs_box_xfce_panel_plugins` = applicationsmenu, one launcher per `obs_box_panel_launchers` app (the
  SAME list as tint2, Vypnúť first), tasklist, an expanding separator, systray, pulseaudio (stream role
  only, its own keyboard + multimedia keys OFF so our helper keys win), clock, actions. Ids 1..N in that
  order; ONE panel `p=8;x=0;y=0` (bottom), length 100, locked, 30 px.
- `obs_box_xfce_keys_spec`: `commands/custom` replaced wholesale + `override=true`: Alt+F1 menu, the
  brightness helper (the openbox keybind commands), Print / Alt+Print = `flameshot gui`, on the stream
  role the three XF86Audio keys = `obs-box-headphones` (issue 1465). XKB Alt+Shift stays XKB.

## xfconf files: MERGED and graded by property, never written whole

The XFCE apps rewrite their channel files (strih-snv's user xfwm4.xml carried every property after one
login), so byte identity cannot hold. `scripts/obs_box_xfconf.py`:
- `merge CHANNEL FILE SPEC` prints the file with the declared properties set, every other kept; `SPEC`
  = `PATH<TAB>TYPE<TAB>VALUE` lines (`array:<type>` comma-separated, `PATH<TAB>replace` = drop that
  node's child properties first). An unparsable file or another channel = rc 2, no output (never
  merged over).
- `check FILE SPEC` = `ok` or the first difference (an array exactly, in order).
- ElementTree output, indented; a re-merge prints the same bytes (idempotent).
- **Never written under a live xfconfd:** `obs_box_xfce_write_channel` skips a file that would change
  while the user's `xfconfd` runs (WARN); verify FAILs until setup re-runs with the session logged out
  (the user.ini rule). The slice-2 conversion runs setup from the openbox session, so the files land
  before the first XFCE login.
- Every file goes through `obs_box_write_if_changed[_rendered]` (the stdin form fed only `printf '%s\n'
  "$var"`, the 1474 sweep).

## Verify

- Item 44 (`obs_box_panel_grade_rows`, still `OBS_BOX_PANEL_ROWS`=10): on an xfce box
  `_obs_box_panel_xfce_rows` grades `(panel-pkg)` xfce4-panel, `(panel-config)` the panel properties +
  every launcher plugin file, `(panel-unit)` tint2's unit NOT enabled + an `xfce4-panel` running for the
  user, `(panel-window)` class `xfce4-panel.Xfce4-panel`; the other six rows are the openbox box's.
- Item 48 (`obs_box_session_grade_report`, on the item-43/44 line): `(session-start)` on every box, then
  one NOTE on openbox, or on xfce `(xfce-pkg)`, `(xfce-session-entry)`, `(xfce-hidden)` (files + the
  mask + none of their processes running), `(xfce-settings)`, `(xfce-keys)`, `(xfce-polkit)`,
  `(xfce-running)` (an `xfce4-session` for the user) = `OBS_BOX_XFCE_ROWS` 8.
- Seams: `OBS_BOX_SESSION_START_BIN`, `OBS_BOX_XFCE_POLKIT_RULE`, `OBS_BOX_XFCE_XSESSION`,
  `OBS_BOX_XFCE_PGREP`. Tests: `tests/python/test_obs_box_xfce_1466.py` (stub `chown`; a root:root file
  always re-reads as written as non-root).

## UNVERIFIED until slice 2 on stream-snv (supervisor)

- that xfce4-panel 4.20 shows a launcher whose `.desktop` carries `NoDisplay=true` (shared with tint2's
  text) and does not copy/rename our launcher file;
- that `p=8` puts the panel at the bottom over the full width;
- the `xfce4-panel.Xfce4-panel` WM_CLASS in `wmctrl -lx`;
- that xfsettingsd does not fight the session start's xrandr layout or the display watch (no
  displays.xml profile is written);
- that the pulseaudio plugin with keyboard shortcuts off leaves the XF86Audio keys to the shortcut.

## Slice 2 runbook (stream-snv, supervisor)

1. With the box still in openbox: `sudo ./scripts/setup-strih.sh --box stream-snv` (installs xfce4,
   points lightdm at xfce, writes the files while no xfconfd runs). Stop OBS first only if step 4 swaps
   the bundle (the genlock deploy does that itself).
2. Reboot. Confirm `pgrep -u newlevel -x xfce4-session`, OBS + restreamer + the session apps up,
   `pgrep -u newlevel localsearch-3` empty.
3. `sudo ./scripts/verify-strih.sh --box stream-snv`: items 32 (baseline kiosk = autologin xfce), 44,
   48 PASS.
4. The operator list: touchpad tap + two-finger scroll, the mouse wheel in the Restreamer windows,
   the start menu, Vypnúť / actions power-off (no password), Print -> flameshot, the headphone keys +
   the pulseaudio plugin, the keyboard layout (Alt+Shift; there is no layout item in xfce4-panel yet).
5. CPU (`top`) and OBS `program-render-audit lagged` beside the last openbox numbers.
6. Rollback: `STRIH_DESKTOP=openbox` + setup-strih + reboot.
