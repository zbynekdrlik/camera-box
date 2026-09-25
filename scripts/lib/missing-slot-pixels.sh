#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions; the only top-level statement is the guarded
# win-ssh-exec.sh source below) — the sibling scripts/lib/*.sh convention: sourcing runs in the
# CALLER's shell (recording-e2e.sh already sets `set -euo pipefail`), and every runtime function
# ALWAYS returns 0 on its best-effort paths so it can never trip the caller's `set -e`.
#
# scripts/lib/missing-slot-pixels.sh — issue 1367: pixel proof for every CLASSIFIED missing /
# unreadable slot the merge could not extract.
#
# WHY: the verdict classifies a slot (`full_chain.loss.<node>.classified[]`, e.g. BURN-UNREADABLE)
# in the MERGE on dev1, where the recordings are not present, so those entries keep `png: null`. The
# on-box `--extract-partial` flags only its own undecodable / missing-burn frames, and the #652
# cleanup plan then removes the recordings. Nobody could SEE a damaged frame (blocky NDI decode,
# tear, blend, blur — each points to a different cause).
#
# WHAT: after the merge, before the Discord report and the #652 cleanup plan, for the classified
# slots with `png == null` (capped, MISSING_SLOT_PIXELS_CAP, default 12 per run) this exports the
# slot frame plus its two neighbours as PNG ON the box that holds the recording (camN -> the strih
# recording, strih/stream -> the stream recording — the verdict's NodeSpec pairing), pulls them to
# `$OUTDIR/<node>-missing/`, logs every path, and records a `missing_slot_pixels` block in the
# verdict JSON the Discord report reads. Best-effort: it never changes the verdict or $GATE.
#
# THE INDEXING CONTRACT (the load-bearing part): the verdict's `frame_index` is the ordinal of the
# raw frame on the pipe of `ffmpeg -v error -nostdin -i <rec> -f rawvideo -pix_fmt gray pipe:1`
# (src/probe/recording.rs `read_frames`). That output is CFR: a timestamp gap in the recording is
# filled with a duplicate, so a naive `select=eq(n,k)` on the source is off after the first gap. So
# stage 1 here IS that decode, byte-for-byte, and stage 2 selects by raw-frame ordinal:
#   ffmpeg <verdict decode> | ffmpeg -f rawvideo -pixel_format gray -video_size WxH -framerate 1
#     -i pipe:0 -vf select=... -fps_mode passthrough -frame_pts 1 frame-%d.png
# With `-framerate 1` the pts of each raw frame IS its ordinal, so `-frame_pts 1` names every PNG
# after the verdict frame_index. The PNGs are GRAY — exactly the luma the decoder saw.
# tests/harness_missing_slot_pixels_1367.rs pins this against a VFR fixture with real ffmpeg.

if ! declare -F win_ssh_ps_encoded_command >/dev/null 2>&1; then
  # shellcheck source=scripts/lib/win-ssh-exec.sh
  . "$(dirname "${BASH_SOURCE[0]}")/win-ssh-exec.sh"
fi

# RED stub (issue 1367): every function is defined but does nothing yet.
missing_slot_pixels_cap() { return 0; }
missing_slot_pixels_box_for_node() { return 0; }
missing_slot_pixels_frames() { return 0; }
missing_slot_pixels_indices() { return 0; }
missing_slot_pixels_select_expr() { return 0; }
missing_slot_pixels_decode_head() { return 0; }
missing_slot_pixels_decode_tail() { return 0; }
missing_slot_pixels_stage2_args() { return 0; }
missing_slot_pixels_lowprio_snippet() { return 0; }
missing_slot_pixels_linux_script() { return 0; }
missing_slot_pixels_ps_quote() { return 0; }
missing_slot_pixels_windows_ps() { return 0; }
missing_slot_pixels_box_extract() { return 0; }
missing_slot_pixels_run() { return 0; }
missing_slot_pixels_manifest() { return 0; }
