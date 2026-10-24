---
paths:
  - "scripts/lib/strih-operator.sh"
  - "scripts/setup-strih.sh"
  - "scripts/verify-strih.sh"
  - "tests/python/test_strih_operator_1418.py"
  - "scripts/lib/strih-keyboard.sh"
  - "tests/python/test_strih_keyboard_ibus_1441.py"
---

# strih operator surface: what the operator touches is provisioned AND graded (issue 1418)

Owner, 12.10.2026, 30 min before the SNV event, on the strih-snv notebook:
- the X server stopped answering clients while OBS kept rendering, so the panel froze;
- the Bluetooth mouse could not pair (hci0 DOWN, address 00:00:00:00:00:00);
- the touchpad did not click;
- clicking the Multiview did not cut;
- the Stream Deck was stuck.

The box had been declared production-ready from verify-strih, which graded services, OBS, genlock
and NDI, but nothing the operator touches.

ROZHODNUTE 6126870183 (owner): "nemoze byt ze stale s kazdym notebookom napriek vela hlasenym
chybam a ich opravam zistim ze sa udeju na novom znova a ze si opravy a kontroly nedoplnil do
standartneho procesu".

## The two rules

1. **Every live hand fix on a notebook lands in setup + verify the SAME day.** A fix applied by hand
   to strih / stream / imag (xinput, nmcli, a user.ini edit, a package install) goes into
   setup-strih / setup-imag or the shared obs-box baseline. It also gets a verify row that FAILs when
   it is missing, the same day. A hand fix that lives only on the box comes back on the next notebook.
2. **A box is production-ready only when verify-strih passes the operator items** (item 43, below)
   with OBS running and the panel live, not only the services. Run `sudo ./scripts/verify-strih.sh
   --box <box>` on the box with the operator's devices plugged in.

The audit of every known notebook fix against setup + verify is on the ticket (comment
6126959724). Its open gaps: the X hang root cause, the rtl8852bu Bluetooth root cause, the TLS front
end (issue 1399, now setup-strih step 16i + verify item 45, `strih-https.md`), the hub start order, and the imag hand tweaks.

## The lib (`scripts/lib/strih-operator.sh`, sourced by strih-provision.sh)

**setup-strih step 16g** (`strih_operator_install`, every role):
- `[BasicWindow]` `SaveProjectors=true`, `ProjectorAlwaysOnTop=false`, `MultiviewMouseSwitch=true`
  in the operator's OBS user.ini (`STRIH_OPERATOR_INI_KEYS`). The step-7 SaveProjectors upsert moved
  here, so there is ONE [BasicWindow] seeder.
  - It never writes while OBS runs: OBS rewrites user.ini from memory on exit. A WARN names it, and
    item 43 FAILs until a run with OBS stopped (the genlock deploy stops OBS before setup-strih).
  - It never clobbers an unparseable file, refuses a symlinked user.ini (setup runs as root), and
    replaces the file through a temp file + rename, keeping its mode.
  - `MultiviewMouseSwitch` defaults to true in OBS (`OBSApp.cpp`), so a `false` was written into the
    file. The 11.10 backup restore of user.ini is the likely source (unconfirmed).
- **Every OTHER wired NetworkManager profile on the rig subnet:** `connection.autoconnect no`, then
  `connection down` when it is active.
  - This is the issue-1411 dual-homed NIC: the onboard DHCP profile won the routes and the fleet
    clock broke for ~22 h.
  - The subnet rule is `strih_nic_rig_subnet_others` in strih-nic-driver.sh, the ONE rule verify
    item 36's `(nic-homed)` row uses.
  - The rig NIC's own profile (its active device, or a `connection.interface-name` = the rig NIC) is
    never touched.
  - Nothing is touched while the rig address is not on the rig NIC: the subnet is unknown then.
  - A non-wired connection on the subnet (WiFi) and an address no NetworkManager connection runs are
    named in a WARN, never changed.
  - An INACTIVE DHCP wired profile has no address to place (the 11.10 onboard profile was one, it
    landed on the rig subnet when a cable went in): it is named in a WARN, never changed blind; item 36
    FAILs it once it comes up there.
  - Every `nmcli` call inside the profile loop reads `</dev/null`, so none can eat the loop's rows.
  - Caveat: a setup run over an ssh session to the OTHER interface's address cuts its own session.
    The genlock deploy dials STRIH_IP.

**verify-strih item 43** (`strih_operator_grade_report`, 14 rows, FAIL unless noted). Every X read runs
on `:0` as the operator with their Xauthority (as root through `sudo -u`) and is bounded by `timeout`,
so a hung X server FAILs a row instead of hanging verify.

