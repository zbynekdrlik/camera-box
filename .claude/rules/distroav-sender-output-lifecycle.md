---
paths:
  - "vendor/distroav/src/ndi-output.cpp"
  - "vendor/distroav/src/main-output.cpp"
  - "vendor/distroav/src/plugin-main.cpp"
  - "vendor/distroav/src/ndi-filter.cpp"
  - "vendor/distroav/src/ndi-sender-port.cpp"
  - "vendor/distroav/src/ndi-sender-port.h"
  - "tests/distroav_sender_port_linger_1363.rs"
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
- **libndi's sender port allocator is an in-process CURSOR, not a walk from 5961 per create**
  (measured on dev1, libndi 6.3.2): try the cursor, step up on a failed bind, cursor = bound+1 after
  success; `send_destroy` sets cursor = the freed port (last destroyed wins). A port the process
  never bound (a :5961 busy at the FIRST attempt) is NEVER retried — a sender created after :5961
  frees lands on the HIGHEST port (strace: it binds cursor directly, no 5961 attempt). So inside one
  OBS process only libndi's FIRST bind attempt can take :5961: deferring or re-creating the PGM
  sender later can never reclaim it. Any fix must either delay the first `send_create` or keep the
  TIME_WAIT from existing.
- A `SO_REUSEADDR` bind probe does NOT succeed over libndi's TIME_WAIT on Linux (the tw socket
  carries libndi's no-reuse flag; the kernel needs it on both). A plain bind on the loopback alias
  `127.0.0.2:5961` DOES discriminate: it succeeds over a TIME_WAIT on `127.0.0.1`/the LAN IP and
  fails over libndi's live `0.0.0.0:5961` listener.
- `SO_LINGER {1,0}` on the sender's CONNECTED :5961 sockets (found via `/proc/self/fd` +
  `getsockname`/`getpeername`) right before `send_destroy` closes them with RST → no TIME_WAIT → the
  next process gets :5961 again (measured: control 5962, linger 5961). A crash / kill still leaves a
  TIME_WAIT (kernel FIN-close).
- Probe sources (`alloc.c`, `pinprobe.c`, `linger.c`) are described on the ticket.

## IMPLEMENTED (issue 1363, option C): Linux linger-0 before every sender destroy — `ndi-sender-port.{h,cpp}`

Live on strih-lx only after the FULL strih bundle deploy + an OBS relaunch within 60 s of the
previous stop shows `STRIH-LX (2ME PGM)` on :5961 in avahi AND the OBS log carries the
`ndi-sender-port: TCP :5961 ('2ME PGM'): N connection(s) set to close with RST` INFO line from
the previous session's stop (proof the real OBS SIGTERM shutdown runs the stop/filter-destroy hooks
while libndi's sockets are still open — the dev1 repro used a libndi harness, not OBS).

- **Every** DistroAV sender create goes through `ndi_sender_create_tracked()` and **every** sender
  `send_destroy` is preceded by `ndi_sender_abort_connections_before_destroy(port, name)` — the
  program/preview `ndi_output` (stop, the begin_data_capture failure, the unadopted reservation)
  AND the per-source `ndi_filter` (destroy + the destroy-then-recreate on rename). All of it sits in
  `#ifdef __linux__` (the TU is added in CMake only on Linux), so Windows compiles byte-identical
  code. A NEW sender create/destroy site must get the same pair, or
  `tests/distroav_sender_port_linger_1363.rs` fails (it counts the real `send_create(`/
  `send_destroy(` sites; a `+`/`-` prefix marks the DEBUG log strings it skips).
- **libndi has no API for a sender's port**: `send_get_source_name(s)->p_url_address` is NULL for a
  local sender (measured). The port is the ONE new TCP LISTENING socket in `/proc/self/fd` across the
  `send_create` (`SO_ACCEPTCONN` + `getsockname`), creates serialized under one mutex (always the
  innermost lock). **Only libndi's SENDER port band `[5961, 6960)` counts**
  (`ndi_is_sender_band_port`, `NDI_RECEIVER_FIRST_TCP_PORT 6960`): the FIRST `send_create` of a
  process also opens libndi's `:5960` messaging listener (below the band), and — the part-3 trap —
  a libndi RECEIVER that pulls a REMOTE source opens its OWN listener on **:6960+** from a libndi
  thread while it connects. On strih-lx, OBS holds :6961..:6973, one per camera receiver. On dev1,
  4 receivers pulling a dev2 source listed :6961, :6962, a TRANSIENT :6960, then :6963. A local
  (127.0.0.1 / own-IP) source opens none, which is why a single-box repro never sees it. Those
  appear on OTHER threads, so the create mutex cannot keep them out of the diff. Before part 3,
  the MULTIVIEW/Grading creates on strih-lx were ambiguous on EVERY start. Two new band ports stay
  `NDI_LISTEN_PORT_AMBIGUOUS` (never a guess). An unidentified sender logs its OWN label
  `PORTID-1363 - ndi-sender-port: could not identify … (<reason>)`, and the reason comes from the
  pure `ndi_listen_port_failure_text`: unreadable / more than one / no new listener. That sender
  closes normally.
