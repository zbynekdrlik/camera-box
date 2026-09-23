---
paths:
  - "vendor/distroav/src/ndi-output.cpp"
  - "vendor/distroav/src/main-output.cpp"
  - "vendor/distroav/src/plugin-main.cpp"
---

# DistroAV SENDER-output lifecycle + NDI port ordering (#1185)

This is the SENDER side (the program `ndi_output` + preview + the module entry). The RECEIVER
side is `distroav-receiver-lifecycle.md` (ndi-source.cpp) — don't conflate them.

## NDI ports are assigned by CREATION ORDER, and the program starts LAST

libndi assigns each `NDIlib_send_create` a TCP port sequentially from **5961** in the order the
send instances are created. The trap: `plugin-main.cpp` `obs_module_load()` registers a frontend
callback that DEFERS `main_output_init()` + `preview_output_init()` to
`OBS_FRONTEND_EVENT_FINISHED_LOADING` via `QMetaObject::invokeMethod(..., Qt::QueuedConnection)` —
which fires AFTER the scene collection loads. So the per-source `ndi_filter` republishes
(Grading / MULTIVIEW / interkom, created during scene load) win :5962-:5964 and the program
(2ME PGM) lands on a HIGH port. A stock NDI Studio Monitor / building TV that reconnects by
**cached port** then gets the wrong sender for the program after any OBS restart (issue 1180/1181).
The deferral is DELIBERATE: `obs_output_start()` needs the OBS video pipeline ready, so you cannot
simply move the whole main-output start earlier.

Module-load order (OBS): `obs_module_load()` for all modules → `obs_module_post_load()` for all
modules → scene-collection load (creates `ndi_filter` senders) → `FINISHED_LOADING`. So
`obs_module_post_load` is the one hook that runs BEFORE any scene-load sender exists.

## #1185: pin PGM to :5961 by reserve-at-post-load + adopt-in-start

`obs_module_post_load` (plugin-main.cpp) calls `ndi_output_reserve_main_sender(name, groups)`
(defined in ndi-output.cpp) — an early `NDIlib_send_create` that grabs :5961 before scene load —
gated on `config->OutputEnabled && !OutputName.isEmpty()` (never advertise a disabled PGM).
`ndi_output_start` then calls `ndi_output_take_reserved_sender(name, groups)`, which transfers
ownership of the reserved instance on an EXACT name+groups match, else returns nullptr → a fresh
`send_create` as stock. Preview (`PreviewOutputName`), the random-named `main_output_is_supported`
test output (groups `"DistroAV Config"`), and a renamed PGM all fail the match → never adopt.
Cleanup: `ndi_output_release_reserved_main_sender()` destroys an unadopted reservation — called
from `obs_module_unload` BEFORE `ndiLib->destroy()`, and from `main_output_init`'s else branch when
the output will not be created (disabled / unsupported format / empty name). Reservation is a
ONE-SHOT at initial load; a later profile-change `main_output_init` finds nothing reserved and
creates fresh (acceptable — the pin only has to hold across the initial load↔restart cycle). Only
PGM is pinned; PVW + filters still reshuffle (mitigated by the issue-1181 dev1 port-map watchdog).

## Linux: a TIME_WAIT on :5961 from the previous OBS walks the pin to :5962 (issue 1363)

On strih-lx the reserve AND the adopt both ran (`reserved the first NDI port …` / `adopted the
port-reserved main NDI sender …` in the OBS log) and the program still landed on :5962, with nothing
listening on TCP :5961. The cause is in libndi, not in the reserve/adopt path:

- libndi binds each sender listener **without `SO_REUSEADDR`**. It sets `SO_REUSEADDR`+`SO_REUSEPORT`
  only on the :5960 messaging socket. `strace` of a sender process started right after another
  sender with a connected receiver had exited: `bind(…5961) = -1 EADDRINUSE`, then `bind(…5962) = 0`.
