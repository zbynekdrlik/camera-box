# stream-output-stuck-alert-watchdog — install note (issue 1460)

The owner's ruling (ROZHODNUTÉ 12.11.2026 on issue 1460), verbatim: *„na discord do alerts keď je
scéna PRO na stream obs a je zapnuté streamovanie, vtedy má prísť správa do discordu, že sa zasekol
výstup"*.

`scripts/stream-output-stuck-alert-watchdog.sh` pages **only** while all of these hold on the stream
OBS (`stream-snv`, address from `scripts/lib/obs-fleet.sh`, obs-websocket :4455):

1. the **program scene** is the production scene (`STREAM_PRODUCTION_SCENE_DEFAULT` in
   `scripts/lib/stream-dev-scene.sh` = `PRO`), read before AND after the window;
2. the **stream output is active** (`GetStreamStatus.outputActive`);
3. the **output is stuck** — over a ~3 s window inside one WS session:
   - `render`: `GetStats.renderTotalFrames` did not advance (the program picture froze),
   - `encode`: `GetStreamStatus.outputTotalFrames` did not advance (the stream sends no frames),
   - `bytes`: `GetStreamStatus.outputBytes` did not advance (nothing leaves OBS),
   - `reconnecting`: `GetStreamStatus.outputReconnecting` is true;
4. … and that was seen on **2 consecutive passes** (60 s apart).

Outside that state it never pages: a development stream on `Development` or a stopped stream only
log. An unreachable box (SKIP) or an unread fact (UNKNOWN, e.g. a request timed out) is never a
guessed production, with ONE exception: within `STREAM_OUTPUT_STUCK_CARRY_S` (600 s) of a pass that
PROVED production + streaming, a stream OBS that stops answering reads STUCK `unresponsive` and pages
after the same 2-pass confirm -- a frozen OBS answers nothing, and the network-reach watchdog still
sees a box that pings. A silent program while streaming is the separate issue-1495 page
(`measurement-audio-alert-watchdog.sh`, `page:stream-snv-program-audio`).

## Pieces

- `scripts/stream_output_stuck_probe.py` — READ-ONLY probe (`GetCurrentProgramScene`,
  `GetStreamStatus`, `GetStats` through `obs_phase2._conn/_rpc`; it never selects a scene, so the
  issue-1380 `PRO` guard is never bypassed). Prints `key=value` lines; exit 0 iff the WS connected.
- `scripts/stream_output_stuck_decision.py` — the PURE decision (pytest:
  `tests/python/test_stream_output_stuck_1460.py`).
- `scripts/stream-output-stuck-alert-watchdog.sh` — confirm (`obs-watchdog-decision.sh`), state
  (`watchdog-common.sh`), page.

## Delivery

`airuleset.py notify --dedup-key page:stream-snv-output-stuck:<bucket>` — the airuleset `page:` class
(issue 1495, airuleset 0.1.626), the ONLY class the owner policy lets through to the owner's phone,
at most one message per check per 10 min while it lasts. Recovery is one machine-channel log line,
never a phone ping (#1206).

## It ships DISABLED — the supervisor's steps

The units are committed but NOT installed or enabled by this change.

1. Dry-run against the live box (read-only, never pages):
   `scripts/stream-output-stuck-alert-watchdog.sh --dry-run` — expect
   `verdict=NOT_PRODUCTION` (program `Development`, or not streaming) or `verdict=OK` during a
   production; never `UNKNOWN` on a healthy box.
2. Fixture replay of the page path (no box): a fetch script printing a stuck production body, run
   twice with `STREAM_OUTPUT_STUCK_FETCH_CMD=<script> --dry-run` and a scratch
   `STREAM_OUTPUT_STUCK_ALERT_STATE_DIR` — the 2nd pass logs `WOULD page (page:stream-snv-output-stuck)`.
3. Install + enable:
   ```bash
   cp systemd/stream-output-stuck-alert-watchdog.{service,timer} ~/.config/systemd/user/
   systemctl --user daemon-reload
   systemctl --user enable --now stream-output-stuck-alert-watchdog.timer
   ```
4. Read the first pass: `journalctl --user -u stream-output-stuck-alert-watchdog.service --since -5min`
   must show the script's own `pass end` line, never `status=203/EXEC`.
5. The timer is already on `scripts/lib/watchdog-roster.sh`, so until step 3 the development-handover
   check reports it as not enabled -- that is the reminder, by design.

## Known limits

- A frozen PICTURE whose counters still advance (the `NDI 2ME PGM` receiver repeating one frame into a
  rendering, encoding, sending OBS) is not a stuck output here — that is the frozen-input watchdog's
  territory.
- `bytes` advances as long as OBS writes to its local RTMP target (the restreamer on the box); a dead
  YouTube egress behind the restreamer is the restreamer's own alarm.
- Only `stream-snv`; another stream box is `STREAM_OUTPUT_STUCK_BOX=<box>` in a second unit (the
  page key carries the box name, which must be a valid airuleset check name; the script refuses
  anything else).
- The `unresponsive` carry stops 600 s after the last proven production pass; a box frozen longer
  than that stays paged only through the confirm already latched, not re-proven.
