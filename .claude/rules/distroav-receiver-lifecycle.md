---
paths:
  - "vendor/distroav/src/ndi-source.cpp"
  - "tests/distroav_ndi_reconnect_767.rs"
  - "tests/distroav_recv_create_retry_1080.rs"
  - "tests/distroav_fresh_finder_connect_1096.rs"
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

## Do NOT conflate the `break` silent-death with the #1096 wedge — and the #1096 fix (LANDED)

The live strih wedge (#1096) is a DIFFERENT failure: `recv_create_v3` SUCCEEDS (non-null) but the
new receiver, created connect-BY-NAME, never re-resolves a RESTARTED sender (rotated port) because
the long-lived in-process NDI finder state is poisoned — cured only by an OBS restart. #1080's
retry (which fires only on a NULL create) does not enter there and does not cure it.

**The #1096 fix is now IMPLEMENTED in the reset block** (`ndi_source_thread`,
`tests/distroav_fresh_finder_connect_1096.rs`): before `recv_create_v3`, resolve the source through
a FRESH `NDIlib_find` per reset (`find_create_v2` → bounded `find_wait_for_sources` +
`find_get_current_sources` → the pure `ndi_find_url_for_source_name` picker → copy the live
`p_url_address` into `owned_source_url` → `find_destroy`) and connect BY-ADDRESS
(`source_to_connect_to.p_ndi_name = ""`, `p_url_address = owned_source_url`), bypassing the poisoned
long-lived finder (the SDK contract: an EMPTY `p_ndi_name` makes it use `p_url_address` directly).
Fallback when the fresh finder resolves nothing: keep the name-based connect (no worse than
upstream). The pure picker is the std-only lift-compile/truth-table gate; the impure sequence is
source-anchored + pwsh-mirrored in both `windows-genlock*.yml`.

**CRITICAL — the recovery has TWO triggers, split by `recv_get_no_connections()`, and BOTH are
needed** (a fix that only armed one would miss half the sender-restart shapes):
- **`no_connections > 0` (half-open, e.g. a hard sender reboot with no graceful TCP close):** the
  #767 stale watchdog fires (genlocked + connected + silent past `GENLOCK_RECONNECT_STALE_NS`) and
  arms `reset_ndi_receiver` — the reset then runs the fresh finder. #767 is the trigger here.
- **`no_connections == 0` (a GRACEFUL `systemctl restart camera-box` sends a clean FIN, dropping
  the receiver to 0):** #767 explicitly returns false for `no_connections <= 0`, and a by-URL
  receiver has no name for NDI's own internal rebind while a name-based one re-consults the poisoned
  finder — so this case has NO recovery via #767. #1096 therefore ALSO arms a fresh-finder reset
  from the `no_connections == 0` steady path: a dedicated `no_conn_since_ns` timer, genlocked-scope
  (mirroring #767), re-armed at most once per `GENLOCK_RECONNECT_STALE_NS` window (natural backoff
  while the sender is genuinely down), cleared on reconnect. If you touch the `no_connections == 0`
  branch, preserve this arm — deleting it silently reopens the wedge for the graceful-restart case,
  which is the ticket's own primary scenario.

The live cure is NOT offline-verifiable (vendored receive path compiles only on CI, the wedge
reproduces only live) — the offline gate proves the DECISION logic; the actual receive-path cure is
confirmed only by a post-deploy rig wedge repro (the supervisor's, after the full-bundle deploy).

## #1114 — re-applying the SAME `ndi_source_name` over WS is a receiver NO-OP; force a fresh receiver with CLEAR-then-SET

`ndi_source_update()` derives `reset_ndi_receiver` from a NAME CHANGE (`safe_strcmp != 0`), so a
`SetInputSettings ndi_source_name` re-apply of the unchanged name never touches the receiver thread —
a "reattach" built that way is a silent no-op while the issue-1096 retry-in-place thread sits on a
dead pre-bounce sender (the E2E [2/8] ~52s-budget false "camera leg dead", plus the stretched-preflight
FATAL issue-359 painter-freshness casualty when retries "save" the run). The targeted per-input
equivalent of an OBS force-kill: CLEAR the name to `""` (→ `ndi_source_thread_stop`,
behaviour-independent) then SET it back (→ `ndi_source_thread_start` with `reset_ndi_receiver=true`
→ fresh issue-1096 finder resolves the live sender). Implemented in `scripts/strih_mv_scenes.py
reattach()` with a discoverability re-check before the set-back (a vanished source leaves `""` and
returns NOT_DISCOVERABLE instead of re-pinning a dead name into the issue-795 mangle window).
