# YouTube leg in the camera-box release E2E — design

Issue 1404. Owner decision 6.10.2026 (answer "2"): the camera-box release E2E measures quality all the way to the YouTube recording, not only to the stream OBS recording. Restreamer's own release gate (restreamer issue 357) measures the same leg with the same tool.

## What we measure and why

On 5.10 the owner reported the YouTube stream "drops frames now and then, ~20 fps" and saw a Thursday A/V desync. Three manual sessions (issue 1404 comments 6004613512, 6006986090, 6008636005) found two things:

- **The fault was downstream of stream OBS:** a late join and a +1.37 s audio shift on YouTube, from restreamer 0.29.27.
- **A clean chain is measurable to the frame and to the millisecond:** the cam2 painter tick is on every frame, and the QPSK marker survives YouTube's audio encode.

The E2E gate therefore has to prove, for every release, that YouTube shows what stream OBS sent.

## Criteria (the PASS bar)

Each window compares the YouTube VOD against stream OBS's own program recording of the same run.

1. **A/V consistent within the session:** the per-window VOD − recording A/V offset (QPSK marker vs painter tick, `recording-verdict --av-sync`) stays within ±150 ms of the run's first window. The fixed term YouTube adds varies between sessions (+42 / −20 / +5 ms measured), so the bar is relative.
2. **0 downstream dup/skip:** counted by painter tick against the recording. A tick the rig itself repeated cancels out. A skip counts only between adjacent decoded VOD frames. A dup counts only when the recording shows that tick exactly once, with decoded neighbours.
3. **No late join / no lost content after any (re)publish:** the first VOD frame after each publish is within 0.5 s of the publish time, in content time mapped by tick.
4. **Audio continuous:** 0.25 s block cross-correlation of VOD audio against recording audio, with no lag jump > 1.5 ms, no low-correlation block (< 0.6) while the recording has signal, no silent VOD block, and no level drop > 10 dB.
5. **Coverage reported:** decodable % and cadence-proven % per window, VOD and recording. Coverage is report-only, but a window with < 90 % cadence-proven frames is UNKNOWN, never PASS.

**Overall:** PASS only when every window passes every criterion. A tool error, a VOD that never finishes processing, or quota exhaustion is UNKNOWN, and the gate is red (fail closed).

## Components

### A. Restreamer session API (owned by restreamer, restreamer issue 357)

`/api/v1/av-gate/session` on stream.lan, LAN only, with the existing API token. No secret leaves stream.lan.

- `POST {requester}` → `{session_id, broadcast_id}`: creates the unlisted broadcast, binds e2e rtmp, activates the E2E-Test event and starts the VPS.
- `GET {id}`: `starting` → `ready` → (after stop) `processing` → `done {vod_id}` or `failed {reason}`.
- `POST {id}/stop`: drain, complete the broadcast, delete the VPS. Cleanup runs on every exit path and through an idle reaper.
- Constraints:
  - one session at a time, guarded by restreamer's mutex plus the caller's rig lease;
  - ~EUR 0.01 per run;
  - YouTube quota of ~10 sessions/day across both gates.

### B. Measurement tool (owned by camera-box): `scripts/youtube-leg-verdict.py`

It is built from the session tools already proven (`qrticks.py`, `dupskip.py`, `avabs2.py`, `audiocont.py` in `~/.claude/work-products/issue-1404/`). Two changes:
- the tick decode reads BOTH QR halves, because the painter alternates its colour-coded QR left/right and the session decoder lost those frames;
- the `recording-verdict --av-sync` probe binary comes from the CI probe-tools artifact, as the E2E already does.

