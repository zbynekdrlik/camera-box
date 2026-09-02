---
paths:
  - "scripts/recording-e2e.sh"
  - "scripts/rig-busy-gate.sh"
  - "scripts/lib/stray-session-check.sh"
  - "scripts/obs_phase2.py"
---

# Why strih (and stream) auto-RECORD on stream go-live, and orphan after StopStream (#1274)

When stream OBS (10.77.9.204) goes live, a recording starts in the SAME second on **both** stream
AND strih (10.77.9.202) OBS, and after `StopStream` **both recordings keep running** until someone
stops them. A headless stream (the restreamer CI, or a dev broadcast) leaves this orphan pair, and
camera-box's own `rig-busy-gate.sh` / the `[3/8]` re-check then report `RIG_BUSY` and block the
Full-path E2E until the recordings are StopRecord'd.

**This is NOT camera-box code, NOT OBS `RecordWhenStreaming`, and NOT Advanced Scene Switcher** — it
is a **Bitfocus Companion** controller. Attribution is STRONGLY INFERRED (see "how certain" below),
not directly logged. This file exists so a future `RIG_BUSY`-from-orphan-recording incident is a
2-minute read instead of a re-chase of OBS/adv-ss config on the two boxes.

## The mechanism — a Bitfocus Companion scene state machine (10.77.9.205)

**Owner: Bitfocus Companion, `10.77.9.205:8000`** (admin page title "Bitfocus Companion - Admin").
It holds long-lived OBS-WebSocket control connections to strih (1) and stream (2). Its recording
automation is THREE enabled triggers, each `condition_true` (rising edge) on stream OBS's PROGRAM
SCENE (`stream_obs` connection; a `moduleId: obs-studio` connection to `stream.lan:4455`):

| Companion trigger | condition (on `stream_obs`) | presses | net effect |
|---|---|---|---|
| `PRE`        | program scene == `PRE`               | button `24/2/0` | pre-roll: set var `live=0`, a ProPresenter-7 macro, HA `media_live` OFF. **Does NOT stop recording.** |
| `PRODUCTION` | program scene == `PRO` **AND** streaming | button `24/2/1` | GO-LIVE combo (below) — **StartRecord both boxes** |
| `POST`       | program scene == `POST`              | button `24/2/2` | GO-OFF-AIR combo (below) — **StopRecord both boxes** |

**Button 24/2/1 (GO-LIVE combo, "down"):** set var `live=1`; a ProPresenter-7 macro
(`renewedvision-propresenter`, macro UUID `DC7B5042-…`); press nested button `$(this:page)/1/1`
(= `24/1/1`); a separate Presenter-app `broadcast.set_live` ON (connection `presenter`, moduleId
`presenter`); Home-Assistant `input_boolean.media_live` ON; `mute_track` track 6.
- **Button 24/1/1** (the nested press): `stream_obs → start_recording` AND `strih-obs → start_recording`.
  → this is what StartRecords BOTH boxes.

**Button 24/2/2 (GO-OFF-AIR combo, "down"):** set var `live=0`; a ProPresenter-7 macro
(`826A3B01-…`); press nested button `$(this:page)/1/2` (= `24/1/2`); Presenter `broadcast.set_live`
OFF; HA `media_live` OFF. (No `mute_track` un-mute is present in the export — track 6 is NOT
un-muted here; if it is un-muted it happens elsewhere.)
- **Button 24/1/2** (the nested press): `stream_obs → stop_recording` AND `strih-obs → stop_recording`.
  → the StopRecord-both path DOES exist and IS auto-pressed — but ONLY by the `POST` trigger, i.e.
  only when the operator switches stream's program scene to `POST`.

**The real root cause of the orphan (corrected — it is NOT "no off trigger"):** the OFF is a REAL,
enabled trigger, but it is gated on **switching stream's PROGRAM SCENE to `POST`**, not on the stream
stopping. A live operator ends a service by cutting to the `POST` scene → `POST` fires → StopRecord
both. A **headless restreamer CI / dev stream** `StartStream`s while stream's program is `PRO`
(satisfies `PRODUCTION` → StartRecord both) and then `StopStream`s **without ever cutting to `POST`**
→ the `POST` trigger never fires → the recordings orphan. (The go-live side is streaming-aware, the
go-off side is scene-only — that scene/stream mismatch is the bug, not a missing off trigger.)

Companion's boolean `streaming` feedback carries no options (no stream target/service), so a Companion
condition cannot distinguish a dev/CI stream (rtmp to a local Restreamer) from a real broadcast — a
symmetric stream-STOP auto-off is the robust fix, not a dev-stream exclusion (see proposal).

