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

## The vendored-C ff_data leak is FIXED (issue 1104, v1.7.0-dev.473)

`obs-ffmpeg-mpegts.c`'s `ffmpeg_mpegts_finalize` used to leak `ff_data` on every failed-start
early-return EXCEPT the `data_init`-failure branch (which already freed it): `init_streams` fail,
`open_output_file != SUCCESS` (the "Failed to open the url" SRT-unreachable crash path), and
`pthread_create != 0` all `return false` without freeing — and the caller's `set_config` `fail:` →
`stop()` skips `full_stop()`/`data_free` because `active()` is false on a failed start. FIXED by
inserting `ffmpeg_mpegts_data_free(stream, &stream->ff_data)` before each of the three early returns
(it is partial-init-safe: guards each field, gates the SRT close on `has_connected`, `av_write_trailer`
on `initialized`, `memset`s at the end — so it is the correct idempotent cleanup at every early exit).
Still defense-in-depth — the listener redesign already removed the crash TRIGGER — so a WER/gdb bench
repro of the ORIGINAL live crash remains the separate, un-done item the ticket describes.

**Reusable pattern — asserting a leak-free on a vendored-C ORCHESTRATOR's early-returns.** A function
like `ffmpeg_mpegts_finalize` calls ~8 helpers + macros, so it can't be feasibly lift-compiled (a
partial lift is a retyped fragment, which `vendored-libobs-change-safety.md` cautions against). The
Tier-0 proof is a static anchor gate (`tests/mpegts_finalize_frees_ff_data_1104.rs`, the
`aux_sender_teardown_ordering_877` idiom): slice the function body (sig → first column-0 `}`), and for
each failure-branch anchor assert the window [anchor → its FIRST `return false;`] contains the cleanup
call — window-to-first-return scoping stops a LATER branch's free from satisfying an EARLIER branch.
Bake in a mutation proof (the SAME predicate over a synthetic no-free fixture that MUST fail + a
with-free one that MUST pass) and watch RED→GREEN via the #1026 `rustc --test` recipe. NB: this rule's
own `paths:` do NOT cover `vendor/obs-studio/plugins/obs-ffmpeg/**`, so it won't auto-load when editing
that plugin — the mpegts output is the same vendored-C / CI-first-compile / anchor-gate class as libobs.
