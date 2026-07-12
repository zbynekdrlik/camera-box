#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects), mirrors the sibling
# scripts/lib/capture-rate-guard.sh / scripts/lib/ndi-alive.sh convention.
#
# scripts/lib/imag-gpu-guard.sh — imag-nb GPU VRAM headroom preflight (#709 prevention).
#
# #709 root cause (live-diagnosed 2026-07-12): imag-nb's obs64-equivalent (plain `obs` on Linux,
# `#458` provisioning) had run for 5 days without a restart. Its own render pipeline (6 continuous
# NDI genlock feeds + the built-in Multiview, #501) had leaked GPU VRAM up to 6872MiB out of the
# box's 8151MiB total — leaving only ~1058MiB free. `StartRecord` over the WebSocket returned
# `result:true` (the RPC itself succeeded — obs-websocket only calls the frontend API, it never
# verifies the encoder actually initialized), but OBS's own log showed the REAL failure a few
# hundred ms later: `[obs-nvenc] init_encoder_h264: nv.nvEncInitializeEncoder(...) failed: 10
# (NV_ENC_ERR_OUT_OF_MEMORY)`, twice (OBS's own texture->non-texture encoder fallback), then
# "Already in non_texture encoder, can't fall back further!" — the recording silently never
# started. The EXISTING #627 post-start liveness check correctly caught the dead-on-arrival
# recording and aborted the run (working as designed) — but with no upfront diagnostic, the
# failure read as an opaque "recording FAILED the post-start liveness check", giving no hint that
# the actual cause was GPU memory exhaustion. A plain `pkill -9 -x obs` + relaunch (matching
# setup-imag.sh's own hot-swap relaunch pattern) freed the leak back down to 302MiB used —
# confirmed the diagnosis and fully restored recording (verified live: two short test recordings
# both wrote real, growing bytes).
#
# This preflight reads the box's CURRENT free VRAM (nvidia-smi) BEFORE StartRecord ever runs and
# FAILS FAST with an actionable message — the same fail-fast-before-burning-a-run philosophy as
# the #365 frozen-camera gate, the #656 capture-rate preflight, and the #355 orphan-finalize wait
# — instead of letting a doomed run burn ~10s discovering the #627 liveness failure with no
# root-cause hint.
#
# Source-only: this file defines pure functions and performs no side effects on its own.

# imag_gpu_query_cmd -> the REMOTE nvidia-smi command text that prints ONLY the free VRAM in MiB
# as a single integer line (no header, no units) for the box's GPU 0 — the same value `nvidia-smi`
# reports on its own dashboard line, just script-parseable.
imag_gpu_query_cmd() {
  echo 'nvidia-smi --query-gpu=memory.free --format=csv,noheader,nounits -i 0 2>&1'
}

# imag_gpu_free_mib_from_query OUTPUT -> the parsed free-VRAM MiB integer from imag_gpu_query_cmd's
# raw output, or empty when unparseable (nvidia-smi missing/erroring, driver down, empty output) —
# an unreadable value is NEVER silently treated as "plenty free" (mirrors _wait_record_idle's
# "unreadable status = treated as still active, never a silent pass" convention in obs_phase2.py).
# Takes only the FIRST line (a multi-GPU box would print one line per GPU; -i 0 above already
# scopes to GPU 0, this is belt-and-braces against a driver that ignores -i).
imag_gpu_free_mib_from_query() {
  local out="$1" first_line
  first_line="$(printf '%s\n' "$out" | head -1 | tr -d '[:space:]')"
  case "$first_line" in
    '' | *[!0-9]*) return 1 ;;
    *) echo "$first_line" ;;
  esac
}

# imag_gpu_headroom_ok FREE_MIB MIN_MIB -> "true"/"false" (pure integer comparison; both args
# MUST already be validated non-empty integers by the caller — this function does not re-validate,
# matching the sibling guards' "parse once, decide once" shape).
imag_gpu_headroom_ok() {
  local free_mib="$1" min_mib="$2"
  if [ "$free_mib" -ge "$min_mib" ]; then
    echo "true"
  else
    echo "false"
  fi
}

# imag_gpu_preflight_message FREE_MIB MIN_MIB -> the operator-facing fail message, a pure string
# formatter (no I/O) so it is directly unit-testable — names the #709 root cause AND the proven fix.
imag_gpu_preflight_message() {
  local free_mib="$1" min_mib="$2"
  echo "imag-nb GPU has only ${free_mib}MiB VRAM free (need >= ${min_mib}MiB) — the #709 leak class (a long-uptime OBS render pipeline leaking VRAM until StartRecord's NVENC encoder init fails with NV_ENC_ERR_OUT_OF_MEMORY). Restart OBS on imag-nb (pkill -9 -x obs, then relaunch — see #709 / .claude/skills/e2e) before retrying."
}

# imag_gpu_unreadable_message -> the fail message for when nvidia-smi's output could not be
# parsed at all (distinct from "low but readable" — never conflate the two diagnostics).
imag_gpu_unreadable_message() {
  echo "imag-nb GPU free-VRAM query (nvidia-smi) returned an unreadable value — cannot confirm headroom before StartRecord. Check nvidia-smi / the NVIDIA driver on imag-nb directly."
}
