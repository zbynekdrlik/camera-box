---
paths:
  - "vendor/distroav/src/ndi-source.cpp"
  - "tests/distroav_ndi_reconnect_767.rs"
  - "tests/distroav_recv_create_retry_1080.rs"
  - "tests/distroav_fresh_finder_connect_1096.rs"
  - "tests/distroav_by_url_identity_verify_1180.rs"
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

## #1180 — a sender restart can hand your cached NDI port to a SIBLING sender; a BY-URL connect is name-blind, so verify identity AFTER frames flow

**The port-reshuffle hazard (stock receivers included).** An OBS box that publishes SEVERAL NDI
outputs (strih: `2ME PGM`, `2ME PVW`, `Grading`, `MULTIVIEW`, interkom …) assigns their ports at
STARTUP in creation order — the aux `ndi_filter` republishes are created during scene-collection
load, BEFORE the main outputs start, so on a restart they can grab the lower ports and PUSH a main
output up. Live P0 (2026-08-23 Sunday service): strih's NIC failed, the operator rebooted, and
after the OBS restart the ports RESHUFFLED — `STRIH-SNV (2ME PGM)` moved `10.77.9.202:5964` →
`:5965`, and `STRIH-SNV (Grading)` (a full-screen SINGLE camera, cam3 at the time) INHERITED the
old `:5964`. **Every receiver that reconnected by CACHED URL latched onto the wrong sender** —
NDI connect-by-URL does not verify the sender's NAME, so whatever now listens on that port is what
you get. This hit both stock NDI Studio Monitor on the building TVs (unfixable by us — stock NewTek
code; the real protection there is the NIC fix + a stable sender set) AND our own #1096 BY-URL
connect. There is NO hidden "fallback to another source" logic anywhere — the reshuffle + a cached
URL is the entire mechanism. So: **a cached NDI port is NOT a stable identity across a sender
restart.** Any code (or any doc reasoning) that trusts a remembered URL/port to still be "the same
sender" after the sender bounced is wrong; identity lives in the NAME, resolved fresh.

**Why #1096's BY-URL connect NEEDS a post-connect identity check (and #1114's reattach does not).**
#1096 deliberately connects BY-URL to bypass the poisoned long-lived finder after a sender restart
— exactly the situation where the reshuffle also happens. Its fresh finder can even serve the DYING
sender's LAST advertisement (name→old URL) during the reshuffle window, so the resolved URL itself
can already be the wrong-sender port. And once frames flow, the #767 stale watchdog is SILENCE-based
(`no_connections>0` + no new frame) — it never fires while the WRONG sender delivers frames happily.
So a BY-URL bind has a wrong-source lock-in window that nothing else closes.

