# cam2 QR-synced audio → automated A/V-sync calibration (design)

**Date:** 2026-07-01
**Status:** design agreed (user, interactive brainstorm 2026-07-01)
**Tickets:** root behind #188 (in-OBS A/V-sync dock) + #145 (A/V clock drift, closed — root lives here) + #300 relation (measurement chain). New implementation pieces to be filed.

## Goal

Automate the stream A/V-align. Today the operator manually nudges the genlock video-delay
(preload, ~1 s) until the video matches the **~1 s-late mastered audio** — by eye/ear, re-nudging on
drift (#145). Replace that with a one-shot **calibration** that MEASURES the true video↔audio offset
and AUTO-SETS the genlock video-delay to zero it.

**The A/V-align latency is NOT a fixed constant** (there is no "correct 450 vs 1000") — it is whatever
this calibration measures for the current audio chain. (This is why the drift-guard #357 pin of a
fixed value is only a drift backstop, not the source of truth.)

## Decisions (user, 2026-07-01)

1. **Target = measure + AUTO-SET the delay** (closed loop), not just display.
2. **When = one-shot calibration BEFORE the event** (no audience). Marker may be AUDIBLE. Live
   drift-during-event correction is OUT OF SCOPE (possible future continuous mode).
3. **Capture path:** hand mic → the SAME mastering chain (incl. the ~1 s delay) as the event audio →
   Dante/ASIO → stream OBS audio mix → **stream OBS recording**. The offset is measured from ONE
   stream-OBS recording (video QR + audio marker together).
4. **Mastering:** the calibration click traverses the same ~1 s mastering as the event audio, so the
   measured offset DIRECTLY gives the correct video-delay (no separate mastering-delay term needed).

## The calibration loop

1. **cam2 marker** — the painter (`frame-probe --paint-only`), alongside the dual-QR, emits a short
   distinctive **audio marker** (start: a click / short chirp) at a slow cadence (~4–5 s ≫ the ~1 s
   offset → unambiguous which marker is which), **sample-aligned to a known QR `frame_id`** (the
   painter logs the marker↔frame_id pairing). Output via cam2 USB audio (card 0). Cadence + audibility
   OK because it is a pre-event calibration.
2. **Measure** — from the stream-OBS recording: decode QR tick `T` → its video PTS; detect the marker
   for `T` in the audio track (onset / cross-correlation) → its audio PTS;
   **offset = video_PTS(T) − audio_PTS(T)** = the real A/V desync in ms. Pure, Tier-0-testable.
3. **Auto-set** — from the offset compute the required genlock video-delay and set
   `genlock_latency_ms_src` on stream 'NDI 2ME PGM' via the OBS WebSocket so the offset → 0; verify
   read-back; SAFETY: sane range clamp, log, snapshot/restore-on-abort (reuse the #358
   snapshot/set/restore + verified-read-back pattern — never leave prod off a valid A/V-align).
4. **Dock #188** — surface the measured A/V offset live (and ~0 after set).

## Components (isolated units)

| Unit | Responsibility | Where |
|---|---|---|
| a. Audio-marker generator | painter emits the marker + logs marker↔frame_id | `src/probe/` painter path |
| b. A/V offset measurement | detect marker in audio + pair with QR + compute offset | `recording-verdict` (+ a pure Tier-0 offset module) |
| c. Auto-set controller | offset → genlock video-delay set (WS) + safety + verify | harness / obs_phase2.py (reuse #358 set/restore) |
| d. #188 dock panel | display the measured A/V offset | vendor/ OBS dock fork (#188) |

## Data flow

cam2 (QR video + audio marker, both stamped tick `T`) → **video path** (cam2 monitor → cam1 → NDI →
strih → stream, through the genlock video-delay) + **audio path** (hand mic → mastering ~1 s → Dante →
stream) → **stream OBS recording** (video QR + audio marker) → recording-verdict (offset) →
controller (set delay) → dock (display).

## Testing

- **Tier-0 (no rig):** offset computation (video PTS + marker PTS → offset); marker detection on a
  fixture audio clip (RED→GREEN); controller math (offset → delay); safety clamps.
- **Rig calibration proof (supervisor):** run the calibration, measure the offset, auto-set the
  video-delay, then re-record and confirm the marker↔QR offset is ~0 (A/V aligned). Verified on the
  live rig by the supervisor, not a worker (drive-rig-steps).

## Marker signal (technical, decided during implementation + rig-verified)

Start with a **click / short chirp**; verify it survives the mastering chain (limiter/EQ/compression)
cleanly enough for ~1-frame timing on the rig. If a click's transient is squashed by the mastering
limiter, upgrade to a short chirp (cross-correlation, sub-ms) or an **FSK burst encoding the
`frame_id`** (exact pairing, audio analog of the QR). ~1-frame precision (16–33 ms) suffices — human
A/V threshold is ~20–40 ms.

## Out of scope

- Continuous live-drift correction during the event (future continuous/inaudible-marker mode).
- The mastered-audio ~1 s delay itself is the user's domain — this only MEASURES and SETS the video
  to match it, never changes the audio.

## Key assumptions (confirmed with user 2026-07-01)

- The hand-mic audio IS present in the stream OBS recording's audio track.
- The calibration click traverses the SAME mastering (~1 s) as the event audio → the measured offset
  directly equals the production A/V desync.
