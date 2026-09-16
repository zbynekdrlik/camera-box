---
paths:
  - "vendor/distroav/src/ndi-source.cpp"
  - "tests/distroav_ndi_reconnect_767.rs"
  - "tests/distroav_recv_create_retry_1080.rs"
  - "tests/distroav_fresh_finder_connect_1096.rs"
  - "tests/distroav_by_url_identity_verify_1180.rs"
  - "tests/distroav_frameless_by_url_escape_1287.rs"
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

**The C++ verify above is the IDENTITY (wrong-source) half ONLY — the LIVENESS half lives in the
python receiver-policy layer.** This name→URL check catches "wrong camera, frames flowing"; it does
NOT prove frames are flowing at all. The 2026-08-27 strih NIC-swap aftermath showed the other
failure: a receiver holding a FROZEN frame with the CORRECT name (a `break`-wedged thread, above) —
`recv-timing #797` never listed it, but `ndi_source_name` was right, so every name-only verify
(`--heal`, `reenforce_ndi_name` read-back) reported a false success and only an OBS restart cured
it. That LIVENESS term — a WS screenshot-diff (`obs_phase2.sample_receiver_liveness` /
`classify_receiver_liveness`, exposed as `set-ndi-mapping.py --verify-live`, exit 1 on FROZEN →
escalate to an OBS restart) — is documented in `.claude/rules/ndi-name-recovery.md`'s "#1180
LIVENESS term" section, not here (it is a receiver-policy/WS concern, not a vendored-receiver one).

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

## #1224 — the PROPERTIES path is the ONLY route to `obs.dll!new_prop`, and its async finder callback is a detached-thread lifetime hazard

A c0000005 dump whose distroav frames name `ndi_source_update` / `new_ndi_receiver_name` reaching
`obs.dll!new_prop` is **offset-symbolized** ("nearest export"): `ndi_source_update` NEVER builds
`obs_properties` (it only reads/writes settings + drives the receiver thread), so it has NO path to
`new_prop`. In distroav `new_prop` (`vendor/obs-studio/libobs/obs-properties.c`, reached only by
`obs_properties_add_*`) is reachable ONLY from **`ndi_source_getproperties`**. So when a `new_prop`
crash points "into distroav", the true function is `ndi_source_getproperties`, full stop — don't
chase the named frames.

Two facts that kill the naive NULL-guard theory (verify before claiming a props-NULL fix "closes"
a `new_prop` crash):
- **`obs_properties_create()` NEVER returns NULL on stock libobs** — `bmalloc` `bcrash()`es on OOM
  (`libobs/util/bmem.c`), it does not return NULL. So a `if (!props) return` guard in
  `ndi_source_getproperties` is DEAD belt-and-braces (harmless, but do not credit it with the fix).
