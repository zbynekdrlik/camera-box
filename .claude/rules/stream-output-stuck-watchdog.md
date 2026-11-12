---
paths:
  - "scripts/stream-output-stuck-alert-watchdog.sh"
  - "scripts/stream_output_stuck_decision.py"
  - "scripts/stream_output_stuck_probe.py"
  - "systemd/stream-output-stuck-alert-watchdog.*"
  - "tests/python/test_stream_output_stuck_1460.py"
---

# The stuck-output page (issue 1460, owner ROZHODNUTÉ 12.11.2026)

Owner, verbatim: "na discord do alerts keď je scéna PRO na stream obs a je zapnuté streamovanie,
vtedy má prísť správa do discordu, že sa zasekol výstup". Page ONLY in that state, never outside it.

## What it reads

- `scripts/stream_output_stuck_probe.py`, READ-ONLY through `obs_phase2._conn/_rpc` (zero event
  subscription, never a scene selection, so the issue-1380 `PRO` guard stays the one choke point):
  the program scene, then snapshot 0 (`GetStreamStatus` + `GetStats`), the `--window-s` sleep (3 s),
  snapshot 1, the program scene again. A failed or timed-out request (3 s each) prints an EMPTY value.
- `scripts/stream_output_stuck_decision.py` (pure, pytest): SKIP (no WS) / UNKNOWN (an unread fact) /
  NOT_PRODUCTION (either scene read is not the production scene, or `outputActive` is not 1 in both
  snapshots) / OK / STUCK with reasons `render` (`renderTotalFrames` still), `encode`
  (`outputTotalFrames` still), `bytes` (`outputBytes` still), `reconnecting`. A counter that went
  BACKWARDS is a new stream session, never a stall. The production scene is passed in from
  `scripts/lib/stream-dev-scene.sh` (`STREAM_PRODUCTION_SCENE_DEFAULT`), never retyped.
- **The `unresponsive` carry -- HUNG, never ABSENT (review round 2):** a frozen OBS answers nothing
  and network-reach still sees a box that pings, so neither watchdog would page. The probe first does a
  plain TCP connect (5 s): refused / no route / timeout = exit 1 ABSENT; TCP up but the obs-websocket
  handshake times out = exit 2 HUNG; after the handshake the socket timeout is 3 s and every request
  that times out (`TimeoutError` / `WebSocketTimeoutException`, never a closed connection or an error
  reply) counts in `timeouts=`. `carry_unresponsive` turns SKIP (exit 2, or 124/137 = killed by the
  45 s bound) / UNKNOWN with `timeouts>0` into STUCK `unresponsive` ONLY within `--carry-s` (600 s) of
  a pass that PROVED production + streaming; the watchdog stores that proof as `last_production_<box>`
  (set by OK and counter-STUCK, cleared by NOT_PRODUCTION, never refreshed by a carried pass).
  A first cut carried a refused connection too and paged after an ordinary "stop stream, close OBS"
  end of show -- never carry an absence. Accepted gap: an OBS crash (port refused) is not this page.
- Live read 12.11.2026 (stream-snv not streaming): `render+90 frames+0 bytes+0` over 3 s: an idle
  stream's counters stand still, which is why the gate on `outputActive` in both snapshots matters.

## Delivery

- The airuleset `page:` class, the only key class the owner policy lets reach the phone (issue 1495,
  airuleset 0.1.626, one message per check per 10 min while it lasts):
  `page:<box>-output-stuck:<watchdog_notify_key bucket>`. airuleset rewrites the suffix to its own
  window. The box name is the check name, so the script refuses a name outside `[a-z0-9.-]` (an
  invalid check name is suppressed silently by airuleset).
- On the #1206 sweep's production-critical allowlist and in `scripts/lib/watchdog-roster.sh`.
- Recovery: one machine-channel log line, never a phone ping.
- Never route a diagnostic onto `page:`.

## Tests and limits

- `tests/python/test_stream_output_stuck_1460.py`: the decision, the probe's printed lines (a stub
  `websocket` module when websocket-client is missing, never a skip), the watchdog through the
  `STREAM_OUTPUT_STUCK_FETCH_CMD` seam on an injected clock (`STREAM_OUTPUT_STUCK_NOW`), and one
  NON-dry-run pass whose `AIRULESET_NOTIFY` stub records the real argv (the page key must match
  airuleset's page grammar).
- Not covered: a frozen PICTURE whose counters still advance (the `NDI 2ME PGM` receiver repeating a
  frame) -- frozen-input territory; a dead YouTube egress behind the local restreamer (`outputBytes`
  only measures to the RTMP target on the box).
- Only stream-snv (`STREAM_OUTPUT_STUCK_BOX`); ships DISABLED, the supervisor runbook is the README.
