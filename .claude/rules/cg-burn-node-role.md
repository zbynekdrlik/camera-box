---
paths:
  - "src/cg_chain_gate.rs"
  - "scripts/lib/cg-chain-e2e.sh"
  - "tests/harness_cg_chain_e2e_1301.rs"
  - "tests/harness_cg_chain_e2e_1302.rs"
  - "scripts/cg_chain_scene.py"
  - "tests/python/test_cg_chain_scene_1302.py"
---

# CG-path burn-id node role — SongPlayer (911014 ORIGIN) / cg OBS (911015 HOP), REPORT-ONLY (#1301)

`recording-verdict` proves pixel-level frame contiguity for content that ORIGINATES at the cam2
optical painter (the camera chain) and for the digital node hops (cam1/strih/stream/imag). #1301
adds the SAME kind of proof for content that ORIGINATES in **SongPlayer** (the song-lyrics / CG
source) and flows `SongPlayer → cg OBS (RESOLUME-SNV) → strih → stream`.

## The two roles (the load-bearing modeling decision — do NOT conflate them)

- **SongPlayer = `BURN_RUN_ID_SONGPLAYER` = 911014, a chain ORIGIN role** — like the cam2 painter
  is the origin of the camera chain. It is the content SOURCE whose burn id is tracked THROUGH the
  chain; it is NOT a camera-under-test hop, so **911014 is never in `CAMERA_UNDER_TEST_NODES` /
  `OPTICAL_INJECTION_NODES`** (the three copies: `recording-verdict.rs`, `recording_span_gate.rs`,
  `switch_latency.rs`). SongPlayer PAINTS this burn itself (sender half = zbynekdrlik/songplayer#151,
  contract #1294 §8) — NOT the OBS burn filter.
- **cg OBS = `BURN_RUN_ID_CG` = 911015, a HOP node** — like strih/stream. The DistroAV burn filter
  composites it at `burn_geom::Corner::BottomCenterRight` (the mirror of imag's BottomCenterLeft
  from the right; resolved from the `resolume` hostname in `ndi-burn-filter.cpp`'s host-role map).

Both ids ARE tick-excluded (`src/probe/recording.rs::NODE_BURN_RUN_IDS`) + in the python mirrors
(`qr_align_pins.py`, `mv_skew_snapshot.py`) + in EVERY `all_burns`/`latency_all_burns` exclusion
array + the `#638` push site in `recording-verdict.rs` — because a CG burn can ride into a
strih/stream recording during a CG_CHAIN run and must never hijack the cam2 Vernier tick (the
#463/#312 gotcha).

## The verdict — REPORT-ONLY, provably never touches the camera-chain `overall_pass`

The pure decision is the crate-root `src/cg_chain_gate.rs` (Tier-0, mirrors `imag_tick_gate.rs` +
reuses `burn_hold`): per hop (`cg_obs`/`strih`/`stream`) the SongPlayer (911014) and cg (911015)
burn-id contiguity (presence-only `first..=last`) + max-hold (`MAX_HOLD_FRAMES=4`,
#575-boundary-trimmed). `recording-verdict --cg <path>` emits `report["cg_chain"]`, gated on `--cg`
being supplied (a normal camera run omits it). It folds via `cg_chain_gate::folds_into_overall_pass`,
which is a **no-op while `gates_overall_pass() == false`** — so the camera-chain gate is untouched.

**It is REPORT-ONLY on purpose.** The SongPlayer burn shipped (songplayer 151, 25.9.2026) but no
CG_CHAIN=1 run has produced real data yet, so the hold/decimation behaviour is uncalibrated. The
flip to LIVE is the standard one-line seam
(`gates_overall_pass() -> true`, `verdict-gate-seam-calibration.md`) — but the checklist is MORE
than "calibrate the hold". Before the flip:

1. A real captured cg-OBS frame with the SP burn replaces the generated decode fixture
   (`pattern-change-needs-decode-fixture.md`).
2. **The strih + stream `cg_chain` hops must DECODE sp/cg on the real decode path — they do NOT
   today.** The strih/stream recordings are decoded via `decode_for_grouped` with expected-burn
   lists of `[strih]` / `[strih,stream]` (recording-verdict.rs `main()`) — sp/cg (911014/911015)
   are in NEITHER, so the #207 robust tiling never chases the cg/sp corners on those recordings;
   only whatever the cheap plain+Otsu full-frame pass happens to read shows up. So the "a dropped
   SongPlayer frame shows the SAME missing id at strih AND stream" propagation is proven ONLY in
   the `cg_chain_gate.rs` unit test, NOT end-to-end. The flip MUST first add sp/cg to the
   strih/stream expected-burn decode sets (without regressing the #463 GENERIC_DIAGNOSTIC fast-path
   on normal runs — they are not present there, so gate it to CG_CHAIN runs / a dedicated decode).
3. **Model the cg→strih 60→30 DECIMATION on the strih hop.** strih records at 30fps, so the SP id
   (painted per cg-OBS render) lands DECIMATED in the strih recording — a strict `first..=last`
   presence check would false-FAIL it exactly like the cam-chain #571 case. The strih (and
   stream) hop needs the same decimation-aware treatment (step-aware, or gap-ignore) the camera
   chain already applies, NOT the raw presence check `cg_chain_gate::hop_contiguity` does today
   (which is correct only for the cg-OBS ORIGIN recording, 1:1).
4. A green CG_CHAIN=1 run series calibrates the hold bound (`MAX_HOLD_FRAMES`) against real data.

Until all four hold, flipping `gates_overall_pass()` true would enable a STRUCTURALLY-RED gate
(the strih/stream hops would fail on decode gaps + decimation), not a calibrated one — do NOT flip
blind. The cg-OBS ORIGIN hop (decoded WITH sp/cg in its expected set, 1:1) is the only one
currently honest end-to-end.

## The decode fixture is GENERATED — a real cg-OBS frame MUST replace it

`tests/burn_payload_parity.rs::songplayer_origin_burn_911014_round_trips_through_the_production_decoder_1301`
renders the 911014 payload with the production renderer and decodes it with the production decoder —
proving the fleet decode path reads the new origin run_id, but NOT that it survives the real lossy
chain (projection → grabber → NDI → re-encode). Mine the real fixture from the first CG_CHAIN=1 rig
run (songplayer 151 is deployed since 25.9.2026; a supervisor/rig-ops step), per the #1196 precedent.

## The E2E profile is opt-in + leak-guarded (`scripts/lib/cg-chain-e2e.sh`)

`CG_CHAIN=1` turns the SongPlayer burn ON + cuts cg OBS program to the SP scene + StartRecords cg
OBS at `[5/8]` (no cg recording started ⇒ the burn goes straight back OFF), runs ONE tail CG window
on strih before `[7/8]`, ends the CG leg right after the `[7/8]` StopRecord
(`cg_chain_after_stoprecord`: cg StopRecord keeping the host path, burn OFF, strih restored — so
nothing CG runs through the long on-box decodes), pulls the cg recording at `[8/8d]` (feeding
`--cg`), and — the #246/#844 leak-guard — repeats burn OFF + cg StopRecord + every scene restore in
`cleanup()` even on an early abort (the burn must NEVER stay on the LED wall). cleanup() uses a 3 s
per-request burn timeout (`CG_CHAIN_CLEANUP_BURN_TIMEOUT`) so an unreachable SongPlayer never
stalls the stream/strih teardowns behind it by a minute. All runners are best-effort + loud;
`CG_CHAIN` unset = byte-for-byte inert. A live CG_CHAIN=1 run is a supervisor/rig-ops step, never
unattended.

### The shipped SongPlayer burn API (issue 1302)

- Toggle: `POST {base}/api/v1/ndi/burn` with the JSON body `{"output":"SP-fast","on":true}` (or
  `false`). Base = `CG_CHAIN_SONGPLAYER_API` (default `http://resolume.lan:8920` — SongPlayer runs on
  RESOLUME-SNV 10.77.9.201); output = `CG_CHAIN_SONGPLAYER_OUTPUT` (default `SP-fast`). on/off is in
  the BODY, never the URL.
- Confirmation: `GET {base}/api/v1/ndi/health` returns a JSON ARRAY, one object per output, keyed by
  `ndi_name`; the toggle is verified by that output's `burn_on` (true/false). Absent output, bad JSON
  or a missing `burn_on` = `unknown`, never read as off.
- `cg_chain_songplayer_burn` POSTs + reads back up to `CG_CHAIN_BURN_ATTEMPTS` (3) times. An ON that
  never reads true is a WARNING (the cg_chain section then proves nothing); an OFF that never reads
  false prints a `LEAK` line with the exact manual-off curl. Both always return 0.

### The cg recording pull (issue 1302)

`cg_chain_record_stop` keeps the `obs_phase2.py record --action stop` stdout (the StopRecord
`outputPath`, e.g. `C:\Users\Resolume\Videos\<stamp>.mkv`) in `CG_HOST_RECORDING_PATH`; an empty
answer (the `[8/8d]` / cleanup() re-stop) never clears it. With `CG_CHAIN_PULL_CMD` unset, the
default pull fetches THAT file from `${CG_CHAIN_USER:-newlevel}@<cg-ip>` through the SHARED
`win_ssh_download` (win-ssh-exec.sh — its `win_ssh_scp_source_path` fixes the backslash scp source
that reads "No such file"), bounded by `CG_CHAIN_PULL_TIMEOUT` (900 s) via `timeout bash -c '. lib;
win_ssh_download …'` (`timeout` cannot exec a shell function). Only this run's file is pulled; the
recording now stops right after `[7/8]`, so it is the recording window, not the whole decode. The
merge decodes it on dev1.

### The ONE tail CG window (issue 1302) — why it is NOT a switch-schedule window

- **Every program cut is a HARD CUT.** `cg_chain_scene.py` switches the box to its cut transition
  (found by `transitionKind == cut_transition`, the name can be localized), snapshots the previous
  transition and restores it LAST. A Fade blends the old program into the new one and those frames
  read as undecodable — cg OBS runs a live 2 s Fade (strih ran Cut on 25.9.2026). The sweep's own
  `obs_phase2.py switch` does not need this: every sweep cut is camera→camera over the SAME painted
  QR, so a blend is invisible there.
- **cg OBS:** `scripts/cg_chain_scene.py program` cuts program to `CG_CHAIN_CG_SCENE` (default the
  lower-cased output, `sp-fast` — the live `cg_scenes` has one `sp-*` scene per SongPlayer output)
  BEFORE the cg StartRecord, so the cg recording carries the SP burn for the whole run.
- **strih:** `cg_chain_scene.py strih-solo` finds the ONE scene carrying `CG_CHAIN_STRIH_INPUT`
  (default `CG-obs`, the `RESOLUME-SNV (cg-obs)` ndi_source; live 25.9.2026 only `CG bridge` carries
  it), shows ONLY that item (live, `CG-obs` is DISABLED in `CG bridge` and a `CG-presenter` browser
  overlay sits on top — cutting to the scene as-is would show the overlay, not the burns) and cuts
  program to it, then runs obs_phase2's own polled `_assert_program_nonblack` (`min_mean=0`,
  peak-only: an idle SongPlayer frame can be dark, a dead CG-obs is not). Several scenes carrying it
  = fail loud unless `CG_CHAIN_STRIH_SCENE` names one.
- **It is a TAIL window after the last camera window, never an entry in `switch-schedule.json`:** a
  schedule window with no cam2 tick FAILS the per-cambox verdict (zero in-window frames), and a
  mid-run CG cut would land INSIDE the optical span (undecodable frames against the live floor). After
  the last camera window the CG frames are outside every schedule window and a trailing no-tick run
  past the last optical read — the tail placement + the hard cut are DESIGNED to keep the
  camera-chain verdict out of it; the first live CG_CHAIN=1 run is what confirms it (watch the
  undecodable counts and the A/V marker pairing on that run). The window is recorded to
  `$OUTDIR/cg-window.json` (`{"kind":"cg","scene","input","start_ns","end_ns"}`) and echoed into the
  log — the run's evidence of WHEN strih carried the chain; no verdict code reads it yet (scoping the
  strih/stream `cg_chain` hops to it is a follow-up). Never cut strih BACK to a camera before
  StopRecord — a cam burn reappearing after the gap opens missing ids in its `first..=last`.
- **Snapshot-before-mutate:** every scene change writes its restore snapshot
  (`$OUTDIR/cg-chain-{cg-program,strih-scene}-state-<RUN_ID>.json`: host, scene, previous program,
  previous transition, every item's enabled state) BEFORE changing anything; `restore` replays it
  (program first, then items, then the transition) and renames the file `.restored`, so the cleanup()
  second pass is a no-op. The RUN_ID key means a reused OUTDIR never replays another run's snapshot.
  strih is restored right after the `[7/8]` StopRecord, again in cleanup(). A failed restore keeps
  the snapshot and prints the manual restore command. `cg_chain_scene.py` closes every WS session it
  opens.
- `recording-e2e.sh` wiring adds NEW lines only (plus rewording three stale comment/echo lines of the
  #1301 block that said the SongPlayer burn was unshipped): `CG_CHAIN_STATE_DIR="$OUTDIR"` next to
  `CG_RECORDING`; the burn-back-OFF line inside the `[5/8]` CG block; `if cg_chain_window_due; then
  cg_chain_window "$STRIH" …; fi` between the sweep/hold `fi` and the `[7/8]` banner (its comment
  must never contain the `[7/8]` literal — a test anchors on the FIRST `[7/8]` in the file); and
  `cg_chain_after_stoprecord …` after the `imag host file` echo. Anchor gotcha hit here: `[5b/8]`
  first appears in a source-block comment near the top — anchor on `# [5b/8] #707 B1`.
- Tier-0: the pure bash builders + the fake-curl / fake-python3 / fake-scp runners are driven by
  `tests/harness_cg_chain_e2e_1302.rs` (std-only — it RUNS locally with plain `rustc --test` +
  `CARGO_MANIFEST_DIR`, from a script file in a worktree); the scene helper is pytest
  (`tests/python/test_cg_chain_scene_1302.py`, a scripted fake OBS that refuses a program cut under
  anything but Cut).
- **Follow-up before trusting the strih/stream hops (out of this slice):** the strih/stream
  `--extract-partial` decode sets do not include 911014/911015 (item 2 above), and the cg hop burn is
  decimated on the 30 fps strih recording (item 3), so `cg_chain.strih` / `.stream` may read empty
  or gapped on the first live run even with the window working.

## Adding ANOTHER chain-origin/hop pair later

Reserve a fresh `BURN_RUN_ID_*` → add to `NODE_BURN_RUN_IDS` + the two python mirrors + every
`all_burns` array; a HOP that composites an OBS burn also needs a new `burn_geom::Corner` synced
across the #463 FOUR mirrors (burn-geom.hpp, colour_sample.rs, colour_scale.rs test,
burn_payload_parity.rs) + the burn-filter host-role map. Reuse `cg_chain_gate` (it is generic
per-hop contiguity+hold), never a parallel copy. The `recording-decode` skill's "new node role"
GOTCHA has the full ~5-site checklist.
