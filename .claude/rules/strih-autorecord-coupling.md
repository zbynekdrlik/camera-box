---
paths:
  - "scripts/recording-e2e.sh"
  - "scripts/rig-busy-gate.sh"
  - "scripts/lib/stray-session-check.sh"
  - "scripts/obs_phase2.py"
---

# Why strih (and stream) auto-RECORD on stream go-live, and orphan after StopStream (#1274)

When stream OBS (10.77.9.204) starts streaming, a recording starts in the SAME second on **both**
stream AND strih (10.77.9.202) OBS, and after `StopStream` **both recordings keep running** until a
human stops them. Every restreamer CI broadcast (which only calls `StartStream`/`StopStream` on
stream OBS) leaves this orphan pair, and camera-box's own `rig-busy-gate.sh` / the `[3/8]` re-check
then correctly report `RIG_BUSY` and block the Full-path E2E until someone StopRecords by hand.

**This is NOT camera-box code, NOT OBS `RecordWhenStreaming`, and NOT Advanced Scene Switcher.**
The #1274 investigation falsified the adv-ss hypothesis and proved the owner is **Bitfocus
Companion on 10.77.9.205** (a production hardware-controller box). This file documents the exact
mechanism so a future `RIG_BUSY`-from-orphan-recording incident is a 2-minute read, and so nobody
re-chases OBS/adv-ss config on the two boxes.

## The mechanism (all fragments quoted from a read-only inspection 2026-09-02)

**Owner: Bitfocus Companion, `10.77.9.205:8000`** (admin title "Bitfocus Companion - Admin"). It
holds PERSISTENT OBS-WebSocket connections opened at each OBS boot — verified because each box's
OBS log has `onOpen` exactly one higher than `onClose` (the still-open controller connection), and
`10.77.9.205` is the client that connected once at OBS start and never disconnected: 1 connection
to strih, 2 to stream. (The other rig WS clients are benign: `10.77.9.103` = the camera-box
rig-busy / bundle-state poller, connect-query-disconnect every ~30–45 s; `127.0.0.1` = local
restreamer/OBS clients.)

Companion connections (from the config export): `strih-obs` → `strih.lan:4455`,
`stream_obs` → `stream.lan:4455` (also `imag-obs`, `cg_obs`), all `moduleId: obs-studio`.

**The trigger — `PRODUCTION` (enabled):**
- `events`: a single `condition_true` event — fires ONLY on the rising edge (condition false→true).
- `condition`: AND of two feedbacks, BOTH on `stream_obs`:
  - `sceneProgram` with `scene == "PRO"` (stream OBS program scene is "PRO"), and
  - `streaming` (stream OBS is streaming).
- `actions`: press button `24/2/1` (internal `button_pressrelease`).

**Button 24/2/1 = the "GO LIVE" combo** (page 24 "stream", "down" set): set custom var `live=1`;
ProPresenter macro `DC7B5042-…`; press nested button `$(this:page)/1/1` (= `24/1/1`); ProPresenter
`broadcast.set_live` ON; Home-Assistant `input_boolean.media_live` ON; `mute_track` track 6.

**Button 24/1/1** (the nested press): `stream_obs → start_recording` AND `strih-obs → start_recording`.
→ this is what StartRecords BOTH boxes.

**The symmetric GO-OFF-AIR combo ALREADY EXISTS but nothing fires it automatically:**
- Button `24/1/2`: `stream_obs → stop_recording` + `strih-obs → stop_recording`.
- Button `24/2/2` = "GO OFF-AIR" combo: set `live=0`; ProPresenter macro `826A3B01-…`; press
  `24/1/2` (StopRecord both); ProPresenter `broadcast.set_live` OFF; HA `media_live` OFF.

**The asymmetry IS the bug.** `PRODUCTION` has a `condition_true` event that presses the ON combo,
but there is **no `condition_false` event and no second trigger** that presses `24/2/2` (or `24/1/2`)
when the condition goes back to false. So StartStream (with program scene "PRO") auto-records both
boxes; StopStream flips the condition false and **nothing** stops the recordings — the operator has
to press the OFF button (`24/2/2`) manually, and the unattended restreamer CI never does.

Why a dev/CI stream triggers it: the restreamer CI's `StartStream` on stream OBS, while stream's
program scene is already "PRO", makes both feedbacks true → the production go-live combo fires. The
Companion `streaming`/`sceneProgram` feedbacks do NOT expose the stream target/service, so Companion
cannot tell a dev/CI stream (rtmp to a local Restreamer) from a real broadcast via a condition — a
symmetric auto-OFF is the robust fix, not a dev-stream exclusion.

## Timing proof (2026-09-02 12:11:36 incident)

- stream: `12:11:36.238 ==== Streaming Start` → `12:11:36.420 ==== Recording Start` (+182 ms) →
  `12:43:14 Streaming Stop` → recording ran on until the manual `12:44:25.529 ==== Recording Stop`.
- strih: `12:11:36.573 ==== Recording Start` (+335 ms), no WS connect at that instant (persistent
  Companion connection), file `D:\_REC\2026-09-02 12-11-36.mkv`.

## camera-box already BOUNDS the damage (no code change needed for #1274)

