---
paths:
  - "scripts/imag_scenes.py"
  - "scripts/imag_record_encoder.py"
  - "scripts/imag_record_stats_capture.py"
  - "src/record_render_stats.rs"
  - "src/probe/recording_partial.rs"
---

# imag E2E record encoder — VAAPI-tex, its make-it-live mechanism, and the record-render observability (#1143)

The imag E2E records its OBS PROGRAM for the zero-loss verdict. Recording with **software x264**
overloads the 30W-PL1-clamped imag-nb (render thread past 16.67ms → OBS ~18.4% "lagged" → the
recording repeats ~19.5% of frames = OBSERVER EFFECT, #1130). The fix is the Intel iGPU HW encoder.

## Encoder choice — `ffmpeg_vaapi_tex`, NEVER QSV, x264 only as last-resort fallback

- **`ffmpeg_vaapi_tex`** (FFmpeg VAAPI H.264, TEXTURE / GPU zero-copy — no CPU frame download, no CPU
  encode) is LIVE-PROVEN on this box (2026-08-20): records valid H.264 **High 1080p60**, render held
  ~4ms / ~0% lagged. This is the no-dGPU choice in `imag_record_encoder.choose_record_encoder`.
- **QSV (`obs_qsv11_v2`) is BROKEN here** — #847 live-proved libmfx Texture-interop
  `MFX_ERR_UNSUPPORTED` at `Init()`. NEVER choose it (there is an explicit never-QSV test guard).
- **x264** is the graceful fallback ONLY when VAAPI is genuinely unavailable — it is what causes the
  observer effect, so it is never preferred.
- dGPU box → NVENC (`obs_nvenc_h264_tex`), unchanged from #502/#847.
- **VAAPI-tex requires `[Video] ColorFormat=NV12`** (verify in basic.ini) and the render node
  `/dev/dri/renderD128`. Rate control = **CQP qp 18-22** (default 20), NOT CBR/VBR: on the low-power
  EncSliceLP entrypoint CBR/VBR need HuC firmware, CQP sidesteps that AND preserves QR/burn detail
  (disk covered by #1122 retention). `recordEncoder.json`: `{vaapi_device, rate_control:CQP, qp,
  keyint_sec, bf:0, profile:100 (H264 High)}`.

## Make-it-live: a WS `SetProfileParameter(RecEncoder)` does NOT take effect without an OBS restart

#847 already documents this trap; #1143 pins the exact working APPLY ORDERING (`imag_scenes.py
ensure_rec_encoder`, live-validated x264→VAAPI→verify):

1. **WS `SetProfileParameter(AdvOut, RecEncoder, target)` FIRST** (persists into OBS's in-memory +
   disk config so its own shutdown-save keeps it).
2. **`systemctl --user stop imag-obs`** (USER unit, `export XDG_RUNTIME_DIR=/run/user/$(id -u)`;
   NEVER a direct `imag-obs-start.sh` call — #1015).
3. **Write `recordEncoder.json` while OBS is DOWN** — a RUNNING OBS clobbers it on its clean-shutdown
   save, and WS `SetProfileParameter` CANNOT write it at all (it writes basic.ini keys only;
   `recordEncoder.json` is the separate advanced-output encoder-settings file).
4. **`systemctl --user start imag-obs`** → OBS builds the record encoder FROM DISK at startup.
5. **Reconnect WS + verify** the read-back.

Apply ONLY when needed (`record_encoder_apply_plan`): a make-it-live restart every run is too
invasive; a one-time self-healing restart on config drift is a no-op on the steady state, and its
NDI/shader settle is absorbed by the [1/8] render-health window-1 #882 warm-up.

## Three sharp gotchas reading/verifying the encoder state on the box

- **`[AdvOut] RecEncoder` must be read SECTION-SCOPED.** basic.ini has TWO `RecEncoder=` keys:
  `[SimpleOutput] RecEncoder=x264` (appears FIRST) and `[AdvOut] RecEncoder=obs_x264` (the one the
  advanced record output actually uses). A naive `grep -m1 '^RecEncoder='` returns the WRONG
  (SimpleOutput) value → the apply-plan reads `x264` forever → restarts every run. Use an awk scoped
  to `/^\[AdvOut\]/{a=1} /^\[/{a=0} a && /^RecEncoder=/`.
- **"disk says VAAPI" ≠ "OBS is running VAAPI".** OBS builds the encoder at startup from disk; the
  boot-time `imag_scenes.py --bootstrap` seed then re-writes `RecEncoder` (config-only, does NOT
  rebuild the live encoder). So the reliable liveness proof is **OBS's start time vs
  `recordEncoder.json` mtime** (`_obs_started_after_record_json`: `systemctl --user show imag-obs -p
  ActiveEnterTimestamp` epoch > `stat -c %Y recordEncoder.json`), folded into `renc_ok`. Ground-truth
  the actual live encoder from the OBS log's `[FFmpeg VAAPI encoder: 'advanced_video_recording']` vs
  `[x264 encoder: …]` line at the next StartRecord — not from disk.
- **`seed_profile` seeds VAAPI but must NOT write `recordEncoder.json`** (OBS is UP during a seed →
  clobber). The E2E `ensure-rec-encoder` step (OBS down) is the only writer.

## record-render observability — carried through the partial, report-only

- OBS's stop-stats have TWO shapes (`imag_record_encoder.parse_obs_record_stats` handles both):
  **x264**: `Total drawn frames: N (M attempted)` + `Number of lagged frames due to rendering
  lag/stalls: L (P%)`. **VAAPI-clean**: `Total drawn frames: N` (no `(attempted)`) and NO lagged line
  at all (OBS omits it when ~0 lagged) → lagged=0, pct=0.0, attempted=drawn. Also parses the max
  in-record `program-render-audit avg_frame_ms` (Task 4: render budget DURING record).
- Carried as `RecordingPartial.record_render: Option<RecordRenderStats>` (crate-root type, `#[serde(
  default)]`, **NO `Eq` derive** — it has f64 fields, #726) — PARTIAL_SCHEMA_VERSION bump (the #1118
  sha-gate redeploys the on-imag binary). Surfaced report-only under `full_chain.loss.imag`
  (`record_render_lagged_pct` etc.); NEVER gates `overall_pass` (the blocking flip is #1142's job).
- **Passing the JSON dev1→imag survives `recording-verdict-on-imag.sh`'s `printf %q` forwarding**
  (verified): `--record-render-stats '{"drawn_frames":…}'` re-parses correctly after ssh + remote
  bash. The dev1 harness captures it AFTER StopRecord via `imag_record_stats_capture.py` (ssh-greps
  the record window from the newest imag OBS log, parses, prints JSON) and passes it to the extract;
  best-effort (empty on any failure → no `record_render` carried, report-only).
