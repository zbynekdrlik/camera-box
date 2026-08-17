---
paths:
  - "vendor/distroav/src/ndi-source.cpp"
  - "tests/distroav_ndi_reconnect_767.rs"
  - "tests/distroav_recv_create_retry_1080.rs"
---

# DistroAV NDI receiver-thread lifecycle — a `break` is a PERMANENT, reattach-proof death (#1080)

The one fact that governs every error path in `ndi_source_thread`
(`vendor/distroav/src/ndi-source.cpp`):

**A `break` out of the `while (s->running)` receiver loop NEVER sets `s->running = false`** — only
`ndi_source_thread_stop()` does that. So after a `break` the thread runs its cleanup (recv_destroy,
framesync_destroy, free names) and RETURNS, but `s->running` stays `true`. `ndi_source_update()`
(the reattach / SetInputSettings entry point) then hits `if (s->running) { …only set the reset
flag… } else { ndi_source_thread_start(s); }` — it sees `running == true`, so it just sets a reset
flag the DEAD thread will never read, and NEVER restarts the thread. The source is **permanently,
reattach-proof black** until a human recreates it (or a hide→show that routes through
`ndi_source_thread_stop`, which needs a non-`PROP_BEHAVIOR_KEEP_ACTIVE` behavior).

Consequences for any change here:

- **NEVER `break` on a RECOVERABLE error in the receiver loop.** Retry in place instead: blank the
  source (`process_empty_frame(s)`), back off, re-arm `reset_ndi_receiver = true` under
  `config_mutex`, set `was_disconnected = true`, and `continue` (which re-runs the whole reset
  block — the flag is cleared at the top and re-armed by you). #1080 fixed the `recv_create_v3`
  NULL break this way (pure `ndi_recv_create_retry_backoff_ns` helper, 250 ms→3 s bounded backoff,
  retry COUNT never capped, chunked 100 ms sleep so OBS shutdown is never blocked). The sibling
  `framesync_create` NULL break is the SAME class, tracked as #1097 (currently UNREACHABLE — the
  genlock forcer sets `PROP_FRAMESYNC` false, so `snap_framesync_enabled` is always false here).
- **The #767 stale watchdog makes the reset block reachable UNATTENDED** (it sets
  `reset_ndi_receiver` autonomously on a genlocked+connected+silent source), so ANY `break` in the
  reset block is now an unattended permanent death, not just a human-triggered one.

## The std-only gate pattern for a DistroAV-receiver-loop-only decision helper

A pure `static inline` decision/backoff helper here has NO Rust appliance consumer, so DON'T invent
a crate-root module to parity-check it. Make the gate self-contained (`distroav_ndi_reconnect_767.rs`,
`distroav_recv_create_retry_1080.rs`): Facet A `fs::read_to_string` source-anchors the tokens (revert
protection against a `git subtree pull`); Facet B lifts the helper VERBATIM by signature → first
`\n}\n`, compiles it with `cc -Werror -Wconversion -Wformat=2` against a tiny `<stdint.h>` stub, and
runs a hand-written truth table (the truth table IS the spec). Runs BOTH under `cargo test` AND
offline via the #1026 recipe: `CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021
tests/<file>.rs -o /tmp/x && /tmp/x`. **Watch the truth table go RED under a scratch mutation** — a
gate never seen fail is unproven (#1003). Mirror the key token anchors into BOTH
`windows-genlock.yml` AND `windows-genlock-fast.yml` (the fast path hot-swaps `distroav.dll`
un-gated, #912) — verify each pwsh literal offline against the `re.sub(r'\s+',' ',text)`-squished
file (pwsh is not on dev1). A source anchor that only the truth table can't reach (e.g. an
overflow shift-clamp on x86) still needs an explicit source-anchor assertion.

## Do NOT conflate the `break` silent-death with the #1096 wedge

The live strih wedge (#1096) is a DIFFERENT failure: `recv_create_v3` SUCCEEDS (non-null) but the
new receiver, created connect-BY-NAME, never re-resolves a RESTARTED sender (rotated port) because
the long-lived in-process NDI finder state is poisoned — cured only by an OBS restart. #1080's
retry (which fires only on a NULL create) does not enter there and does not cure it. The #1096 cure
direction is a FRESH `NDIlib_find` + connect-by-`p_url_address` (see #1096 comments) — a distinct,
rig-validated, receive-path change.
