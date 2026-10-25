---
paths:
  - "scripts/lib/obs-box-panel.sh"
  - "scripts/obs_box_kbd_layout.py"
  - "scripts/obs_box_recordings.py"
  - "scripts/obs_box_power.py"
  - "scripts/stream_operator_volume.py"
  - "systemd/obs-box-panel.service"
  - "tests/python/test_obs_box_panel_1420.py"
  - "tests/python/test_obs_box_kbd_layout_1441.py"
  - "tests/python/test_obs_box_panel_discord_install_1448.py"
  - "tests/python/test_obs_box_recordings_1458.py"
  - "tests/python/test_obs_box_power_1464.py"
---

# The kiosk panel of the Linux OBS boxes (tint2, issues 1420 / 1441 / 1448 / 1458 / 1464)

One bottom tint2 panel on every Linux workstation box (strih-snv, strih-pp, stream-pp; gate `16h-panel`).
Everything lives in `scripts/lib/obs-box-panel.sh`:
- `obs_box_panel_tint2rc` is the ONE declared config;
- `obs_box_panel_install` is setup-strih step 16h (write-if-changed, enable, try-restart a running panel,
  never a start);
- `obs_box_panel_grade_rows` is verify-strih item 44 (`OBS_BOX_PANEL_ROWS` rows, the count is pinned).

## Launchers (`obs_box_panel_launchers`, one `id|Name|Exec|Icon` per app the box has)

- OBS, Shading, Restreamer start through their units, never a second copy; RustDesk, Discord where the
  binary exists.
- **`files` = `Nahrávky` (issue 1458, owner 21.10.2026: a file explorer to delete old OBS recordings).**
  - It runs `/usr/bin/python3 /usr/local/lib/camera-box/obs_box_recordings.py open`, which resolves the
    ACTIVE OBS profile's record folder at click time: `[Basic] ProfileDir` -> `basic.ini` -> Advanced
    `[AdvOut] RecFilePath` (`FFFilePath` for RecType=FFmpeg) / Simple `[SimpleOutput] FilePath`; an empty
    or missing folder falls back to OBS's own default `~/Videos`, else home. It then starts pcmanfm there.
  - stream-pp's profile (21.10.2026) has no RecFilePath at all, so a per-box fact would have been wrong.
  - The same resolution as `strih-recordings-retention.sh` `rr_obs_profile_record_dir`, which REFUSES
    instead of falling back (it feeds a delete). Keep the two in step when OBS's keys change.
  - pcmanfm's `~/.config/libfm/libfm.conf` gets `[config] use_trash=0` + `confirm_del=1`
    (`obs_box_recordings.py seed-libfm`, merge, other keys kept, written only on a difference): the trash
    of a folder on another disk sits on that disk, so a trashed recording would keep its space. A delete
    still asks first. There is no undo after the confirmation.
  - Row `(panel-files)`: pcmanfm installed, the helper equals the checkout, `check-libfm` passes.
- The launcher list is computed at install AND at grade time; a launcher's presence seam is its binary
  path (`OBS_BOX_PANEL_FILES_BIN`, `_RUSTDESK_BIN`, `_DISCORD_BIN`).

## The power button + Chrome (issue 1464, owner 23.10.2026)

