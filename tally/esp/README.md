# Camera tally light firmware (ESP8266) — issue 1417

`WsTally/WsTally.ino` is the firmware of the camera tally lights: an ESP8266 (Wemos D1 style) with a
7-pixel NeoPixel ring. It is the owner's WsTally firmware, unchanged except that the venue WiFi
credentials moved into a git-ignored `secrets.h`.

- **Tally id = camera number.** The four DIP switches (pins 16 / 14 / 12 / 13) are read at power-on
  and ADDED as 1 / 2 / 3 / 4; the light blinks red that many times. Set them to the camera the
  light sits on.
- The firmware connects to `atemrpi.local:9240`, path `/`, as a WebSocket client (reconnect every
  5 s, heartbeat 15 s / 3 s / 2 misses). Both the name and the port are hard-coded: the server side
  announces exactly that name.
- **The server is `strih-tally`** on each Linux strih box (strih-snv, strih-pp): a relay on :9240
  plus a client of the strih OBS, which sends the program/preview camera. See
  `.claude/rules/strih-tally.md` and `scripts/strih_tally.py`.

## Build

1. Arduino IDE with the ESP8266 board core (Boards Manager: "esp8266 by ESP8266 Community").
2. Libraries: **WebSockets** by Markus Sattler (`WebSocketsClient.h`) and **Adafruit NeoPixel**.
3. `cp WsTally/secrets.h.example WsTally/secrets.h` and fill in the venue tally WiFi.
   `secrets.h` is git-ignored: never commit it, never paste its values into a ticket or a log.
4. Open `WsTally/WsTally.ino` (the sketch folder name must match), pick the board, upload.

## Protocol (plain-text WebSocket frames, one command and an optional argument)

| Frame | Direction | Meaning |
|---|---|---|
| `JOIN <hostname>` | tally -> server | sent on connect; hostname = the ESP default hostname + `-<tally id>` |
| `TALLY-PGM <n>` | server -> tally | program camera n (0 = none); the previous program camera is kept as the transition camera |
| `TALLY-PVW <n>` | server -> tally | preview camera n (0 = none) |
| `TALLY-TRS` | server -> tally | transition in progress: red when the id is program, preview or the previous program |
| `TALLY-LOCK 1\|0` | any client -> tally | yellow lock on / off |
| `SET-LUMINANCE <50..255>` | any client -> tally | brightness, stored in EEPROM |

Colour after PGM/PVW: red when program == id, else green when preview == id, else gray (one LED).
Status blinks: blue = no WiFi, orange = no WebSocket. The server sends `TALLY-PVW` before
`TALLY-PGM`, because the firmware stores the old program camera on every `TALLY-PGM`.

## The PP light build (`TallyPP/TallyPP.ino`)

The source of the firmware the Poprad lights run, as the owner supplied it on 30.10.2026, unchanged
(the WiFi `SSID` / `PASS` are empty in git; fill them locally before a build, never commit them).
It differs from `WsTally` in two places: it dials `tally.lan:9240` instead of `atemrpi.local`, and the
WiFi values sit in `#define SSID` / `PASS` instead of `secrets.h`. Its parser reads `TALLY-PVW` /
`TALLY-PGM` (the `...2` 2ME twins the relay also sends since 14.10.2026 are ignored by this build).
