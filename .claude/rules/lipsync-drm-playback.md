---
paths:
  - "scripts/lipsync-test-mode.sh"
  - "tests/harness_lipsync_test_mode.rs"
---

# lipsync-test-mode.sh — DRM/KMS playback (mpv), NOT raw fbdev (#1187)

cam2's lipsync TEST-mode PLAYBACK runs through **`mpv --vo=drm`**, never a raw `/dev/fb0` write.
`mpv` takes the DRM master, page-flips its own buffers at vblank (never touches fb0) and cleanly
restores the CRTC on exit — this is the STRUCTURAL fix for the #1176 stale-frame leak (a killed
`ffmpeg -f fbdev` left its last frame resident in fb0 memory, revealed on cam2's HDMI once the DRM
master was released). Do NOT reintroduce an `-f fbdev` sink for playback.

## The `--audio-delay` SIGN is the easy thing to get wrong (silent lipsync break)

`LIPSYNC_AUDIO_LEAD_MS` compensates the ~408 ms ALSA output-pipeline depth on `hw:CARD=PCH,DEV=3` by
DELAYING THE VIDEO relative to audio (the old ffmpeg code did this with a positive `-itsoffset` on
the video demux). mpv's `--audio-delay` semantics: **positive delays audio, NEGATIVE delays video.**
So the mapping is `--audio-delay = -(lead_ms/1000)` (e.g. `408` → `--audio-delay=-0.408`; `0` is
special-cased to `0.000` to avoid `-0.000`). Flipping the sign does not cancel the offset — it
DOUBLES it. The knob is validated non-negative upstream (`case` guard), so the value is always ≤ 0.

The 408 ms VALUE was calibrated on the ffmpeg/ALSA path; mpv's ALSA buffering may differ, so it may
need re-derivation. It stays a knob precisely so the supervisor re-tunes via the paired cross-check
campaign (`scripts/lipsync-cross-check.sh`) WITHOUT a code change — never hardcode a new value blind.

## mpv command shape (one process, one pidfile — unchanged lifecycle)

`nohup mpv --no-config --no-terminal --vo=drm [--drm-device=<dev>] --loop-file=inf
--audio-device=alsa/<AUDIO> --audio-channels=stereo --audio-delay=<sec> '<media>'` — one process
feeds BOTH sinks (video DRM, audio ALSA), tracked by ONE pidfile with a fail-loud `kill -0`
liveness check. `--audio-channels=stereo` mirrors the old `-ac 2` (the device refuses mono).
`LIPSYNC_DRM_DEVICE` empty = mpv auto-selects the connected KMS card (#854: `/dev/dri/cardN` is not a
stable ABI, so auto is the safe default); a non-empty value pins `--drm-device`.

## Stop path blanks fb0 belt-and-braces — REUSE the canonical #660 builder

`lipsync_stop_playback_cmds` blanks fb0 after the kill by embedding
`$(rig_test_ledger_clean_paint_fallback_cmds "$fb")` (sourced from `scripts/lib/rig-test-ledger.sh`,
a pure lib that deliberately never sets `set -euo pipefail`). ONE source of truth for the blank —
never hand-roll a second `dd if=/dev/zero of=/dev/fb0 ...`. Why still needed after moving off fbdev:
after ANY DRM master release the kernel fbdev emulation re-takes scanout from fb0 memory, so the
legacy surface must still be neutralized (relates to the open #1173 deadman half).

## Testing preflight / mpv-presence deterministically — the `LIPSYNC_MPV_BIN` seam

The preflight does `command -v "$LIPSYNC_MPV_BIN"` (default `mpv`) + an
`mpv --vo=null --ao=null --frames=120` decode probe (touches neither fb0 nor the CRTC). To test the
three branches without PATH-shadowing (a real mpv on dev1/CI would perturb it): set `LIPSYNC_MPV_BIN`
to an absolute-path fake `exit 0` (present+decodes → PASS), `exit 3` (decode-fail → FAIL loud), or a
bogus name that does not exist (missing → FAIL loud). This is the same env-seam-not-PATH pattern the
repo uses elsewhere for a `command -v X` check.

## Provisioning + Tier-0

mpv is installed in `setup-device.sh` STEP 16 (same fail-loud apt line as ffmpeg; mpv Depends on the
ffmpeg libav* codecs, so `--no-install-recommends mpv` still decodes H.264) and acceptance-checked by
`verify-device.sh`'s `(x2)` check (inserted BEFORE `(q)`, mirroring the `(x)` ffmpeg check). Tier-0:
no local cargo — verify the pure `*_cmds`/`lipsync_preflight_cmd` builders by sourcing the script and
calling them directly (their generated remote-bash text is asserted byte-for-byte by the harness),
plus `bash -n` / `shellcheck` / `rustfmt --edition 2021 --check`. Live mpv/DRM playback on cam2 is a
supervisor rig step (the cam2 painter is untouched by code+tests work).