**The #1180 fix (LANDED, `ndi_source_thread`, `tests/distroav_by_url_identity_verify_1180.rs`).**
After a BY-URL bind (`connected_by_url_1180`, armed from `url_resolved_1096`) STARTS DELIVERING
FRAMES (`frames_seen_since_reset_1180`), re-run a bounded FRESH finder (`NDI_IDENTITY_VERIFY_MAX_WAITS`,
the same create→wait→read→`ndi_find_url_for_source_name`→destroy sequence #1096 uses, never under
`config_mutex`), resolve the configured name → URL, and feed the pure decision helper
`ndi_by_url_identity_mismatch(connected_url, resolved_url_for_name)`. It returns MISMATCH ONLY when
both URLs are known AND differ; NOT-a-BY-URL-bind and name-not-currently-discoverable both return
false (INCONCLUSIVE — never tear down a working feed on a can't-confirm). On a confirmed mismatch it
sets `force_by_name_next_reset_1180` and re-arms `reset_ndi_receiver`; the reset block CONSUMES that
flag (`force_by_name_1180`) to SKIP the fresh-finder BY-URL path and connect BY-NAME (loud
`#1180 connect BY-NAME` log), abandoning the wrong-sender URL — the same recovery reopening Studio
Monitor did live. The verify is a **ONE-SHOT, EVENT-DRIVEN per reconnect, NOT a steady-state poll**:
`identity_verify_pending_1180` fires once at the first frames of a BY-URL bind and is re-armed on
EVERY reset — and every BY-URL reconnect routes through a reset (the #767 stale rebind, the #1096
`no_connections==0` rebind, or a config change), which is exactly the reshuffle window. A first draft
added a 60 s periodic re-verify too, but review caught that it ran a BLOCKING fresh finder (~1 s)
inside the live frame-pull loop for essentially EVERY genlocked BY-URL input in steady state — a
fleet-wide ~1 s stall every 60 s that drops NDI recv-queue frames and bursts copies/gaps on the
tightly-gated zero-loss E2E path. Dropped: the per-reset one-shot IS the event-driven coverage
(re-verify on the disconnect/reconnect transition) with ZERO steady-state stall. If a wrong-sender
bind's one-shot is genuinely INCONCLUSIVE at first-frames (our configured name not advertising yet),
no action is correct then anyway — and the next disconnect/reset re-fires it. Scoped to
`genlock_source_is_active` (mirrors #767/#1096) — a BY-NAME bind never enters the verify path, so
its behaviour is byte-identical. `force_by_name_next_reset_1180` is also re-armed in the reset
block's recv/framesync create-FAILURE branches, so a forced BY-NAME reset that hits a transient
create failure stays BY-NAME on the #1080 retry instead of silently reverting to BY-URL. The pure
helper is the std-only lift-compile/truth-table gate; the impure sequence is source-anchored +
pwsh-mirrored in BOTH `windows-genlock*.yml`.

**Why BY-NAME (not re-BY-URL to the freshly-resolved URL) on the recovery.** Re-connecting BY-URL to
the just-resolved URL carries the SAME name-blindness risk (that URL can reshuffle/race again);
BY-NAME is the conservative, self-correcting choice (NDI keeps the receiver pointed at whatever
currently advertises the name, and its own internal rebind is available) and matches what the
operator's Studio-Monitor re-pick does. Rejected alternatives: dropping #1096 BY-URL entirely
re-opens the #1096 restart-wedge; verifying INSIDE the reset block (before connect) doesn't help —
the reshuffle races the reset, so a second finder sampled at the same instant resolves the same
stale advertisement. The verify MUST be post-connect, after the sender set has settled.

The live wrong-source cure is NOT offline-verifiable (vendored receive path compiles on CI only,
the reshuffle reproduces only live) — the offline gate proves the DECISION logic; the actual cure
is confirmed by the supervisor's post-deploy rig verification. NOT in scope for #1180: the NIC
hardware root cause (separate owned lane) and Studio Monitor on the TVs (stock NewTek code).

### #1181 — SENDER-side port-map stability: operator doctrine + a dev1 baseline watchdog (stock-receiver protection)

**Operator doctrine — adding/removing a dedicated NDI output mid-session reshuffles the NEXT
restart's port map, so it needs a controlled restart + a baseline re-capture.** Because libndi
assigns sender ports in CREATION ORDER (above), the moment you ADD or REMOVE ANY dedicated NDI
output (a `2ME`/main output, or a per-source `ndi_filter` republish like `Grading`) on strih/stream
DURING a running session, the saved-state creation order no longer matches the running order — so
the sender port map is deterministic on a CLEAN restart but will RESHUFFLE the first time OBS
restarts after your change. Stock NDI Studio Monitor on the building TVs (which we cannot patch)
reconnects by cached port and would then show the WRONG sender. So after ANY such add/remove:
schedule a CONTROLLED OBS restart OFF-PRODUCTION so every stock receiver re-pins to the new map,
confirm the TVs show the right sources, and RE-CAPTURE the checked-in baseline
(`scripts/ndi-portmap-audit.sh --capture`, committed in a PR). Never leave a live-added output to
reshuffle silently at the next unplanned restart (that is exactly the 2026-08-23 P0 sequence).

**dev1 baseline watchdog (the sender-side prevention layer, ships DISABLED).**
`scripts/ndi-portmap-alert-watchdog.sh` (5-min dev1 timer, reusing the shared
`scripts/lib/obs-watchdog-decision.sh` confirm/throttle like every issue-1001-family watchdog) runs
`scripts/ndi-portmap-audit.sh --check`, which reads the live mDNS map (`avahi-browse -rtp
_ndi._tcp`), isolates the STRIH-SNV OBS instance by the mDNS-hostname group of the anchor program
sender (`STRIH-SNV (2ME PGM)` — this EXCLUDES the separate Arena/CG-bridge Spout at the same IP,
whose port never participates in the OBS reshuffle), and diffs it against the checked-in
`scripts/ndi-portmap-baseline.json`. A CONFIRMED moved port fires ONE Slovak Discord alert naming
the affected senders + the operator action above (re-open the stock receivers; re-capture if the
change was intentional). The pure map-diff (`scripts/lib/ndi-portmap-health.sh`) is Tier-0-testable
offline (avahi `-p` `\DDD` DECIMAL escapes, hostname-group isolation, OK/MOVED/ABSENT/UNSET →
CHANGED-only-on-MOVED), fed a `NDI_PORTMAP_AVAHI_FIXTURE` in tests. An empty/anchor-absent live map
is a GATHER ERROR (exit 2), never a page — OBS-down/box-reachability is #1001's job. Ships DISABLED
by default per the watchdog fleet convention; enable on dev1 (same multi-step form as the netcfg
sibling — the repo `systemd/` dir is NOT in systemd's unit search path, so a bare `systemctl enable`
of the timer fails "Unit file does not exist"):

```bash
cp systemd/ndi-portmap-alert-watchdog.{timer,service} ~/.config/systemd/user/
systemctl --user daemon-reload && systemctl --user enable --now ndi-portmap-alert-watchdog.timer
```

Optional NDI_PORTMAP_* overrides (box IP, anchor, confirm/throttle) go in
`~/.config/camera-box/ndi-portmap-alert.env` (the `.service` loads it via an optional
`EnvironmentFile=-`). This protects the receivers #1180 could not — the stock TVs — by making any
sender-map change LOUD instead of a silent wrong-source-on-air.

**Investigation (pinning `2ME PGM` to the first port) — a filed follow-up, not done here.** See the
#1181 investigation comment: DistroAV defers `main_output_init()`/`preview_output_init()` to
`OBS_FRONTEND_EVENT_FINISHED_LOADING` (`plugin-main.cpp`, `Qt::QueuedConnection`), AFTER the
scene-collection `ndi_filter` republishes, which is WHY the program/preview outputs land on the HIGH
ports today. Creating the main output's send at `obs_module_post_load` to grab :5961 is feasible only
as a genuine vendored refactor (pre-create + reuse the send instance) and carries a real
early-idle-sender caveat; it is tracked as a standalone CI+rig-validated follow-up, never bundled
into this cheap-layer lane.