- **`power` = `Vypnúť`, ALWAYS the first launcher (the left end), on every box.** It runs
  `/usr/bin/python3 /usr/local/bin/obs-box-power` = `scripts/obs_box_power.py`, installed by step 16h
  with the other helpers (`OBS_BOX_PANEL_POWER_HELPER`). Its icon is `scripts/lib/obs-box-panel-power.svg`,
  installed to `~/.local/share/icons/obs-box-panel/power.svg` and named by absolute path (tint2 renders
  SVG through librsvg): the theme name `system-shutdown` exists only in some icon themes (on Ubuntu
  Humanity / LoginIcons, not hicolor), so a minimal kiosk could show a blank button.
  - One Gtk 3 dialog: `Zrušiť` / `Reštartovať` / `Vypnúť`. `Zrušiť` is the default response and gets
    the focus AFTER `show_all` (show_all moves the focus otherwise), so Enter / Space / Escape / closing
    the window all cancel. Only the two positive response ids run a command, exactly one of
    `systemctl poweroff` / `systemctl reboot` (bounded 30 s). Keep-above, so a fullscreen projector
    never hides it.
  - A refused command (polkit, an inhibitor) opens a message dialog with systemctl's own error, exit 1.
  - The toolkit is an argument of `PowerDialog`; a real-Gtk smoke under `xvfb-run` on dev1 is the way to
    check the real wiring (respond through `GLib.timeout_add` + `dialog.response(...)`, a stub
    `run_cmd`, NEVER the real systemctl).
- **Row `(panel-power)`:** the program = the checkout, AND logind's `CanPowerOff` + `CanReboot` = `yes`
  read as the desktop user through `systemd-run --user --wait --pipe ... busctl --timeout=10 call
  org.freedesktop.login1 ... Can<Verb>` (as root via `sudo -u <user> XDG_RUNTIME_DIR=/run/user/<uid>`),
  bounded by `OBS_BOX_PANEL_LOGIND_TIMEOUT` (15 s).
  - WHY `systemd-run --user`: tint2 is the --user unit obs-box-panel.service, so the button's systemctl
    runs in the user manager, outside any logind session; polkit attributes such a process to the user's
    display session (the active local seat0 autologin). A busctl from verify's ssh session would be judged
    as that ssh session (`challenge`), a false FAIL.
  - busctl bounds its own call too: with `--pipe` the transient unit holds the read's stdout, so a busctl
    still waiting after `timeout` killed systemd-run would keep the command substitution open. The read's
    stdin is `/dev/null` (`--pipe` hands stdin on as well). It runs through `strih_session_apps_user_run`,
    the one root-or-inline helper `strih_session_apps_user_systemctl` uses too.
  - A FAIL names `challenge` / `no` / `na` / `no answer`. If a box reads `challenge` while its seat0
    session is active + local, the fix is a polkit rule for `org.freedesktop.login1.power-off` /
    `reboot` (not shipped: graded first, never assumed).
- **`chrome` = `Chrome`** (`google-chrome-stable`, icon `google-chrome`) only where the binary exists
  (`OBS_BOX_PANEL_CHROME_BIN`) AND `strih_box_role` = stream. The strih role never gets it (owner
  24.9.2026: production-only). It sits after Restreamer. It never "disappeared" on stream-snv: the
  panel never had one. Its Exec carries `--password-store=basic` (issue 1469): the kiosk autologin
  never unlocks the GNOME login keyring, so plain Chrome popped `Unlock Login Keyring`.
