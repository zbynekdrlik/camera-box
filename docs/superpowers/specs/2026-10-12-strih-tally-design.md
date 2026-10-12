# strih-tally — camera tally lights driven by the strih OBS (issue 1417)

Status: approved design (owner, 12.10.2026). Implementation follows a written plan.

## Goal

The ESP8266 tally lights on the cameras show the strih's program (red) and preview (green) camera,
taken directly from the strih OBS. Today they depend on WsTally, a Windows/Node relay fed by Bitfocus
Companion; on 12.10.2026 nothing answered as `atemrpi.local:9240` in SNV or PP, so the tallies run
nowhere. The replacement runs on both Linux strih boxes (strih-snv, strih-pp) and lives in this repo.

## Owner decisions (issue 1417)

- Tally N lights when program/preview is the scene `Cam N` OR a scene that contains exactly ONE
  camera N (e.g. `Moderatori` -> cam3). A scene with several cameras (`MULTIVIEW`) lights nothing.
- Tally id (the ESP's DIP switches) = camera number.
- A standalone Python service per strih (the `strih-browser-keeper` pattern). Rejected: a new Rust
  program (a CI artifact + deploy path for ~300 lines, no Rust OBS client in the repo) and a module
  inside intercom-hub (a tally bug must never take the intercom audio down).
- The ESP firmware stays unchanged.

## The protocol (read from the WsTally source the owner uploaded)

ESP8266 firmware (`WsTally.ino`), a WebSocket client:

- connects to `atemrpi.local:9240`, path `/`, reconnect every 5 s, WS heartbeat 15 s / 3 s / 2 misses;
- on connect sends `JOIN <hostname>` (hostname = the ESP's default hostname + `-<tally id>`);
- receives plain text, a command and an optional argument separated by one space:
  - `TALLY-PGM <n>` — program camera; `trs` = the previous program camera;
  - `TALLY-PVW <n>` — preview camera;
  - colour after either: red if pgm == id, else green if pvw == id, else gray (one LED);
  - `TALLY-TRS` — red when id is pgm, pvw or the previous pgm (a transition in progress);
  - `TALLY-LOCK 1|0` — yellow lock on/off;
  - `SET-LUMINANCE <50..255>` — brightness, stored in EEPROM;
- status blinks: blue = no WiFi, orange = no WebSocket.

The server side (`services/ws.js`) is a relay: every text a client sends is forwarded to every
OTHER client. Companion was one such client. The rest of WsTally (`app.js`, KNX lighting) is not
part of tally and is not ported.

## Architecture

One process per strih, `strih-tally` (Python 3, asyncio, `python3-websockets` from Ubuntu):

1. **Relay server** on `0.0.0.0:9240`. Accepts any WS client; logs `JOIN` lines with the peer IP.
   Forwards every received text to all other clients (Companion can still send `SET-LUMINANCE`,
   `TALLY-LOCK`). On a new connection it immediately sends the current `TALLY-PVW`/`TALLY-PGM`. It
   resends the current state to all clients every 10 s, so a lost frame heals itself.
2. **OBS client** to `ws://127.0.0.1:4455` (the strih OBS runs without WS auth). On connect it reads
   the studio mode, the program and preview scenes and the scene tree. It subscribes to
   program/preview scene changes, transition start/end, studio mode changes and scene-item changes
   (create, remove, enable-state, scene list), and recomputes on each.
3. **Pure decision module** `scripts/strih_tally_decision.py`, no I/O:
   - `camera_of_scene(scene, tree, inputs) -> int`: walks the scene recursively (nested scenes and
     groups), keeps ENABLED items only, maps each `ndi_source` input to a camera by its NDI source
     name `CAM<n> (...)` (never by the OBS input name). Exactly one distinct camera -> n, else 0.
     Cycle-guarded.
   - `messages(prev_state, new_state) -> [str]`: the texts to send for a state change, `TALLY-PVW`
     before `TALLY-PGM` (the firmware keeps the previous pgm as `trs` on PGM); nothing when unchanged.
   - Studio mode off -> preview 0.
4. **mDNS name**: `avahi-publish -a -R atemrpi.local <the box's STRIH_IP>` as its own unit
   `strih-tally-mdns.service` (a name collision with another device is logged loud and graded).

Transitions: on `SceneTransitionStarted` send `TALLY-TRS`; the program change that follows sends the
new PGM. OBS unreachable: the relay keeps running, the client retries every 2 s; after 10 s without
OBS it sends `TALLY-PVW 0` + `TALLY-PGM 0` once (lights gray, no stale on-air red) and logs it.

## Deployment

- Units: `strih-tally.service` and `strih-tally-mdns.service`, system units running as the operator
  user, `Restart=always`, enabled. Installed by setup-strih as a new step, only on the `strih` role
  (never stream-pp), through a new lib `scripts/lib/strih-tally.sh` (install + grade). setup-strih and
  verify-strih are at their 1000-line budget: the new step and check are one call each, and existing
  text moves into a lib to make room.
- verify-strih grades: both units active + enabled, `:9240` listening, `atemrpi.local` resolving to
  the box (and to nothing else), and the service's reported state equal to OBS's current
  program/preview camera (a small `/state` HTTP read or a log line; decided in the plan).
- Firmware: `tally/esp/WsTally.ino` with the WiFi credentials moved to a gitignored `secrets.h`
  (a committed `secrets.h.example`). No firmware behaviour change.

## Logging

Every state change (`pgm a->b`, `pvw c->d`, transition), every tally JOIN/leave with IP, every OBS
connect/disconnect, the 10-s gray fallback. One line each, journald.

## Testing (Tier-0, pytest)

- Decision module over fixtures shaped like the real strih collection: `Cam N`, `Moderatori` (cam3 +
  a browser source), `MULTIVIEW` (several cameras -> 0), `Interkom` (no camera -> 0), nested scenes,
  groups, disabled items, a non-camera NDI source (`CG-obs`), a scene cycle.
- The service end to end against a fake OBS WS server and fake tally clients: initial state on
  connect, a program change, a preview change, a transition, studio mode off, the relay forwarding a
  client text to the others but not back, the 10-s resend, OBS loss -> gray after 10 s, OBS back.
- No live rig in tests; the first live check is verify-strih on both strihs after install.

## Out of scope

- The KNX lighting bridge from WsTally.
- Firmware changes (multi-camera tally, new hostname).
