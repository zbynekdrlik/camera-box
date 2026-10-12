---
paths:
  - "scripts/strih_tally.py"
  - "scripts/strih_tally_decision.py"
  - "scripts/lib/strih-tally.sh"
  - "systemd/strih-tally*"
  - "tally/**"
  - "tests/python/test_strih_tally_*"
---

# strih-tally: the camera tally lights driven by the strih OBS (issue 1417)

Owner, 12.10.2026: the ESP8266 tally lights on the cameras show the strih's program (red) and preview
(green) camera, taken directly from the strih OBS. Before, they hung on WsTally, a Windows/Node relay
fed by Bitfocus Companion. That day nothing answered as `atemrpi.local:9240` in SNV or PP, so the
tallies ran nowhere. Spec: `docs/superpowers/specs/2026-10-12-strih-tally-design.md`. Plan:
`docs/superpowers/plans/2026-10-12-strih-tally.md`.

## The pieces

| File | What |
|---|---|
| `scripts/strih_tally_decision.py` | pure: scene tree -> camera; state diff -> messages |
| `scripts/strih_tally.py` | the service: the :9240 relay + `GET /state`, the obs-websocket client, the resend + gray loops |
| `systemd/strih-tally.service.in` | system unit, `User=@USER@` filled with the operator at install |
| `systemd/strih-tally-mdns.service` | `avahi-publish -a -R atemrpi.local ${STRIH_TALLY_IP}` from `/etc/default/strih-tally` |
| `scripts/lib/strih-tally.sh` | setup-strih step 16f (install) + verify-strih item 42 (4 grade rows) |
| `tally/esp/WsTally/WsTally.ino` | the firmware, unchanged except the WiFi credentials (below) |

## The protocol (the firmware's, never change it)

The firmware hard-codes `atemrpi.local`, port `9240`, path `/`, plain-text frames, a command and an
optional argument separated by one space.

| Frame | Direction | Meaning |
|---|---|---|
| `JOIN <hostname>` | tally -> server | on connect; hostname = ESP default + `-<tally id>`; logged with the IP |
| `TALLY-PVW <n>` | server -> tally | preview camera n, 0 = none |
| `TALLY-PGM <n>` | server -> tally | program camera n, 0 = none; the firmware keeps the OLD program as `trs` |
| `TALLY-TRS` | server -> tally | transition running: red on pgm, pvw and the previous pgm |
| `TALLY-LOCK 1\|0`, `SET-LUMINANCE <50..255>` | any client | yellow lock / brightness (EEPROM) |

- **Order: `TALLY-PVW` before `TALLY-PGM`** on every change and every resend, because `TALLY-PGM`
  shifts the stored program into `trs`.
- **Tally id = camera number.** The four DIP switches are ADDED as 1 / 2 / 3 / 4 at power-on (so ids
  1..10); the light blinks red that many times.
- Status blinks: blue = no WiFi, orange = no WebSocket. An orange blink on a camera = nothing answers
  `atemrpi.local:9240` from where the light is (check the mDNS row and the unit).

## The owner's scene rule (`strih_tally_decision.py`)

- A scene lights camera n when its ENABLED content, nested scenes and groups included, holds exactly
  ONE distinct camera n (`Cam 3`, or `Moderatori` = cam3 + a browser source). Several cameras
  (`MULTIVIEW`) or none (`Interkom`) light nothing (0).
- A camera is the NDI SOURCE name `CAM<n> (...)` of an `ndi_source` input, never the OBS input name.
- One `seen` set is shared across siblings: a scene reached twice is counted once, a cycle stops.
- Studio mode off -> preview 0.

## The service (`strih_tally.py`)

- **Relay:** a protocol text (JOIN, TALLY-*, SET-LUMINANCE) a client sends goes to every OTHER
  client, never back to the sender, and the server's own state is untouched by it (Companion can
  still send LUMINANCE / LOCK); any other text is dropped (logged once per client), and a frame over
  256 bytes closes its sender (the ESP8266 copies a whole frame into its small heap). JOIN names are
  cut to printable characters, 64 at most. A new client gets the current PVW + PGM at once (0/0
  before OBS was ever read); every client gets them again every 10 s, so a lost frame heals. One
  lock per connection keeps a PVW + PGM pair from interleaving with another send. A client whose send
  fails or takes over 2 s is dropped and logged.
- **OBS client** (`ws://127.0.0.1:4455`, no auth; a WS password makes it fail loud and retry):
  Identify with Scenes + Inputs + Transitions + SceneItems + Ui (1180). A program / preview / studio /
  event re-reads only those three (a cut must light quickly); a scene-item / scene-list / scene-name /
  input settings / input name event re-reads the whole tree (`GetSceneList`, `GetSceneItemList`,
  `GetGroupSceneItemList`, `GetInputList`, `GetInputSettings` per `ndi_source`). Requests go one at a
  time; events read meanwhile are queued and handled after, as ONE re-read (a collection load sends
  dozens).