- **Known limit (not fixed, issue 1464 review):** a launcher tint2 starts lives in the panel unit's cgroup,
  so a panel restart (a tint2 crash, step 16h's try-restart after a panel file changed) ends the apps it
  started (RustDesk, Discord, pcmanfm, now Chrome). The fix is a launch through `systemd-run --user
  --scope` for the binary launchers, which needs a live check on a box first (tracked as issue 1465
  item 7, comment 6363702188).

## The screenshot tool (issue 1481, owner 25.10.2026: "nastroj ... podobne ako pod windows mali lightshot")

- flameshot on EVERY role (one Linux workstation type), installed by step 16h with
  `--no-install-recommends` (`obs_box_panel_shot_install`). The `screenshot` = `Snímka obrazovky`
  launcher (`/usr/bin/flameshot gui`, icon `org.flameshot.Flameshot`) sits after Chrome, before
  RustDesk, where the binary exists (`OBS_BOX_PANEL_SHOT_BIN`). Use: drag a region, Ctrl+C =
  clipboard, Ctrl+S = a file in `~/Pictures/Snimky`.
- `~/.config/flameshot/flameshot.ini`: `obs_box_panel_shot_ini_merge` sets the declared `[General]` keys
  (fixed save path, `snimka-%F_%H-%M-%S`, no tray icon, no startup message) and keeps every other key:
  flameshot rewrites its own ini from its settings dialog.
- The openbox keys: the stock `/etc/xdg/openbox/rc.xml` binds `Print` / `A-Print` to scrot.
  `obs_box_panel_shot_rc` REWRITES those `<command>`s to `flameshot gui` and adds a missing one before
  `</keyboard>`. The kiosk's `obs_box_openbox_rc_with_keys` only APPENDS, which would leave two Print
  bindings. The user's rc.xml is seeded from the stock one when absent (`OBS_BOX_PANEL_OPENBOX_RC_SRC`),
  a symlinked file or folder is refused. Keys apply at the next login / `openbox --reconfigure`.
- Row `(panel-shot)` (row 10): the package, the ini and the rc.xml graded by idempotence (the filter's
  output equals the file).

## The headphone volume item (issue 1465 item 3 revision, stream role only)

Owner 24.10.2026 (ROZHODNUTE 6383739315): the stream operator needs an understandable GLOBAL headphone volume.
`_obs_box_panel_tint2rc_headphones` (rendered only when `strih_box_role` = stream, BEFORE the keyboard item, so
`panel_items` is `TSEEC` / `LTSEEC` there and `TSEC` / `LTSEC` on the strih role) runs
`/usr/bin/python3 $OBS_BOX_PANEL_HEADPHONES_HELPER watch` (`/usr/local/bin/obs-box-headphones` =
`scripts/stream_operator_volume.py`): `Slúchadlá NN %` / `Slúchadlá stlmené`, wheel = +-5 %, left click = mute.
`obs_box_panel_headphones_keybinds_xml` declares the XF86Audio keys running the same helper. Both the helper and
the keys are INSTALLED by setup step 12 and graded by verify item 9b's `(operator-audio-control)` row
(`stream-operator-audio.md`); step 16h only renders the item.

## The keyboard item (issue 1441)

`obs_box_kbd_layout.py watch|next|print` (tint2 executor) shows and switches the XKB group. It only works
when X carries BOTH groups: since issue 1441 (21.10.2026) setup step 16g keeps ibus from resetting the
layout to `us` (`scripts/lib/strih-keyboard.sh`, `.claude/rules/strih-operator.md`).

## Test seams (any test that runs the install MUST set them)

`OBS_BOX_PANEL_KBD_HELPER`, `OBS_BOX_PANEL_FILES_HELPER` (else the install writes the real
`/usr/local/lib/camera-box`), `OBS_BOX_PANEL_FILES_BIN`, `OBS_BOX_PANEL_SYS_UNIT_DIR`,
`OBS_BOX_PANEL_RUSTDESK_BIN`, `OBS_BOX_PANEL_DISCORD_BIN`, `OBS_BOX_PANEL_SHOT_BIN`, `OBS_BOX_PANEL_OPENBOX_RC_SRC`
(else the install reads the test host's `/etc/xdg/openbox/rc.xml`), `OBS_BOX_PANEL_PROC`, `OBS_BOX_PANEL_POWER_HELPER`
(else the install writes the real `/usr/local/bin`), `OBS_BOX_PANEL_CHROME_BIN` (dev1 HAS
`/usr/bin/google-chrome-stable`, so a test that leaves it unset gets a Chrome launcher on the stream role
and a launcher-list assert fails on dev1 only), `OBS_BOX_PANEL_LOGIND_TIMEOUT`. The `Box` stub's
`systemd-run` drops its options and runs the command; its `busctl` answers `s "yes"` unless
`state/off` / `state/reb` holds another answer. The shared `Box` stub in
`test_obs_box_panel_1420.py` answers "installed" for EVERY package once one install ran; a test about
WHICH package apt installs needs per-package stubs (`test_install_asks_apt_for_pcmanfm_when_it_is_missing`).