| Row | Grades |
|---|---|
| `(tap-to-click)` | the LIVE `libinput Tapping Enabled` = 1 on every touchpad (`xinput list-props`); a touchpad without the libinput property FAILs, a device whose properties do not answer is named; NOTE with no touchpad; a missing `xinput` is named (the kiosk installs it), never read as a hung X |
| `(bluetooth)` | REPORT-ONLY on every role since the owner's 16.10.2026 ruling ("ok nemusis to riesit netreba mys": the operator needs no Bluetooth mouse; strih-snv's rtl8852bu ROM v3 has no eco-4 Linux firmware upstream, issue 1418): a NOTE naming the state, never a FAIL. Before: `hciconfig hci0`: UP and a non-zero BD address. Graded on the strih role only (setup-strih step 11 passes `keep-bluetooth` to the kiosk, test-pinned); a NOTE on the stream role (owner 13.10.2026: "na stream-pp netreba bluethooth ani na stream snv, iba na strih-snv treba"; stream-pp's hci0 is rfkill soft-blocked and stays so). NOTE when the kernel registered no adapter at all (no `/sys/class/bluetooth/hci*`: strih-pp is a desktop with a USB receiver mouse) |
| `(stream-deck)` | the deck on USB AND at least one Satellite surface, read through `scripts/strih_satellite_watch.py` (`STREAM_DECK_USB_IDS`, `deck_on_usb`, `fetch_json` + `surface_names` on `/api/surfaces`); NOTE with no deck plugged in; NOTE on the stream role (step-16 gate) |
| `(mv-click)` | `[BasicWindow] MultiviewMouseSwitch` true the way libobs `config_get_bool` reads it (`util/config-file.c`: `true` in any case, or a non-zero strtoull, base 16 after a leading `0x`; `on` / `yes` read FALSE); an absent key is OBS's built-in true but FAILs as "not seeded" |
| `(operator-ini)` | every `STRIH_OPERATOR_INI_KEYS` key=value in `[BasicWindow]` (booleans case-insensitive): the three issue-1418 keys plus strih-snv's `CloseExistingProjectors=true` and `WarnBeforeStartingStream` / `WarnBeforeStoppingStream` / `WarnBeforeStoppingRecord` = false (issue 1419) |
| `(studio-mode)` | `GetStudioModeEnabled` over the OBS websocket; NOTE while OBS is not running |
| `(ws-seed)` | the step-7 obs-websocket config: `server_enabled` true, `server_port` 4455, `auth_required` false (pinned to the step-7 heredoc by a test) |
| `(qt6-svg)` | the `qt6-svg-plugins` package installed with BOTH plugins (item 21 grades only the icon engine) |
| `(operator-input)` | X lists a real keyboard AND a real pointer (never XTEST, never the ACPI buttons, `STRIH_OPERATOR_NOT_A_KEYBOARD`) |
| `(panel-gpu)` | `xrandr --listproviders` on :0 (bounded 10 s): every provider after Provider 0 that lists `Sink Output` (the reverse-PRIME panel sink) must be named `modesetting`; any other name is a vendor DDX and FAILs naming `xserver-xorg-video-amdgpu`, which the shared baseline removes (`STRIH_OPERATOR_AMDGPU_DDX_PKG`, pinned equal to `obs_box_amdgpu_ddx_package`); NOTE with one provider; a failed or timed-out read = a hung X; a missing `xrandr` (rc 127) is named |
| `(drm-hang)` | `journalctl -k -b 0 --no-pager` (bounded 30 s): a `blocked for more than` hung task whose trace within 25 lines (before the next `INFO: task`) has a `commit_work+0x` or `drm_atomic_helper_wait_for_fences+0x` frame, or a DRM `*ERROR*` commit timeout (`flip_done` / `hw_done` / `commit wait` timed out, amdgpu_dm `Waiting for fences timed out`, nvidia-drm `Flip event timeout`), FAILs naming its first time stamp; an unreadable or empty log FAILs, and so does one with no kernel line (`-- No entries --` when run outside root / adm / systemd-journal) |
| `(operator-panel)` | two `ffmpeg -f x11grab -i :0.0 -frames:v 1 -f md5 -` frames 4 s apart differ while OBS runs (the 12.10 hang signature); NOTE while OBS is not running |
| `(mv-projector)` | a `Projector - Multiview` window in `wmctrl -lx` on :0 while OBS runs (issue 1419: strih-pp's operator had no Multiview; step 16g's `strih_operator_projector_seed` runs `scripts/strih_projector_seed.py`, which seeds `{"monitor": 0, "type": 4}` into an EMPTY `saved_projectors` of the box's collection only, with OBS stopped); NOTE while OBS is not running; on the stream role (one screen carrying the stream OBS + the Restreamer dashboard, 16.10.2026) step 16g seeds NO projector and the row FAILs an open `Projector - Multiview` window, OK without one (issue 1469: the stream-snv conversion had seeded one and verify only printed a NOTE) |

**The 11/12.10.2026 X freeze (comments 6132168799, ROZHODNUTE 6132283178).** The panel's amdgpu
`commit_work` blocked in `drm_atomic_helper_wait_for_fences` from 11.10 22:17, 18 h before the owner
saw the frozen panel. strih-snv runs reverse PRIME: NVIDIA renders the X screen, the AMD Radeon 780M
scans the panel out as a sink, so each panel flip waits on an NVIDIA fence. The returned Intel
notebook had the SAME topology with the sink on the Xorg `modesetting` DDX and was stable; on
strih-snv Xorg ran `xf86-video-amdgpu` for the sink. The cure is therefore the shared baseline
(`obs-box-baseline.md`, the `igpu-sink` item): it REMOVES the amdgpu DDX package on every NVIDIA box,
because its own `10-amdgpu.conf` beats any xorg.conf.d OutputClass override (tried live, a dead end).
Applied by hand on strih-snv on 12.10.2026 23:00: the sink reads `name:modesetting` and the operator
rows pass. The BIOS discrete-GPU mode is only a fallback if the modesetting sink hangs too.
- `(panel-gpu)` reads the RUNNING provider name, so it FAILs until X restarts after the removal.
  The provider name is the DDX's: `modesetting` for the modesetting DDX, the vendor's own name
  (`AMD Radeon 780M Graphics @ pci:...`) for xf86-video-amdgpu.
- `(drm-hang)` catches the next hang in the boot it starts, not at the event. Only a reboot clears a
  hung commit (the worker sits in D state).
- Seams: `STRIH_OPERATOR_XRANDR` and `STRIH_OPERATOR_KLOG` read fixture files instead of the live
  tools; under `tests/python/fixtures/strih_operator_1418/` the provider list is the strih-snv text
  verbatim and the commit-hang log is a reconstruction of the abridged trace in the comment (its
  first line says which parts are verbatim).

Known limit of `(operator-panel)`: a truly still screen (no meters, no clock, no live camera) reads
as frozen. The strih panel always has live Multiview tiles, so this has not happened.

**Verify item 16 (the intercom hub)** stays in verify-strih:
- an enabled + active hub passes;
- inactive, disabled or not installed FAILs on the strih role (`obs_box_step_applies 13-hub-live`);
- a crash loop FAILs on every role;
- the stream role keeps the report-only note.

Setup and verify agree on it: setup-strih STARTS a stopped hub on the strih role right after step 14
(the same `13-hub-live` gate, `systemctl is-active` first, never a restart of a running hub). It runs
there because the unit's `LoadCredential` needs the room secret step 14 writes: a start in step 13 on
a fresh Janus box looped on 243/CREDENTIALS while `systemctl start` (a simple service) returned 0. So
the start is confirmed by `strih_intercom_hub_crash_loop`, and a loop fails setup by name. The old enable-only
rule was the parallel-run contract; `tests/intercom_hub_provisioning.rs` now pins the role split.

**The X reads** go through ONE helper, `strih_kiosk_x_run EUID USER HOME SECONDS CMD...` in
`scripts/lib/strih-session-apps.sh` (`sudo -u` the operator as root, `DISPLAY=:0`, their `.Xauthority`,
`timeout`), shared with the session apps' window check. `strih-drm-output.sh` keeps its own xrandr
reader because its tests source that lib alone.

## The tap-to-click finding (code only, the box was not read)

The baseline writes `/etc/X11/xorg.conf.d/30-touchpad-tap.conf` (`MatchIsTouchpad`, `Driver
"libinput"`, `Tapping on`). The stock `40-libinput.conf` sets no Tapping, so nothing in the repo
turns tapping off. The baseline row only checks the FILE. The cause on 12.10 is not visible in the
code. To find it on the box:
- read the X log for the touchpad's `Applying InputClass` lines (is our class applied to the device?);
- read `xinput list-props` (is the device a touchpad to libinput at all?).
The new `(tap-to-click)` row grades the live value whatever the cause.

## Tests (Tier-0, pytest)

`tests/python/test_strih_operator_1418.py`: the real lib under the callers' `set -euo pipefail` with
these fakes:
- for the setup half: a fake NetworkManager (one directory per connection) and a fake `ip -o -4 addr
  show` file;
- for the grader: fake `xinput` / `hciconfig` / `dpkg-query` / `sudo` / Studio Mode reader / grab
  command, a fake sysfs USB tree, and a real local HTTP server for `/api/surfaces`;
- verify item 16's real text sliced from verify-strih, with a fake `systemctl`.
Seams are listed in the lib header.

## The full strih OBS setup from the repo (issue 1419, owner 13.10.2026)

Owner verbatim: "ale ved tam malo byt cely obs setup skopirovany aky mame v strih snv". strih-snv's OBS
setup was hand-made; the live diff (13.10.2026) found the collection and the plugin set already equal
on strih-pp, and three gaps, now all seeded by setup-strih with OBS stopped and graded by verify:
- the profile: `obs_box_stream_profile.py --strih` (step 3, strih role) writes `BASIC_INI` +
  `STRIH_PROFILE` (record keys = `strih_lx_profile_facts`, test-pinned) + `recordEncoder.json` (NVENC
  CBR 20000 p5); verify item 10's `(strih-profile)` row runs `--grade-strih`;
- user.ini: the four keys above in `STRIH_OPERATOR_INI_KEYS`, graded by `(operator-ini)`;
- the saved Multiview projector, graded live by `(mv-projector)`.
Never copied: window geometry, the Panels cookie id, the rtmp service key, the stale Windows-era
advanced-scene-switcher module data, and the transition duration (an operator choice).

## The operator keyboard: us + Slovak, Alt+Shift (owner 14.10.2026)

"potrebujeme slovensku klavesnicu vediet sa prepnut linuxoch": every Linux workstation box (strih-snv,
strih-pp, stream-pp) types `us,sk` (Slovak QWERTZ) and switches with **Alt+Shift**
(`grp:alt_shift_toggle`).
- **Setup** (step 16g, inside `strih_operator_install`): `strih_operator_keyboard_install` writes
  `XKBLAYOUT` / `XKBOPTIONS` into `/etc/default/keyboard` (X reads it at its start; the other keys
  stay; a missing file is created) and applies it to the running `:0` with `setxkbmap`. A file it
  cannot write is a WARN there, so the rig-subnet isolation after it still runs; verify FAILs it.
- **Verify** (item 43, the `keyboard` row): the file AND the live `setxkbmap -query` on `:0` must
  both carry the layout and the option; X not answering FAILs the row.
- Seams: `STRIH_OPERATOR_KEYBOARD_FILE`, `STRIH_OPERATOR_SETXKBMAP`. A test that runs
  `strih_operator_install` MUST set both, or it writes this box's real file and changes the
  keyboard of dev1's own display.
- Applied live on 14.10.2026 to strih-pp, stream-pp and strih-snv before the code landed.

**ibus overwrote it at every login (issue 1441, owner 21.10.2026 on stream-pp: the panel item showed
US and a click never switched to SK).** im-config starts `ibus-daemon --xim` in the openbox session.
With `org.freedesktop.ibus.general use-system-keyboard-layout false` (the default), ibus applies its
own engine layout (`xkb:us::eng`) to X at login. :0 then carries ONE group, so the panel's
`obs_box_kbd_layout.py next` locks US again (journal `layout US -> US`). The file and `localectl` still
read `us,sk`, so only a LIVE read shows it. All three boxes had it on 21.10.2026.
- The keyboard code lives in its own lib `scripts/lib/strih-keyboard.sh` (moved verbatim from
  strih-operator.sh for its line budget; strih-operator.sh sources it, same function names).
- Setup: `strih_operator_ibus_keep_layout` sets the key `true` for the operator BEFORE the live
  `setxkbmap` (skipped without ibus, already-true is a no-op, a failed set is a WARN). gsettings runs
  as the operator over their session bus `/run/user/<uid>/bus` when it exists, so the live ibus takes
  it at once, else over a private `dbus-run-session` bus.
- Verify: the `keyboard` row takes the ibus state as a third input. `false` FAILs naming the key
  (checked before the live layout, it is the cause), an unreadable or non-boolean value FAILs, a box
  without the ibus schema or without gsettings is graded on the file + live layout only.
- Live fix by hand on stream-pp 21.10.2026 17:40 (gsettings + setxkbmap); strih-pp and strih-snv
  get it at their next setup-strih run.
- Seams: `STRIH_OPERATOR_GSETTINGS`, `STRIH_OPERATOR_RUN_USER_DIR`. A test that runs the install must
  also set `STRIH_OPERATOR_GSETTINGS`, or it changes dev1's own ibus setting. Tests:
  `tests/python/test_strih_keyboard_ibus_1441.py` (the issue-1418 fixture with fake gsettings and
  dbus-run-session).