- **Transitions** (review round 1): `SceneTransitionStarted` sends `TALLY-TRS`, and while it runs
  `TALLY-TRS` FOLLOWS every state change, and the 10 s resend sends only `TALLY-PVW` + `TALLY-TRS`
  (a resent `TALLY-PGM` would shift the stored program into `trs` again, review round 2): the firmware recolours on `TALLY-PGM`, so a
  TRS sent before the program change (OBS sends the start event first) would be undone at once.
  `SceneTransitionEnded` / `SceneTransitionVideoEnded` (or 15 s without either: an interrupted
  transition) sends the FULL state, which puts the lights back to red / green / gray. During a fade the
  firmware lights pgm, pvw and the previous pgm red (its `trs`).
- **OBS gone:** retry every 2 s (one log line per disconnect, not per retry); after 10 s without OBS
  `TALLY-PVW 0` + `TALLY-PGM 0` ONCE (no stale on-air red). A hung OBS (TCP open, nothing answering)
  counts as gone once the WS keepalive (5 s ping, 5 s timeout) drops it, so ~20 s in all. OBS back ->
  the new state as a diff.
- **`GET /state`** on :9240 (websockets `process_request`; a WS upgrade proceeds):
  `{pgm, pvw, program_scene, preview_scene, studio_mode, obs_connected, camera_inputs, tallies}`,
  camera_inputs = how many inputs map to a camera, tallies = `"<ip> <JOIN name>"`.
- Logs: every state change (`pgm a->b pvw c->d (program=… preview=…)`), JOIN / LEAVE with the IP, OBS
  connect / disconnect, the gray fallback.

## Install + grade (strih role only; never stream-pp)

- `obs_box_step_applies 16f` is in the strih-only case of `scripts/lib/obs-box-role.sh`.
- **setup-strih step 16f** (`strih_tally_install`): apt `python3-websockets python3-websocket
  avahi-utils` only when missing, an import preflight of `websockets.asyncio`, the two python files into
  `/usr/local/lib/camera-box` (flat, 0644), both units into `/etc/systemd/system`, the default file;
  each written only when it differs (`strih_session_apps_put`). Both units are hardened
  (`NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`); avahi-publish still runs
  as root (a `DynamicUser` needs the avahi D-Bus policy checked live first). Then daemon-reload after a write,
  `try-restart` of a unit whose files changed (only a running one), `enable --now` of a unit not yet
  enabled, else `start`. NOT enable-only: tally is production function with no TEST/EVENT dependency.
- **verify-strih item 42** (`strih_tally_grade_report`, 4 rows): `(tally-units)` enabled + active +
  files match; `(tally-port)` /state answers with `obs_connected=true` and at least
  one mapped camera input; `(tally-mdns)` `atemrpi.local` resolves to this box's STRIH_IP and nothing
  else (avahi-resolve returns one answer, so another device shows as its address); `(tally-obs)` the service's program scene =
  OBS's own `GetCurrentProgramScene` (read twice on a mismatch, a cut can land between the reads).
- **A second device answering `atemrpi.local` on the venue LAN breaks the tallies** (they may dial
  it): avahi-publish logs a collision and exits (Restart retries), and `(tally-mdns)` names the other
  address. The old WsTally host must stay off, or be renamed.
- **setup-strih.sh and verify-strih.sh are at their 1000-line budget.** Step 16f / item 42 are one
  gate + one lib call each; the room came from moving the 16e / 41 comment text into the
  strih-program-audio.sh header and putting their `else` branches on one line.

## The firmware credentials

`tally/esp/WsTally/WsTally.ino` reads `WIFI_SSID` / `WIFI_PASS` from `#include "secrets.h"`, which is
git-ignored; `secrets.h.example` is the committed template. The real venue WiFi values never go into
git, a ticket, a test or a log. Build steps: `tally/esp/README.md`.

## Check it live

- `curl -s http://<strih>:9240/state` while switching scenes in OBS (pgm / pvw follow).
- `journalctl -u strih-tally -n 50` (JOIN lines when a tally powers on) and `-u strih-tally-mdns`.
- `avahi-resolve -4 -n atemrpi.local` from another LAN host = the strih's IP only.
- `sudo ./scripts/verify-strih.sh --box <box>`: item 42 four PASS rows.

## Tests (Tier-0, pytest)

- `tests/python/test_strih_tally_decision_1417.py`, `test_strih_tally_service_1417.py` (real
  `websockets` server + clients against a fake OBS; `asyncio.run`, no pytest-asyncio),
  `test_strih_tally_install_1417.py` (the lib under `set -euo pipefail` with fake systemctl / apt /
  resolver / OBS reader and a real local HTTP server for /state), `test_strih_tally_firmware_1417.py`.
- CI's python-tests job installs `websockets==15.0.1` (the Ubuntu 26.04 version). dev1's system python
  has no websockets and PEP 668 refuses `pip --user`: run the service test from a scratch venv
  (`python3 -m venv --system-site-packages <dir>; <dir>/bin/pip install websockets==15.0.1`).
- A unit TEMPLATE's comment must not spell its own placeholder (`@USER@`): the render only fills the
  `User=` line, so a comment copy survives into the installed unit (caught by the first install test).
