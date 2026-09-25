---
paths:
  - "scripts/lib/missing-slot-pixels.sh"
  - "tests/harness_missing_slot_pixels_1367.rs"
  - "tests/python/test_e2e_discord_report_missing_slot_pixels_1367.py"
---

# Pixel proof for classified slots the merge could not extract (issue 1367)

The verdict classifies missing / unreadable slots (`full_chain.loss.<node>.classified[]`) in the
MERGE on dev1, where the recordings are not present, so those entries carry `png: null`. The on-box
`--extract-partial` only flags its own undecodable / missing-burn frames. So a merge-time
BURN-UNREADABLE frame had no picture, and the #652 cleanup plan then removes the recordings.
`scripts/lib/missing-slot-pixels.sh` closes that: right after the merge (after the genlock-audit
snapshot, before the Discord report and the `[8/8e]` cleanup plan) `missing_slot_pixels_run`
exports the slot frame plus its two neighbours as PNG ON the box holding the recording, pulls them to
`$OUTDIR/<node>-missing/frame-<index>.png`, logs every path, writes
`$OUTDIR/missing-slot-pixels-<RUN_ID>.json` and merges it into the verdict JSON as
`missing_slot_pixels`. Best-effort and report-only: the call is `|| true`, it never touches `$GATE`
or `overall_pass`.

## The indexing contract — never select on the source file

The verdict's `frame_index` is the ordinal on the pipe of
`ffmpeg -v error -nostdin -i <rec> -f rawvideo -pix_fmt gray pipe:1` (`read_frames` in
`src/probe/recording.rs`). The rawvideo output is **CFR**: a timestamp gap in the recording is
filled with a duplicate (measured: a 29-packet VFR fixture decodes to 30 frames). A `select=eq(n,k)`
on the source file is therefore off by one after every gap. So:

- stage 1 IS the verdict decode, byte-for-byte (`missing_slot_pixels_decode_head/_tail`, pinned to
  the Rust argument arrays by a test);
- stage 2 reads that raw stream with `-framerate 1`, so each frame's pts IS its ordinal, selects
  by `n`, and writes with `-fps_mode passthrough -frame_pts 1 frame-%d.png` — the file name is the
  verdict frame_index; `-frames:v <count>` ends stage 2 early (stage 1 then dies on the broken pipe).

The PNGs are GRAY — the luma the decoder saw — like the existing on-box pixel proofs. The harness test
pins this with REAL ffmpeg against a VFR fixture (the Test/coverage CI jobs install ffmpeg).

## Where each node's slots live

camN -> the strih recording (the camera burns are read from it); `strih` / `stream` -> the stream
recording; anything else (imag, the `cam2_*` optical node) is skipped. Same pairing as the verdict's
NodeSpec `source`. The cap (`MISSING_SLOT_PIXELS_CAP`, default 12) counts SLOTS, sorted by
(node, index); each slot brings up to 3 PNGs.

## Per box

- **strih-lx (Linux)**: one ssh bash script (`missing_slot_pixels_linux_script`, every dynamic value
  `%q`-quoted — OBS file names have spaces) probes the size, then runs both stages at the SAME idle
  priority as the on-box decode: `missing_slot_pixels_lowprio_snippet` is byte-identical to
  `recording-verdict-on-strih-lx.sh`'s `LOWPRIO_SNIPPET` (a test pins the two equal; issue 1354 —
  a P-core bulk decode relock-storms the live NDI receivers). Remote dir
  `$STRIH_LX_REMOTE_OUT_DIR/missing-slot-pixels-strih-<RUN_ID>`, scp -r back, then the remote PNGs
  and the dir are removed (never a sweep: our own run-keyed dir, `rm -f` of its `frame-*.png`, then
  `rmdir`).
- **stream (Windows)**: Windows PowerShell 5.1 pipes between native programs are NOT binary-safe,
  so the PowerShell program (`missing_slot_pixels_windows_ps`, via `-EncodedCommand`) sets the
  shared `onbox_decode_priority_class` priority, writes the two-stage pipeline to an `extract.cmd`
  and runs it through `cmd.exe` (cmd's `|` is binary-safe). In the `.cmd` the output pattern's `%` is
  doubled (`frame-%%d.png`). A recording path with `"` `%` `^` `&` `|` `<` `>` is refused (the
  `.cmd` line cannot carry it). Pull via `win_ssh_download_dir`, then the files are removed.
- Each box's ssh export is bounded by `MISSING_SLOT_PIXELS_TIMEOUT` (600 s) — `timeout` wraps
  `sshpass` directly (never a shell function).

## Tier-0

`tests/harness_missing_slot_pixels_1367.rs` runs locally with plain `rustc --test` + a `tempfile`
stub rlib (`CARGO_MANIFEST_DIR` set): the pure builders, the VFR index pin, the Windows program text,
the whole runner against a fake `sshpass` first on PATH (ssh = run the remote command with local
bash, scp = local `cp -r`; a `powershell` command fails, which exercises the best-effort failure
path), and the recording-e2e.sh order (merge < this step < the Discord report < the cleanup plan).
The report side is `tests/python/test_e2e_discord_report_missing_slot_pixels_1367.py`.