## How certain is the Companion attribution — STRONGLY INFERRED, not logged

obs-websocket does not log request bodies and adv-ss macro-run logging is off, so no line names the
StartRecord's origin. The attribution is inference by elimination + a persistent-socket chain, not a
controlled repro (a repro — disable `PRODUCTION`, or StartStream on a non-`PRO` scene, and confirm no
Recording Start — was NOT run; the rig was read-only under a live E2E):
- On strih there was NO new WS connect between `12:11:27.650` (a poller disconnect) and the
  `12:11:36.573 Recording Start`, so the StartRecord rode an already-open socket.
- The only end-of-log-still-open socket on strih is `10.77.9.205` (onOpen 500 − onClose 499 = 1; the
  `.205` client connected once at `11:21:29` and never disconnected). On stream both still-open
  sockets are `.205` (onOpen 255 − onClose 253 = 2). ("Long-lived" is measured at log END; a `127.0.0.1`
  session was not duration-checked, but `.205` is the only client that spans the whole session.)
- Neither box's active scene-collection adv-ss has any streaming/recording/websocket macro
  (strih: 19 tally + Multiview macros, 0 websocketConnections; stream: 11, none record-shaped), and
  `RecordWhenStreaming` is absent from both `basic.ini`.
- `10.77.9.205:8000` serves "Bitfocus Companion - Admin"; its read-only config export contains the
  three triggers + the button graph above, exactly matching the observed StartRecord-both behaviour
  and the `24/1/1` action order (stream then strih matches the `12:11:36.420` / `12:11:36.573` order).

## Timing proof (2026-09-02 12:11:36 incident)

- stream: `12:11:36.238 ==== Streaming Start` → `12:11:36.420 ==== Recording Start` (+182 ms) →
  `12:43:14.901 ==== Streaming Stop` → recording ran on until `12:44:25.529 ==== Recording Stop`.
  (`==== Streaming Stop` follows OBS's generic `User stopped the stream` line, which a WS `StopStream`
  ALSO prints — it does not prove a human.)
- strih: `12:11:36.573 ==== Recording Start` (+335 ms; no WS connect at that instant — the persistent
  `.205` socket), file `D:\_REC\2026-09-02 12-11-36.mkv`. strih's Recording Stop time was NOT captured.