`scripts/rig-busy-gate.sh` (#657) + `scripts/obs_phase2.py` (`_stray_recording_hosts`,
`record --action guard`) already treat "recording ON + streaming OFF" as OUR stray signature and,
after `STRAY_HEAL_THRESHOLD` (3) consecutive stray-only polls, `StopRecord` that box themselves —
keeps the file, never touches program routing — converting the permanent self-deadlock into a
self-healing one. So the orphan recording is a REAL production artifact until the owner's Companion
fix lands, and camera-box's existing self-heal already unblocks the E2E; **no camera-box code
change is warranted** (a "recording that started within N s of a stream StartStream whose stream has
since stopped" classifier would only re-derive what the stray-signature self-heal already does).

- **CAVEAT to state plainly (do NOT "fix" it):** "recording ON + streaming OFF" is not intrinsically
  impossible — it is strih's NORMAL broadcast state, because **strih never streams** (it feeds stream
  over NDI; only stream OBS holds the RTMP output). The self-heal calls it "stray" ONLY because the
  standing user-is-guard rule means camera-box runs OFF-AIR (a real broadcast would have stopped
  Claude), so during a gate run any recording is ours. The heal never touches a box that is ALSO
  streaming (a real broadcast always streams+records together), so it can never hit a live event —
  but the signature is a run-context assumption, not a physical impossibility. Keep the
  streaming-AND-recording gate on the heal; never widen it to "recording alone on strih = stray"
  outside the off-air gate context.

## The fix is OWNER-SIDE (Bitfocus Companion on 10.77.9.205) — proposed, not applied

Make the OFF symmetric in Companion (owner's box; camera-box has no write authority there, and this
lane applies nothing — it is a read-only investigation). **Recommended shape** (the #1274 advisor
verdict; the owner confirms via the ticket question before anyone applies it):

- A NEW trigger **`PRODUCTION OFF`** (leave `PRODUCTION` untouched), event `condition_true`,
  condition = **[stream-OBS instance connection OK] AND [stream-OBS `streaming` feedback, inverted]**,
  action = press **`24/1/2`** (StopRecord stream + strih, nothing else).
- Why `streaming`-inverted, NOT the full `PRO`-scene-AND-streaming mirror: `streaming==false` is the
  only unambiguous "off-air" signal; the program scene is an operator choice mid-broadcast, so a full
  mirror also fires on an interstitial cut away from "PRO". `streaming`-alone strictly dominates.
- Why press `24/1/2` (StopRecord only), NOT the full `24/2/2` combo: automate the invisible thing the
  operator forgets (the recording), and leave the VISIBLE production states (ProPresenter broadcast,
  HA `media_live`, mute) on the operator's existing manual GO-OFF-AIR press. A misfire then splits the
  archive into two files instead of yanking ProPresenter/HA mid-service. (StopRecord on an
  already-idle recorder is a no-op, so the auto-OFF and the manual OFF compose safely in any order.)
- Why connection-gate it: if Companion's OWN WS link to stream OBS drops (stream OBS restart, a WS
  hiccup) the `streaming` feedback can read false while the RTMP stream is alive → a bare
  `condition_false` would StopRecord strih (still connected) mid-service. Gating on the stream-OBS
  instance status neutralizes that. (Dev-time test before trusting it: drop Companion's stream-OBS
  connection during a dev stream and confirm the trigger does NOT fire.)
- Reconnect behaviour: OBS keeps `outputActive` true during its internal RECONNECTING retries, so a
  short uplink blip does not fire the auto-OFF; only a genuine give-up stops the recordings, and the
  operator's next StartStream re-fires `PRODUCTION` (rising edge) and recording resumes — cost is a
  split archive, never a lost one.
- REJECTED (one line, do not build now): a Companion custom variable "dev stream" set by the CI over
  Companion's HTTP API and ANDed into `PRODUCTION` — it would make CI invisible to production
  automation, but a stuck flag silently disables Sunday GO LIVE (fails in the DANGEROUS direction).
- **The whole GO-LIVE state orphans, not just recordings.** Every CI/dev StartStream (with program on
  "PRO") also flips ProPresenter `broadcast.set_live` ON, HA `input_boolean.media_live` ON, and mutes
  track 6 — and nothing reverts them either. The recommended `24/1/2`-only auto-OFF deliberately does
  NOT touch those; whether they should also auto-revert is a SEPARATE, later owner question, not this
  ticket's scope.
- The one genuine OWNER decision (in the #1274 question): should a real broadcast's recording keep
  running past `StopStream` (a "record the postlude" habit), or auto-stop with it? Applying the fix is
  itself a rig mutation on a LIVE production controller — export the config first (that export IS the
  rollback), then verify with one dev StartStream→StopStream whose OBS logs show both StopRecords
  within ~1 s and whose Companion trigger log shows exactly one fire.

## Re-inspect it read-only (no rig mutation)

- Both OBS logs (win-* MCP `FileRead`/`Shell` read-only, never ssh): grep the ACTIVE log for
  `==== (Streaming|Recording) (Start|Stop)` and `has connected from`/`has disconnected`. The active
  strih log is OBS-locked → open it read-shared: `FileStream(path,'Open','Read','ReadWrite')`.
  `onOpen`>`onClose` ⇒ a persistent controller connection is still open; the never-disconnected
  client IP is the controller.
- Confirm native auto-record is OFF: `basic.ini` (profiles `Stream_Obs` / `light`) has no
  `RecordWhenStreaming` key.
- Confirm adv-ss is NOT the owner: the active scene-collection JSON
  (`…\basic\scenes\<file>.json` → `modules.advanced-scene-switcher.macros`) has no
  streaming/recording/websocket macro on either box.
- The Companion config is a read-only export: `curl http://10.77.9.205:8000/int/export/full`
  (gzip JSON) — `triggers` (the `PRODUCTION` trigger), `instances` (the OBS connections), `pages`
  (page 24 buttons `1/1` StartRecord-both, `1/2` StopRecord-both, `2/1` GO-LIVE, `2/2` GO-OFF-AIR).
  Never hit a Companion `.../press` / action endpoint on a LIVE production controller — GET the
  export only. A saved export lives at `~/.claude/work-products/issue-1274/`.
