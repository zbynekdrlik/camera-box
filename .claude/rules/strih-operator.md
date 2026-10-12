---
paths:
  - "scripts/lib/strih-operator.sh"
  - "scripts/setup-strih.sh"
  - "scripts/verify-strih.sh"
  - "tests/python/test_strih_operator_1418.py"
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
end (issue 1399), the hub start order, and the imag hand tweaks.

## The lib (`scripts/lib/strih-operator.sh`, sourced by strih-provision.sh)

**setup-strih step 16g** (`strih_operator_install`, every role):
- `[BasicWindow]` `SaveProjectors=true`, `ProjectorAlwaysOnTop=false`, `MultiviewMouseSwitch=true`
  in the operator's OBS user.ini (`STRIH_OPERATOR_INI_KEYS`). The step-7 SaveProjectors upsert moved
  here, so there is ONE [BasicWindow] seeder.
  - It never writes while OBS runs: OBS rewrites user.ini from memory on exit. A WARN names it, and
    item 43 FAILs until a run with OBS stopped (the genlock deploy stops OBS before setup-strih).
  - It never clobbers an unparseable file.
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
  - Caveat: a setup run over an ssh session to the OTHER interface's address cuts its own session.
    The genlock deploy dials STRIH_IP.

**verify-strih item 43** (`strih_operator_grade_report`, 9 rows, FAIL unless noted). Every X read runs
on `:0` as the operator with their Xauthority (as root through `sudo -u`) and is bounded by `timeout`,
so a hung X server FAILs a row instead of hanging verify.

| Row | Grades |
|---|---|
| `(tap-to-click)` | the LIVE `libinput Tapping Enabled` = 1 on every touchpad (`xinput list-props`); a touchpad without the libinput property FAILs; NOTE with no touchpad |
| `(bluetooth)` | `hciconfig hci0`: UP and a non-zero BD address. Graded because the kiosk keeps bluetooth (`STRIH_OPERATOR_KIOSK_BLUETOOTH`, test-pinned to the step-11 `obs_box_kiosk ... keep-bluetooth` call) |
| `(stream-deck)` | the deck on USB AND at least one Satellite surface, read through `scripts/strih_satellite_watch.py` (`STREAM_DECK_USB_IDS`, `deck_on_usb`, `fetch_json` + `surface_names` on `/api/surfaces`); NOTE with no deck plugged in; NOTE on the stream role (step-16 gate) |
| `(mv-click)` | `[BasicWindow] MultiviewMouseSwitch` true (read the way libobs `config_get_bool` reads it) |
| `(studio-mode)` | `GetStudioModeEnabled` over the OBS websocket; NOTE while OBS is not running |
| `(ws-seed)` | the step-7 obs-websocket config: `server_enabled` true, `server_port` 4455, `auth_required` false (pinned to the step-7 heredoc by a test) |
| `(qt6-svg)` | the `qt6-svg-plugins` package installed with BOTH plugins (item 21 grades only the icon engine) |
| `(operator-input)` | X lists a real keyboard AND a real pointer (never XTEST, never the ACPI buttons, `STRIH_OPERATOR_NOT_A_KEYBOARD`) |
| `(operator-panel)` | two `ffmpeg -f x11grab -i :0.0 -frames:v 1 -f md5 -` frames 4 s apart differ while OBS runs (the 12.10 hang signature); NOTE while OBS is not running |

Known limit of `(operator-panel)`: a truly still screen (no meters, no clock, no live camera) reads
as frozen. The strih panel always has live Multiview tiles, so this has not happened.

**Verify item 16 (the intercom hub)** stays in verify-strih:
- an enabled + active hub passes;
- inactive, disabled or not installed FAILs on the strih role (`obs_box_step_applies 13-hub-live`);
- a crash loop FAILs on every role;
- the stream role keeps the report-only note.

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