- The starter of the `12:11:36.238` stream is not identified from these logs; per the ticket it was
  the restreamer CI (only `StartStream`/`StopStream` on stream OBS, restreamer #361) — not re-verified
  in this lane. The coupling fires on ANY `StartStream` (a manual go-live included) while program == `PRO`.

## camera-box side — no code change, but the mitigation is BOUNDED (state it honestly)

`scripts/rig-busy-gate.sh` (#657) + `scripts/obs_phase2.py` (`_stray_recording_hosts`) partition a box
"recording ON + streaming OFF" as OUR stray signature and, after `STRAY_HEAL_THRESHOLD` (3) consecutive
stray-only polls DURING a run's preflight, `StopRecord` that box (keeps the file, no program-routing
change). This MITIGATES the orphan **only while a camera-box run is actively polling** — an orphan from
an off-hours stream test runs until the next E2E preflight or a manual off. It is NOT proven to have
bounded the 12:11:36 orphans: per the ticket the `12:44:25` stop was a **manual** WS `StopRecord`, not a
`#657 self-heal` line, and strih's stop is unrecorded here. Still, **no camera-box code change is
warranted** — a "recording that started within N s of a stream StartStream whose stream has since
stopped" classifier only re-derives the existing stray-signature heal, and the durable fix is
owner-side (Companion). If a future ticket DOES harden camera-box, the honest improvement is a
FLEET-AWARE stray test ("strih recording AND stream NOT streaming") rather than the per-box partition
(which structurally can never exclude a never-streaming strih) — but that is optional and out of #1274.

- **CAVEAT — do NOT "fix" the stray signature.** "recording ON + streaming OFF" is strih's NORMAL
  broadcast state, because **strih never streams** (it feeds stream over NDI; only stream OBS holds
  the RTMP output). The heal calls it "stray" ONLY because the standing user-is-guard rule means
  camera-box runs OFF-AIR, so during a gate run any recording is ours; and the heal never touches a
  box that is ALSO streaming (a real broadcast streams+records together), so it can't hit a live
  event. It is a run-context assumption, not a physical impossibility — keep the
  streaming-AND-recording gate; never widen it to "recording alone on strih = stray".

## The fix is OWNER-SIDE (Bitfocus Companion on 10.77.9.205) — proposed, NOT applied (this lane is read-only)

Add a stream-STOP off path so the recordings stop even when nobody cuts to `POST` (the advisor verdict;
owner confirms the behaviour via the #1274 question before anyone applies it — applying is a rig
mutation on a LIVE production controller):

- A NEW trigger **`PRODUCTION OFF`** (leave `PRE`/`PRODUCTION`/`POST` untouched), event
  `condition_true`, condition = **[`stream_obs` instance connection == OK] AND [`stream_obs` `streaming`
  feedback, inverted]**, action = press **`24/1/2`** (StopRecord both, nothing else — the existing
  StopRecord-both button).
- Why `streaming`-inverted, not scene-based: it fires when streaming actually STOPS, regardless of
  scene, so it catches BOTH the headless CI/dev orphan (never cuts to `POST`) AND a live service where
  the operator forgot to cut to `POST`. The existing `POST` trigger stays as the operator's scene path.
- Why press `24/1/2` (StopRecord only), not the full `24/2/2` combo: automate the invisible thing the
  operator forgets (the recording); leave the visible production states (Presenter broadcast, HA
  `media_live`) on the existing `POST`/manual path. A misfire then only splits the archive into two
  files. StopRecord on an idle recorder is a no-op, so it composes safely with `POST` in any order.
- Why connection-gate it: if Companion's OWN link to stream OBS drops, `streaming` can read false while
  RTMP is alive → a bare streaming-inverted trigger would StopRecord strih mid-service. Gating on the
  stream-OBS instance status removes that. **Un-noted edge to flag to the owner:** the connection gate
  ALSO makes the trigger fire when the stream-OBS connection COMES UP while not streaming — Companion
  boot (idle → StopRecord no-op, harmless) OR a stream-OBS crash/restart MID-EVENT (would StopRecord
  strih's live recording). Dev-time test both: (1) StartStream→StopStream shows both StopRecords within
  ~1 s and exactly one trigger fire; (2) drop the stream-OBS connection during a dev stream and confirm
  no false stop; (3) restart stream OBS during a dev stream and see whether the reconnect fires it.
- Reconnect behaviour: OBS keeps `outputActive` true through its internal RECONNECTING retries, so a
  short uplink blip does not fire the off; only a genuine give-up stops recording, and the next
  StartStream re-fires `PRODUCTION` — cost is a split archive, never a lost one.
- REJECTED (do not build): a CI-set Companion "dev stream" variable ANDed into `PRODUCTION` — a stuck
  flag would silently disable Sunday GO LIVE (fails in the dangerous direction).
- **The whole GO-LIVE state orphans, not just recordings.** Each headless StartStream on `PRO` also
  flips Presenter `broadcast.set_live` ON, HA `media_live` ON, and mutes track 6, and only a cut to
  `POST` reverts the first two (track 6's un-mute is not in the off combo at all). The `24/1/2`-only
  auto-off deliberately does NOT touch those — whether they should also auto-revert is a SEPARATE,
  later owner question, out of #1274.
- Disk consequence: each headless stream test leaves a full-duration orphan on stream (recording +
  streaming, never "stray" by design) PLUS a strih orphan → feeds the `D:\_REC` retention problem
  (issue 1122).

## Re-inspect it read-only (no rig mutation)

- Both OBS logs (win-* MCP `FileRead`/`Shell` read-only, never ssh): grep the ACTIVE log for
  `==== (Streaming|Recording) (Start|Stop)` and `has connected from`/`has disconnected`. The active
  strih log is OBS-locked → open it read-shared: `FileStream(path,'Open','Read','ReadWrite')`.
  `onOpen` > `onClose` ⇒ a long-lived controller connection is still open; the never-disconnected
  client IP is the controller.
- Native auto-record OFF: `basic.ini` (profiles `Stream_Obs` / `light`) has no `RecordWhenStreaming`.
- adv-ss is NOT the owner: the active scene-collection JSON (`…\basic\scenes\<file>.json` →
  `modules.advanced-scene-switcher.macros`) has no streaming/recording/websocket macro on either box.
- The Companion config is a read-only export: `curl http://10.77.9.205:8000/int/export/full` (gzip
  JSON) — `triggers` (`PRE`/`PRODUCTION`/`POST`), `instances` (the OBS connections `strih-obs`/
  `stream_obs`), `pages` page 24 (`1/1` StartRecord-both, `1/2` StopRecord-both, `2/0` PRE, `2/1`
  GO-LIVE, `2/2` GO-OFF-AIR). **Never hit a Companion `.../press` / action endpoint on a LIVE
  production controller — GET the export ONLY.** A saved export lives at
  `~/.claude/work-products/issue-1274/`.
