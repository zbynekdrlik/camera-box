---
paths:
  - "scripts/srt_tap.py"
  - "scripts/av-sync-tap-bench.sh"
  - "scripts/av_sync_measure.py"
---

# SRT A/V-sync tap safety — listener-not-caller, or OBS crashes (#802)

The A/V-sync monitor (#801) taps the stream program as an SRT copy (`srt://…:9998`). On
2026-07-19 (LIVE) starting an Aitum Multistream SRT-**CALLER** output against an unreachable
listener crashed the WHOLE OBS process (failed-start cleanup in vendored
`obs-ffmpeg-mpegts.c`: `connect_mpegts_url`→`libsrt_open`<0 → "Failed to open the url" → died).
VLC 3.x SRT is **caller-only**, so the intended local listener never bound.

## The rule: the OBS-side tap output MUST be an SRT LISTENER

`srt://0.0.0.0:9998?mode=listener` — a listener BIND succeeds immediately with **no peer**, so
the failing-start crash trigger structurally cannot occur; the player (VLC/ffmpeg, caller-capable)
connects on demand as the CALLER. `scripts/srt_tap.py` is the single source of truth:

- `assert_safe_to_start(url)` REFUSES any `caller`/`rendezvous` tap (incl. the libsrt/ffmpeg
  default of no `mode=` → caller) and hands back the listener URL; a listener URL always passes.
- `recommend_tap_url(port)` → the canonical `…?mode=listener` URL.
- camera-box CANNOT create the Aitum output over obs-websocket (no such WS request), so it can't
  physically stop a hand-configured caller output — the guard + bench + canonical config make the
  safe path the easy path; the operator/automation must adopt it.

## `probe_udp_port_refused` asymmetry (don't "fix" it)

SRT is UDP and a listener does NOT answer non-SRT probe bytes, so you can NEVER positively confirm
a remote SRT listener without an SRT handshake library. `probe_udp_port_refused` returns True ONLY
on a definitive ICMP port-unreachable (`ConnectionRefusedError` from a connected-UDP send/recv on
Linux) — i.e. PROVABLY nothing there. Everything else (a reply, a timeout, an unusable socket) is
False = "not provably empty". So `reader_should_grab` fails **OPEN**: only a provably-dead tap
short-circuits to NO-SIGNAL; a live-but-quiet listener is never false-rejected. `--tap-preflight`
in `av_sync_measure.py` is opt-in and a no-op without the flag (zero change to the #806/#814 flow).

## Bench technique: prove a listener start can't fail on a missing peer

`scripts/av-sync-tap-bench.sh` runs the guard checks + a REAL ffmpeg SRT-listener probe: start
`ffmpeg … -f mpegts "srt://0.0.0.0:PORT?mode=listener"` with NO player, `sleep 2`, assert the pid
is still alive (a listener output blocks waiting for a caller — it does NOT fail on the missing
peer). Gate it on `ffmpeg -protocols | grep -qw srt` (SKIP where the local ffmpeg lacks libsrt;
run it on the bench OBS box). Everything is pure-stdlib / pytest, Tier-0 (`python -m pytest
tests/python/test_srt_tap.py`) — no rig.

## The vendored-C leak stays bench-gated (issue 1104)

`obs-ffmpeg-mpegts.c`'s `ffmpeg_mpegts_finalize` leaks `ff_data` on the "Failed to open the url"
early-return (the `data_init`-failure branch frees it; this one doesn't). Filed as issue 1104 —
NOT patched blind: unprovable as THE crash without a WER/gdb backtrace (crash was live, no
coredump, not reproducible in Tier-0), high blast-radius (same output type OBS uses for real
SRT/RTMP streaming), needs the full vendored-libobs-change-safety gauntlet + a bench repro first.
The listener redesign already removes the trigger, so that fix is defense-in-depth, not primary.