- **Even a hypothetical NULL `props` can't reach `new_prop`** — every `obs_properties_add_*`
  early-returns on `!props` (and `has_prop`) BEFORE calling `new_prop`. For `new_prop+0xa2`
  (`HASH_ADD_STR(props->properties, …)`) to AV, `props` must be non-NULL GARBAGE = a UAF/corruption
  no NULL guard catches. `obs_property_list_add_string/item_count/item_string` are all NULL-`p`-safe
  too (via `get_list_data`'s `!p` check), so a NULL `source_list` degrades silently on the sync path.

The real reachable NULL sub-class (and the guard-at-consumer that closes it): `ndi_source_getproperties`'s
finder callback runs on a **DETACHED thread** (`ndi-finder.cpp`: `std::thread(refreshNDISourceList,
callback).detach()`), firing 5+ s later (5 s throttle + a `find_wait_for_sources(…,1000)` loop) —
after `ndi_source_getproperties` returned. The lambda captures raw `source_list`/`s` and calls
`obs_source_update_properties(s->obs_source)`; libobs also calls `get_properties(data=NULL)` for
type-level builds, so **`s == NULL` is a real live case** → a NULL deref on the finder thread (in
distroav.dll — matching the dump). Guard `if (!source_list || !s || !s->obs_source) return;` at the
TOP of the lambda (before any deref) — that is the crash-closing guard. **RESIDUAL not fixable here:**
a freed-but-non-NULL `s`/`source_list` UAF (captured-by-value raw pointer, can't become NULL) sails
through — un-catchable without lifetime tracking (weak-ref/refcount = a redesign). Do NOT add a
`config_mutex` lock around the properties build: `ndi_source_update`'s `#93` comment warns of a
`config_mutex`-vs-`pthread_join` deadlock, and `ndi_source_getproperties` runs on the UI thread.
Test = std-only Rust source-anchor ONLY (pure crash/NULL-safety guard → no windows-genlock*.yml pwsh
mirror, per `vendored-obs-frontend-crash-safety.md`); the two `genlock_ensure_saved_source_listed(source_list, s)`
CALL sites (lines ~650/657) ARE pwsh-anchored in both ymls, so keep the call text byte-identical
(put guards in the function body / lambda head, never on the call line). See #1224.

## A FRAME-LESS BY-URL bind is its own wedge class — #1180's verify never fires without frames (2026-09-03, .617 PR E2E attempt 1)

The #1096 BY-URL path has a second failure shape besides #1180's wrong-sender: **a BY-URL connect to
a DEAD port.** Live: after the issue-1202 parity-align restarted every cambox sender, strih's
`'NDI cam7'` sat 6.5 min at `received=` Δ0 / `depth=0` / `underruns +151 per tick` while the #1096
`no_connections==0` arm fired every ~10 s and EVERY one of ~40 rebinds resolved the SAME
`connect BY-URL '10.77.9.67:5962'` (the "fresh" per-reset finder kept serving the dying sender's
cached advertisement); `'NDI cam3'` in the same wave got `fresh finder resolved no URL` → BY-NAME →
recovered in seconds. The sender itself streamed 60 fps the whole time (cam journal) — the leg was
dead purely receiver-side, and it came back only when the cleanup's SECOND restart of cam7 happened
to bind the usb sender on :5962 again. Sender ports are per-restart racy (fleet snapshot: `CAMn (usb)`
on :5961 for cam1/2/5 vs :5962 for cam3/4/6/7, one hole per box), so a cached URL going dead after a
restart is an ordinary event, not a freak.

Why nothing self-heals: #1180's `force_by_name_next_reset_1180` is armed ONLY from the post-connect
identity verify, gated on `frames_seen_since_reset_1180` — a bind that delivers ZERO frames never
reaches it, and the #767 stale arm is silence-on-a-CONNECTED receiver, so `no_connections==0` just
loops BY-URL→dead port forever. Reading the log: `connect BY-URL` repeating with the SAME URL every
stale window + `received=` flat + no `recv-timing #797` line for that input = this class (NOT the
`break` death above — the thread is alive and rebinding — and NOT #1096's poisoned-name wedge, which
BY-URL is the cure for). Reading the log: `connect BY-URL` repeating with the SAME URL every stale
window + `received=` flat + no `recv-timing #797` line for that input = this class.

**The fix LANDED (#1287, `ndi_source_thread`, `tests/distroav_frameless_by_url_escape_1287.rs`).** A
pure decision helper `ndi_force_by_name_after_frameless(connected_by_url, frames_seen_since_reset)`
returns true iff the current bind was BY-URL AND delivered ZERO frames. BOTH reset-forcing arms —
the `no_connections==0` arm and the #767 stale-while-connected arm — call it and, when true, set the
SAME `force_by_name_next_reset_1180` flag #1180 owns, so the NEXT reset connects BY-NAME (loud log
`genlock: #1287 frame-less BY-URL bind (dead sender port?) -- forcing BY-NAME on the next rebind`).
Because the un-forced default reset is BY-URL, forcing BY-NAME only after a frame-less BY-URL bind
ALTERNATES BY-URL↔BY-NAME across consecutive frame-less rebinds, so neither this stale-URL wedge nor
the #1096 poisoned-name wedge can pin a leg; a bind that DID deliver frames is untouched (#1180's
identity path owns the wrong-sender-with-frames case). No new state, no counter. The pure helper is
the std-only lift-compile/truth-table gate; the impure wiring is source-anchored (the WHOLE
`if (helper) { force_by_name_next_reset_1180 = true;` adjacency per arm, so deleting only the set is
caught) + pwsh-mirrored in BOTH `windows-genlock*.yml`. The live receive-path cure is NOT
offline-verifiable — confirmed only by the supervisor's post-deploy rig repro (a graceful
`systemctl restart camera-box` on a cambox whose usb sender lands on the other port; expect strih
`#1287 ... forcing BY-NAME` → `#1180 connect BY-NAME` → `received=` Δ>0 within ~2 stale windows).
The `[1/8]` frozen-camera gate (pixel-hash, 2 samples) was RIGHT to fail on the live incident —
cross-check with the `received=` Δ before calling any FROZEN a false positive.

