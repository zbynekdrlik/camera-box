---
paths:
  - "scripts/lib/obs-box-panel.sh"
  - "scripts/obs_box_kbd_layout.py"
  - "scripts/obs_box_recordings.py"
  - "systemd/obs-box-panel.service"
  - "tests/python/test_obs_box_panel_1420.py"
  - "tests/python/test_obs_box_kbd_layout_1441.py"
  - "tests/python/test_obs_box_panel_discord_install_1448.py"
  - "tests/python/test_obs_box_recordings_1458.py"
---

# The kiosk panel of the Linux OBS boxes (tint2, issues 1420 / 1441 / 1448 / 1458)

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

## The keyboard item (issue 1441)

`obs_box_kbd_layout.py watch|next|print` (tint2 executor) shows and switches the XKB group. It only works
when X carries BOTH groups: since issue 1441 (21.10.2026) setup step 16g keeps ibus from resetting the
layout to `us` (`scripts/lib/strih-keyboard.sh`, `.claude/rules/strih-operator.md`).

## Test seams (any test that runs the install MUST set them)

`OBS_BOX_PANEL_KBD_HELPER`, `OBS_BOX_PANEL_FILES_HELPER` (else the install writes the real
`/usr/local/lib/camera-box`), `OBS_BOX_PANEL_FILES_BIN`, `OBS_BOX_PANEL_SYS_UNIT_DIR`,
`OBS_BOX_PANEL_RUSTDESK_BIN`, `OBS_BOX_PANEL_DISCORD_BIN`, `OBS_BOX_PANEL_PROC`. The shared `Box` stub in
`test_obs_box_panel_1420.py` answers "installed" for EVERY package once one install ran; a test about
WHICH package apt installs needs per-package stubs (`test_install_asks_apt_for_pcmanfm_when_it_is_missing`).