- **Log labels:** `WARN-1363` is RESERVED for the :5961 reserve line (TIME_WAIT / live listener
  on :5961 at post-load). `PORTID-1363` = a sender's port not identified at create.
  `LINGER-1363` = `/proc/self/fd` unreadable at stop. The gate test allows exactly ONE `"WARN-1363`
  log string, the one in ndi-output.cpp.
- The reserved instance carries its port into the output via `g_reserved_main_port` →
  `ndi_output_take_reserved_sender` → `ndi_output_take_adopted_port()` (all under
  `g_reserved_main_mutex`).
- The :5961 reserve probes BEFORE its `send_create` (after it, the reserved sender itself holds the
  port and the probe reads LIVE): plain bind `0.0.0.0:5961`, then `127.0.0.2:5961` →
  FREE / TIME_WAIT / LIVE_LISTENER / UNKNOWN (`ndi_first_port_hold_state`). One loud
  `WARN-1363 - ndi_output_reserve_main_sender: …` line when the pin is lost (landed elsewhere, or port
  unknown while :5961 was held). It NEVER waits — a wait was rejected (it delays OBS start exactly in
  the crash-recovery moment). A crash/kill still leaves the TIME_WAIT; that session's port map is then
  shifted and the dev1 port-map watchdog pages.
- Side effects are best-effort: every syscall failure is logged and skipped (shutdown must never
  crash OBS). Known accepted race: libndi owns the fds, so an fd closed+reused between the scan and
  abort (`setsockopt` + disconnect) could reset one unrelated socket instead of closing it with a FIN.
- **SO_LINGER 0 alone is TOO LATE — libndi shuts down before it closes (measured, part 3).**
  `strace -f` of a real libndi 6.3.2 stop: our `setsockopt(SO_LINGER {1,0})`, then libndi's
  `shutdown(fd, SHUT_RDWR)`, then `close(fd)` 0.16 ms later. The shutdown sends a GRACEFUL FIN
  whatever SO_LINGER says. When the viewer's FIN is back before the close (µs on loopback, sometimes
  on a LAN), the socket is already in TIME_WAIT. With linger only, 7 of 9 real-libndi stops left
  TIME_WAITs even with every port identified. So the abort ALSO resets each connection AT ONCE with
  `connect(fd, {AF_UNSPEC})`. That is the kernel's `tcp_disconnect`: RST now, socket to CLOSE, fd
  left open for libndi, whose shutdown/close then send nothing. After that, 6 of 6 runs were clean.
  A libndi thread that writes to the reset socket gets EPIPE, never a signal: the OBS frontend blocks
  SIGPIPE in every thread (`frontend/obs-main.cpp`). The INFO line keeps
  `N connection(s) set to close with RST` and adds `M reset at once (AF_UNSPEC disconnect)`.
- NOT covered (a TIME_WAIT can still form; harmful only on a relaunch within 60 s): a connection
  libndi closes by itself DURING the session (not at destroy). A socket already IN TIME_WAIT is past
  reach.
- Tier-0 verify recipe (no OBS build): the gate test's Facet C compiles the REAL
  `ndi-sender-port.cpp` with g++ against a stub `plugin-main.h` + a fake libndi (a real 0.0.0.0
  sender-band listener; its destroy does libndi's `shutdown(SHUT_RDWR)`, then the viewer closes on
  EOF, then `close()`). The control reproduces the TIME_WAIT and the abort frees the port. A "race"
  scenario opens an extra out-of-band listener during the create (still identified), and an
  "ambiguous" one opens two band listeners (PORTID-1363, port 0). Run it with plain
  `rustc --test` + `clippy-driver --test -D warnings`, no cargo. For the real libndi, compile
  the same `.cpp` against a stub `plugin-main.h` that includes `Processing.NDI.Lib.h` and dlopens
  `/usr/lib/ndi/libndi.so.6.3.2`, private group `t1363-private`, two processes back to back with a
  receiver connected (the lane's `obs_sim.cpp`): control PGM 5963 / OTHER 5964, fix 5961 / 5962; a
  same-process destroy+recreate keeps its port with the abort, walks up without it. The part-3
  shape (`obs_sim2.cpp`) reproduces strih-lx's back-to-back creates. It creates 5 senders about
  40 ms apart while a thread creates 12 receivers pulling a REMOTE private source on another box
  (dev2: copy a probe binary + `NDI_LIB=/usr/lib/ndi/libndi.so.6`, libndi 6.3.1 there). Each
  receiver opens a :6960+ listener, which is the race. Use a CAPTURING viewer (`recv_capture_v3` in a
  loop) on every sender port. With the non-capturing `ndiprobe recv` viewer, the connections came
  and went mid-session (fewer ESTABLISHED sockets than senders, and TIME_WAITs from before the
  stop), and that muddies the stop-time TIME_WAIT check. Then stop and relaunch within 1 s.
  Read TIME_WAITs by LOCAL port only: `ss -tanH state time-wait '( sport >= :5961 and sport <= :5969 )'`
  (the race receivers' client-side TIME_WAITs toward the remote :5961 are noise). Wait for that to
  drain (60 s) between runs. A `-fsyntax-only` g++ pass of the touched TUs
  against `vendor/obs-studio/libobs` + the frontend-api dir + `/usr/include/x86_64-linux-gnu/qt6`
  (plus a stub `obsconfig.h`) catches a real compile error before CI.

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