## A FINDER-BLIND sender needs a fallback ladder — by-name/BY-URL both die when discovery itself is blind (#1096 reopen)

**Distinct from every wedge above: this one is not the receiver's fault.** The #1080 `break` death, the
#1096 poisoned-name wedge, and the #1287 dead-port wedge all assume the local SDK finder will EVENTUALLY
re-discover the sender's mDNS record — the fresh per-reset finder, the #767 watchdog, and the #1287
BY-URL↔BY-NAME alternation are all built on that. When strih's finder stays BLIND (the #1199 flaky-NIC /
multicast-reception class), NONE of them can reach the sender: the fresh finder resolves nothing → the
`#1096 connect BY-NAME (fresh finder resolved no URL)` path re-consults the poisoned per-process finder →
black. Live 15.9.2026: strih `NDI cam7` ran **607 identical BY-NAME cycles over 12 min** at `received=` Δ0
while the sender emitted 60.0 fps and imag (finder healthy) recovered BY-URL in 6 s.

**The fix LANDED (#1096 reopen, `ndi_source_thread`, `tests/distroav_by_url_fleet_fallback_1096.rs`),
vendored receiver side only.** When a reset's fresh finder resolves nothing and BY-NAME is not
force-required, escalate through a BOUNDED BY-URL ladder before falling to by-name:
- **(b) last-known-good:** `last_delivered_url_1096` persists the URL that actually DELIVERED frames on a
  BY-URL bind; the ladder retries it BY-URL first (a graceful restart usually returns on the same :5961).
- **(a) fleet map:** after `NDI_FLEET_AFTER_NO_URL_CYCLES`=3 consecutive finder-blind resets, the pure
  `ndi_fleet_url_for_name(name, port_index, buf, buflen)` synthesizes the address from the naming
  contract (`CAMn (usb)` ↔ `10.77.9.6n:5961`, cf. `scripts/camera-set.sh`) — returns FALSE for any
  non-camera name so `cg`/`NDI obs hudba` are NEVER given a guessed URL — cycling ports 5961..5963
  (`NDI_FLEET_PORT_CANDIDATES`) one-per-reset across consecutive frame-less fleet binds (bounded, never an
  in-reset socket loop — deliberately NO raw sockets, which would drag winsock2 into a CI-first-compile
  Windows build). The BY-URL fallback choice is the pure `ndi_fallback_bind_mode_1096(...)` ladder.

**Key invariant — both fallbacks route through the SAME `connected_by_url_1180 = url_resolved_1096`
arming** (a separate `url_bind_kind_1096` tags fresh/last-known/fleet for the log line ONLY), so #1180
identity verify (a wrong-sender fleet/last-known guess WITH frames is caught + forced by-name) and #1287
frame-less alternation (a dead-port guess is caught + forced by-name NEXT) apply UNCHANGED. The three
coexist as a by-name → last-known → fleet ladder that no single dead path can pin: e.g. finder-blind +
rotated port → last-known(:5961 dead) → BY-NAME → … → (K reached) fleet(:5961) → BY-NAME → fleet(:5962
LIVE); first frames record the delivering URL as the new last-known and zero the escalation clock. New
log markers use `#1096 rebind BY-URL` (mutually non-substring vs the existing `#1096 connect BY-URL`/
`BY-NAME` lines other tests anchor on). The two pure helpers are the std-only lift-compile/truth-table
gate; CI is the first real compiler. The live cure reproduces only live — UNVERIFIED until the
supervisor's post-deploy rig repro (bounce a cambox sender against strih; expect `#1096 rebind BY-URL …
(last-known good` / `(fleet map …` → `received=` Δ>0 without an OBS restart). Candidate (c), the dev1
frozen-input watchdog extension to strih camera inputs, is a SEPARATE lane (`scripts/frozen-input-*`).

## #1320 — `ndi_source_update` runs on the GRAPHICS thread, so a blocking teardown in its stop-path freezes the PROGRAM render

`ndi_source` is `OBS_SOURCE_ASYNC_VIDEO` (⇒ `OBS_SOURCE_VIDEO`), so `obs_source_update()`
(`vendor/obs-studio/libobs/obs-source.c`) does NOT run `info.update` inline — it **defers** it
(`os_atomic_inc_long(&source->defer_update_count)`), and the deferred `ndi_source_update` runs from
`obs_source_video_tick` → `obs_source_deferred_update`, which `obs-video.c`'s
`obs_graphics_thread_loop` calls **ON THE OBS GRAPHICS/RENDER THREAD**. So anything `ndi_source_update`
does synchronously blocks the PROGRAM render, not just the caller's WS/main thread.

The live incident (issue 1320, strih 15.9.2026 — 7 severe freezes in one afternoon, read-only logs):
a CLEAR-then-SET reattach (a heal/`set-ndi-mapping`-class script, see the #1114 note above) clears the
NDI source name to `""` → `ndi_source_update` → `ndi_source_thread_stop` → `pthread_join`, and the
av-thread's EXIT-path `NDIlib_recv_destroy()` blocks **~7.5 s** (an SDK-internal teardown timeout). The
graphics thread sits in that join the whole time → PROGRAM render freeze (`program-render-audit
lagged=228 avg_frame_ms=782`, the ONLY `lagged>0` window in a 95 min session) → the `2ME PGM` NDI
output starves → the stream receive FIFO underruns → a 462-relock storm → the presented video sits
+2/+3 frames late for ~40 min. EVERY freeze has the identical signature: `ndi_source_update: No NDI
Source selected; Requesting Source Thread Stop` → exit `recv_destroy` → **~7.5 s** → `Reset NDI
Receiver`, and the freeze window's timestamp == the recv_destroy completion. A scene switch is NOT the
cause: 60 scene switches in the same session (incl. a 8-switch rapid-fire storm ~1.5 s apart) produced
exactly ONE `lagged>0` window — the one coincident with the slow reattach.

**The fix (`ndi_reap_receiver_detached` + the pure `ndi_reap_should_defer` gate):** the av-thread's
EXIT-path framesync+receiver teardown is handed to a DETACHED reaper thread, so the blocking
`recv_destroy` never holds up the `pthread_join` — the join, and the render thread, return in ~ms. The
NDI handles are plain instances independent of `ndi_source_t`, so the reaper races nothing in `s` or a
freshly-started av-thread; a `std::thread` spawn failure falls back to a synchronous destroy (never
crash). All existing exit-path diagnostic log lines are kept **byte-identical** (they only PRINT now;
the destroy is deferred — F2 of the review, deliberate) and a new mutually-non-substring `genlock-reap:`
marker records each handoff. Anchors mirrored into BOTH `windows-genlock*.yml` (the fast path hot-swaps
`distroav.dll` un-gated). Std-only gate + truth table: `tests/distroav_scene_switch_reinit_1320.rs`.

**Scoped to the EXIT path only.** The in-loop `reset_ndi_receiver` block's `recv_destroy` (a warm-
receiver recreate) is the SAME defect class but runs on the av-thread and only contributes to the join
latency in the rare case the stop lands mid-reset (the 17:04 partial, `lagged=61`); it sits in the
issue-1080/1096 minefield with its own test anchors, so async-reaping it is a bounded FOLLOW-UP, not
folded here. Accepted shutdown-race (review F1): a detached reaper can outlive `ndiLib->destroy()` on
OBS process shutdown (a rare crash-on-exit) — accepted vs the live freeze; a clean outstanding-reaper
drain before `ndiLib->destroy()` is the follow-up. Detection: the report-only `program_render_lagged`
bundle-state facet (see `program-render-audit.md`); a dev1 watchdog paging on it + `relock_bursts>=1`
is the supervisor's follow-up. The live cure (20 scene switches, no `lagged>0`, dock ±15 ms ≥2 h) is
UNVERIFIED until the supervisor's full-bundle deploy + rig soak.

## #1096 (reopen 16.9.2026) — a CONNECTED bind that never delivers a frame has no aging clock; age it from bind time

A DIFFERENT shape of the wedge, seen right after the 12:10 fleet deploy of 1.7.0-dev.631: strih
reset `NDI cam5`/`cam2`/`cam6` BY-NAME into the poisoned finder, the bind CONNECTED
(`no_connections > 0`) but delivered ZERO frames — `genlock-fifo audit received=` frozen for over an
hour, cam1/3/4/7 fine. None of the existing recovery paths covered it:

- `genlock_reconnect_decision` (the issue-767 stale watchdog) guards `if (last_frame_ns == 0) return
  false;` (never judge a warming-up receiver). A bind that connects but never delivers leaves
  `s->last_frame_timestamp == 0`, so this decision can NEVER fire for it.
- the last-known/fleet-map ladder is gated inside `if (no_conn == 0)` — it cannot re-arm once the
  receiver reports `no_connections > 0`.
- the issue-1287 BY-URL↔BY-NAME alternation runs only *inside* a reset-forcing arm, which never
  fires here.

So the receiver had no clock that ages a connected bind which never delivered — cured only by an
external re-create (the 13:15 evidence: a same-value `set-ndi-mapping.py --heal` reported `7
already-correct` yet drove `obs_source_update → ndi_source_update → the issue-1320 deferred re-init`,
which recreated the receiver silently and the three inputs resumed in ~30 s with NO `reset_ndi_receiver`
line — proving the cure is a receiver re-create the receiver just never applies to itself).

**The fix (LANDED, `ndi_source_thread`, `tests/distroav_frameless_connected_1096.rs`):** record
`recv_bind_ns_1096 = os_gettime_ns()` at every successful `recv_create_v3`, and add the pure sibling
`genlock_frameless_bind_reconnect_decision(genlock_active, no_connections, now_ns, last_frame_ns,
bind_ns, frameless_stale_ns)` — a STRICT COMPLEMENT of `genlock_reconnect_decision`: it fires ONLY
when `last_frame_ns == 0` (the exact case the 767 helper skips), `no_connections > 0`, genlock-active,
and `now - bind_ns >= FRAMELESS_BIND_STALE_NS` (5 s — far above a healthy sub-second first-frame
warm-up, well inside the 60 s acceptance bound; deliberately shorter than the 10 s #767 window, which
covers "was delivering, now silent"). A third watchdog arm sits immediately AFTER
`genlock_reconnect_decision` (so the healthy stale path always wins) and, on a fire, forces the SAME
reset ladder (fresh finder → last-known → fleet map) + the issue-1287 alternation (`frames_seen` is
false), logging `genlock: NDI receiver connected but FRAMELESS for N s since bind -- forcing rebind
'<input>'` (mutually non-substring vs every existing `genlock:` marker).

**Why it doesn't disturb the healthy path:** the #767 reconnect-epoch refresh (`if (was_disconnected)
{ s->last_frame_timestamp = os_gettime_ns(); … }`) sets `last_frame_timestamp` non-zero on a normal
disconnect→reconnect, so this new arm returns false there and `genlock_reconnect_decision` owns it —
byte-identical behaviour. The new arm engages ONLY when `last_frame_ns` is still 0, i.e. the bind has
literally never delivered a frame, which is always a real wedge. `recv_bind_ns_1096` is a plain
thread-local `uint64_t`, no teardown.

The force-by-name gate-and-set adjacency now appears at THREE reset-forcing arms (the
`no_connections==0` arm, the #767 stale-while-connected arm, and this connected-but-frameless arm);
both `windows-genlock*.yml` bumped that Count anchor `-lt 2` → `-lt 3` and added the helper/call-site/
bind-record anchors. The dev1 `frozen-strih-input-alert-watchdog` cure hint now says: WS settings
touch (`set-ndi-mapping.py --host <ip> --heal`) FIRST, OBS relaunch second.

The pure helper is the std-only lift-compile/truth-table gate (a `>= → >` scratch mutation goes RED
on the 5 s boundary vector); the live receive-path cure reproduces only live — the acceptance is a
fleet deploy (7 senders restarting within 45 s) after which every strih camera input's `received=`
advances within 60 s with no WS heal and no OBS relaunch (the supervisor's post-deploy repro).