CLI contract (shared with restreamer's gate):

```
youtube-leg-verdict.py --vod <youtube id | local file> --recording <file>[@<record-start-utc>] ...
                       --markers <cam2 qpsk marker csv> --windows <name:start-utc:end-utc> ...
                       --publish <utc> ... --out <dir>
exit 0 = PASS, 1 = FAIL, 2 = UNKNOWN (tool/decode/download error)
<dir>/youtube-leg-verdict.json:
  {schema: 1, overall: "PASS"|"FAIL"|"UNKNOWN", criteria: {...},
   windows: [{name, start, end,
              av: {rec_ms, vod_ms, delta_ms, markers_rec, markers_vod},
              dupskip: {dup, skip, unjudged},
              audio: {blocks, lag_jumps, low_corr, silent, level_drops, corr_median},
              coverage: {rec_decodable_pct, rec_cadence_pct, vod_decodable_pct, vod_cadence_pct}}],
   publishes: [{utc, first_vod_frame_utc, gap_s}], reasons: [...]}
```

- More than one `--recording` = parts of one session split by an OBS restart, joined by tick.
- The VOD is fetched with `yt-dlp` (video 1080p30 + audio).
- The cam2 QPSK marker log (`/run/rig-qpsk-markers.csv` on cam2) is mirrored read-only over HTTP on dev1, next to the rig lease at `http://dev1:8890/rig-qpsk-markers.csv` (LAN only; plain CSV, the same columns as on cam2). Restreamer's gate fetches it from there; the fleet ssh credentials stay with camera-box.

### C. Wiring in the full-path E2E (camera-box)

It runs inside the existing `recording-e2e.sh` run, which already holds the rig lease:

1. Before `[5/8] StartRecord`: `POST session` and wait for `ready`, bounded at 6 min. Then StartStream on stream OBS. Failure is UNKNOWN: the gate is red, never skipped.
2. The normal recording window runs unchanged. Stream OBS's own recording is the reference.
3. After StopRecord: StopStream and `POST stop`. Then poll `done`, bounded at 25 min; YouTube processing measured 2-6 min.
4. Pull the stream recording to dev1, which is not on the stream box, and run the tool.
5. The verdict folds into the run's overall pass through a new `youtube_leg` seam: `gates_overall_pass()` true. A YouTube FAIL or UNKNOWN makes the run red.
6. The Discord per-run report gets one YouTube line.

Guard: the YouTube leg runs on the release `pull_request` E2E, at most once per head SHA. Every push to dev with an open release PR re-runs the E2E, so a re-run on the same SHA reuses the stored YouTube verdict instead of starting another session.

### D. The two rig-side prerequisites that must be fixed first

On today's rig the YouTube leg would be red for reasons outside YouTube:
- the strih-lx USB NIC packet loss (issue 1242 / issue 1387, the new notebook);
- the nightly date step (issue 1372).

Both freeze the program before stream OBS. Criterion 2 cancels them, because it compares against the recording, so the YouTube leg can ship before those fixes. The existing camera-chain gates stay red until they are fixed.


### E. The CG / SongPlayer path (owner decision 6.10.2026, issue 1404 comment 6016291989)

Owner, verbatim: "a dolezite je aby aj songplayer a obs manual bol zaradeny do testu aby bolo iste ani z cg/songplayer cesty nam neprichadza sekajuci obraz zvuk".

The release E2E run gets two CG segments in its recording window. They are mandatory, not the old opt-in `CG_CHAIN=1` profile, and are measured on the stream recording AND on the YouTube VOD:

1. **SongPlayer segment:** strih program on the CG scene (input `CG-obs` = `RESOLUME-SNV (SP-program)`), SongPlayer playing the test playlist.
   - Picture: SongPlayer's own origin burn `911014` → strih burn → stream burn → YouTube.
   - Sound: SongPlayer's test audio through the same chain.
2. **OBS manuál segment:** SongPlayer's program on its input "OBS manuál" (source −1 = the cg OBS output), cut through SongPlayer's facade. cg OBS shows a test scene carrying its hop burn `911015`.
   - Picture: `911015` → SongPlayer `911014` → strih → stream → YouTube.
   - Sound: the cg OBS test audio through the same chain.

Criteria per segment:
- **Picture:** burn contiguity + max-hold at every hop (the existing `src/cg_chain_gate.rs` contiguity/hold, made BLOCKING), and 0 downstream dup/skip on YouTube (criterion 2).
- **Sound:** continuity against the KNOWN source audio, i.e. block cross-correlation of the program audio against the reference file the source played. That catches a stutter anywhere upstream of stream OBS, not just downstream of it. Plus criterion 4 on YouTube.
- **A/V:** the segment's audio vs its burn timing, consistent within the run.

Owner amendment 6.10.2026 (issue 1404 comment 6016489928): NOTHING copyrighted goes to YouTube. The SongPlayer test item and the cg OBS test scene play a camera-box-generated measurement clip: a per-frame QR and the QPSK marker over a tone bed. A live program-audio guard stops the broadcast when non-measurement audio appears. See the plan's owner amendment.

What SongPlayer has to provide (agreed with the songplayer session before the build):
- a deterministic test playlist: a known audio file plus video, with the `911014` burn on;
- the facade cut to/from "OBS manuál";
- read-back of both programs.

Camera-box provides:
- the cg OBS test scene and its burn;
- the strih/stream cuts (never `PRO` on the stream program; issue 1380 guard);
- the measurement.

The stale pieces are re-targeted to today's topology (issue 1302 comment 6004895923): the cg OBS scene mirror, the sp-* inputs, and `cg-chain-verify`'s cg-obs hop.

## Approaches considered

1. **Chosen:** the YouTube leg is a blocking stage inside the full-path E2E run, with restreamer's session API and camera-box's tool. One rig lease, one run, the owner's "measure to YouTube" in the release gate.
2. **Rejected:** a separate workflow job after the E2E. It needs a second rig lease and a second stream session, so the recording reference would not be the same run's.
3. **Rejected:** YouTube only in restreamer's gate. The owner reversed that scope on 6.10.

## Testing

- **Tool:** fixture tests on the three real sessions' saved tick maps, avsync outputs and audio, which are kept under `~/.claude/work-products/issue-1404/`:
  - the baseline must FAIL (+1368 ms, 46/47 dup/skip, late join);
  - sessions 2 and 3 must PASS.
  
  These are Tier-0 Python tests (pytest), no cargo.
- **Wiring:** static-anchor tests in the `recording-e2e.sh` family, then one live run on the rig with the real API.