- On **Linux** a connection left in TIME_WAIT on local port :5961 (60 s, fixed) blocks a plain
  `bind()`. So an OBS started within 60 s of the previous instance closing its PGM sender can lose
  :5961, and every sender shifts up by one. On **Windows** a plain `bind()` succeeds over TIME_WAIT
  connections, which is why the pin held on the Windows strih.
- Whether a TIME_WAIT is left depends on which side of each :5961 connection closed first at
  shutdown (timing-dependent). strih-lx history 23.9.2026: gaps of 35 s and 45 s kept :5961, 36.8 s
  and 59.0 s shifted to :5962. A shift only ever happens inside the 60 s window.
- libndi 6.3.2 has no config key for a sender port (the full `ndi.*` key list: groups, networks,
  rudp/tcp/unicast/multicast send+recv enable, codec, log, machinename, vendor, sourcefilter). A
  second NDI process on the box (bkshading-service, receiver-only: UDP 5960 bank + TCP/UDP 6981) does
  NOT shift OBS senders; a connected receiver process before a sender process gave :5961 five times
  out of five on dev1.
- Reproduce offline on dev1 (`/usr/lib/ndi/libndi.so.6.3.2`): a small C program that dlopens
  `NDIlib_v6_load`, creates a sender in a private group (e.g. `t1363-private`, so nothing appears on
  public viewers) with a receiver connected to it via its URL, exits, then starts a new sender process
  → `ss -lntp` shows the new one on :5962 while `ss -tan` shows `TIME-WAIT …:5961`.
- Read the live port from the RECEIVER side when the strih log has no URL line: the stream OBS log's
  `reset_ndi_receiver … BY-URL '10.77.9.202:59xx'` for `NDI 2ME PGM` gives the program's port per
  session.
- The fix shape (a bounded wait before the reserve vs a late re-pin vs documenting) is a design
  decision recorded on issue 1363 — check the ticket before changing the reserve path.

## Trap: a start-path bail after the sender exists LEAKS it — and post-#1185 the leak is the pin-holder

`ndi_output_stop` only `send_destroy`s the sender when `o->started` is true, and
`ndi_output_destroy` never frees `o->ndi_sender`. So if `obs_output_begin_data_capture` FAILS after
the sender was created/adopted, `o->started` stays false and the sender leaks for the whole session
— and post-#1185 that leaked instance is the :5961 port-holder advertising the live PGM name
FRAMELESS (the exact wrong-source symptom this fights), while the next start makes a second
same-named sender on a high port. Always destroy the sender in the `begin_data_capture`-failed
branch (`ndiLib->send_destroy(o->ndi_sender); o->ndi_sender = nullptr;` — safe, capture never began).

## Lock-step anchors (vendored change — CI is the first compile)

A change here needs a `distroav_timecode_patch.rs`-style Rust source-guard test AND matching pwsh
gates in BOTH `windows-genlock.yml` and `windows-genlock-fast.yml` (both ship distroav.dll).
Anchor on CALL-SITE-unique tokens, never a bare function name — a name that also appears in an
extern DECLARATION passes the gate on the decl alone even if a subtree-pull drops the call hunk
(the issue-832 anchor class): e.g. `ndi_output_reserve_main_sender(QT_TO_UTF8(config->OutputName)`
and `ndi_output_release_reserved_main_sender(); if (ndiLib) {`, verified count==1 in the squished
source. Verify each pwsh token OFFLINE against the real `re.sub(r'\s+',' ',text)`-squished file
(pwsh is not on dev1), and lift-compile any new `static`/format-string helper under
`g++ -Wformat=2 -Wconversion` before pushing.

## UNVERIFIED without the live rig

The reserved instance advertises the PGM name FRAMELESS for the ~seconds of OBS load. Whether a
stock NDI Studio Monitor / building TV tolerates a frameless source (drops it? blacklists it?) is
UNVERIFIED and must be checked on the live rig at integration — a worktree/code worker cannot prove
it. Keep it in the evidence block as UNVERIFIED, not as a done claim.
