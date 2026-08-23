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
