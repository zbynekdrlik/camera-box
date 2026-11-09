---
paths:
  - "scripts/bkshading-*"
  - "scripts/lib/bkshading-*"
  - "scripts/lib/ci-run-resolve.sh"
  - "systemd/bkshading-*"
---

# bkshading — remote camera shading control (issue 808)

The code, its tests and the rules about them live in `zbynekdrlik/shading` since issue 1487; here is the
deploy, provisioning and rig side. The release camera-box runs: `.claude/rules/shading-pinned-release.md`.

A multiplatform Rust **service** (`bkshading`, runs on the strih PC — Windows first, Linux later)
that aggregates per-camera **relays** into ONE operator web panel; and a cambox/SBC **relay**
(`bkshading-relay`) that drives a Blackmagic camera over USB-PTP via the `gphoto2` CLI. Shared
crate `bkshading-proto` holds the wire types + the byte-verified PTP mapping (ported 1:1 from the
dev2 MVP `pybridge/mapping.py`). Owner architecture: issue 808 comments 5355836067 / 5356048130 /
5356062847 (2026-08-20).

## Non-negotiables (owner decisions)
- **Transports are USB-relay / SBC-relay / ethernet-REST ONLY — NEVER Bluetooth.** The BLE path
  from the dev2 MVP is dead; do not reintroduce it.
- Camera list is config-driven (`shading:bkshading/service/bkshading.example.toml`): a camera is a record
  (id, transport, address, optional `ndi_preview`). **A camera with no `ndi_preview` renders a
  params-only block** (no preview). M2 delivers the LIVE preview (JPEG over HTTP; see M2 section).
- **The panel lists the box's OWN venue cameras (`STRIH_CAMERAS`; owner 26.10.2026: "ale v pp urcite
  nie je 7 kamier!!!" -- strih-snv 2 3 5, strih-pp 1 4 6 7; all seven only at a joint event, issue
  1431), one block per cambox (owner ruling 5977504457).** A handheld SBC is
  never its own block: it drives the block of the cambox its video reaches (`sbc-relay` + its
  address; the block keeps `CAMN (usb)` + `grab_fps`). The strih seed `strih_bkshading_config_text`
  renders it from the box facts `STRIH_CAMERAS` + `STRIH_BKSHADING_RELAYS` (`none` |
  `N=<transport>@<host>:<port>` overrides of the default `cambox-relay` at `camN.lan:8771`, N one of
  the box's cameras; strih-pp lists its four by IP, `camN.lan` does not resolve there). The seed is
  written only when the file is absent, so a live box is brought to it by hand (strih-pp 26.10.2026). Tests: `tests/python/test_bkshading_relays_fact_808.py`.
- **Issue 1431 (joint event: `strih.lan` is the ONE switch): every cambox relay registers itself, and
  both strih boxes declare all seven cameras.** The cambox relay.env carries
  `BKSHADING_REGISTER_URL=http://strih.lan:8770/api/relays/register` + `BKSHADING_RELAY_ID=camN`
  (`bkshading_relay_env_file_content FPS RELAY_ID`; the unit is UNCHANGED because the SBC reuses it).
  The service's camera list is static and its preview workers start from it at boot, so a registration
  alone cannot add a camera: the seed lists the venue cameras first as `address = "register:camN"` +
  `fallback_address = <the literal relay>` (used while no fresh registration exists: an older relay
  binary, a relay that has not announced yet), then the other venue's cameras with `visiting = true`.
  A visiting block is left out of the panel (and not polled) until its relay has announced itself once
  (`aggregator::camera_listed`), and stays (red, stale) after, so a normal event still shows only the
  venue's cameras (the 26.10. ruling). A visiting block must name `register:<id>` (refused at load).
  A handheld override (`N=sbc-relay@...`) is written as before, no fallback. Live boxes keep their
  hand-made `bkshading.toml` until it is re-seeded by hand (the seed is written only when absent).
  verify-device `(ao)` FAILs a relay.env without the two lines. The visiting set is the union of every
  strih fact file's `STRIH_CAMERAS` (`strih_box_fleet_cameras`), never a literal range. Known limits:
  - **Registration is unauthenticated** (the issue-808 design), so with register-first any LAN host that
    POSTs `id=camN` redirects that camera's polls and writes until the real relay announces again. The
    rig LAN is the trust boundary, as it already was for the handheld.
  - **A visiting camera's block is its cambox relay**, never the other venue's handheld override: the ONE
    travelling `handheld-1` already drives a home block on each strih, and naming it twice would list the
    visiting block at every normal event. At a joint event the handheld drives the home block the strih in
    use names for it.
  - **Preview workers run for every declared camera**, visiting ones included (they start from the static
    list at boot); an absent NDI source only backs off, as for any offline camera.
- **`relay/src/transport.rs` is split** (issue 808): `transport/gphoto2.rs` (the CLI seam),
  `transport/reads.rs` (read constants + pure helpers), re-exported from `transport`. New code goes
  into a child module, never back into transport.rs (size budget).
- Relay transport = shell out to the `gphoto2` CLI behind the `CameraTransport`-style trait (NOT a
  `libgphoto2` FFI binding — the trait keeps FFI as a future 2nd impl). Rationale: no build-time C
  dep → clean ARM cross-build for the zero-class arm64 SBC handheld relay.

  constant was renamed `COLOR_FORMAT_UYVY_BGRA` → `COLOR_FORMAT_BGRX_BGRA` (behaviour unchanged; the
  same harmless mislabel still stands in the main display path `src/ndi.rs`, a separate subsystem).
  `ndi` is the DEFAULT service build (owner decision 2026-08-24). **Graceful degrade on a libndi-less host:** with ndi default,
  `source::build_default_source` always builds the real `NdiPreviewSource` (never a stub fallback);
  a missing runtime bails from `ndi_source::load_uncached()` with a platform-neutral message
  (`"NDI runtime not found (install the NDI SDK / NDI Tools, or set NDI_RUNTIME_DIR_V6); ..."`),
  and `worker::run_forever` logs it as a `tracing::warn!` (cam+source+error) then backs off and
  retries forever — fail-loud, non-crashing, no silent stub. **libndi provisioning on the strih
  service host + the live end-to-end verify against a cambox NDI source remain the supervisor's rig
  steps.**


## SBC / handheld provisioning → `.claude/rules/bkshading-sbc.md`
The handheld SBC (the device-agnostic zero-class arm64 board, the reused relay unit, the read-only
root from the shared `scripts/lib/ro-root.sh` canon, the arm64 relay deploy that reads the target's
root, the supervisor bench checklist) lives in its own rule. It auto-loads on the SBC scripts,
`scripts/lib/ro-root.sh` and the relay deploy.


## Service DEPLOY path onto strih (Windows) — issue 808 (repeatable, mirrors the relay canon)

**Retired default (issue 1317 part 3):** the Windows strih PC this section targets is GONE (M4
cut-over); the service now runs on the Linux strih-snv as a systemd unit (the issue-1353 section
below). `bkshading-deploy-service.sh` therefore has NO default `--host` (it used to be 10.77.9.202,
now strih-snv's address) and refuses a linux-genlock fleet address via `obs_fleet_refuse_linux_target`,
naming `setup-strih.sh` step 16c. It stays usable for an explicit Windows box.

The service exe is the pinned release's `bkshading-service-windows-amd64.exe`. The repeatable deploy of THAT onto strih
is `scripts/bkshading-deploy-service.sh` (dev1 orchestrator) + `scripts/bkshading-install-service.ps1`
(on-box installer) + the pure-invariant lib `scripts/lib/bkshading-deploy-service-runtime.sh` — the
ONE source of truth for the exe, install dir (`C:\bkshading`), config filename, task
name (`bkshading-service`), port (`8770`, the shading contract), and keep-alive cadence, so
the .sh / the .ps1 cannot drift (`tests/python/test_bkshading_deploy_service_808.py` cross-checks).

- **Transport = the recordings-retention style:** `scp -O` the exe + config seed + installer to the
  box, then run the installer via `powershell -NoProfile -ExecutionPolicy Bypass -File` — NEVER a
  nested `powershell -Command` over ssh. DRY-RUN is the DEFAULT for BOTH the `.sh` and the `.ps1`;
  `--execute` / `-Execute` performs the mutating half. The `.sh` DRY-RUN touches nothing remote.
- **Keep-alive, NOT Task Scheduler RestartCount:** Task Scheduler has no real Restart=on-failure for
  a long-lived process, so the installer registers ONE task with two triggers (AtLogOn + a repetition
  every N min) whose action re-runs the DEPLOYED installer `-KeepAlive -Execute` — the idempotent
  check-and-relaunch idiom (obs-self-heal / avsync-keepalive, `.claude/rules/avsync-monitoring.md`).
  The `-KeepAlive` pass matches the running service by its EXACT `ExecutablePath` (never a bare
  process name — the avsync gotcha) and relaunches via `Start-Process ... --config <toml>` if absent.
- **Config is seeded ONLY IF absent** (`bkshading.example.toml` -> `bkshading.toml`) — a redeploy
  never clobbers an operator-tuned config. The service config carries NO credential (pure camera
  list + bind + `[preview]`), so nothing secret is ever written by the deploy. The `.ps1` is pure
  ASCII (scp'd → non-UTF-8 codepage on the box, `.claude/rules/recordings-retention.md`).
- **Two verify gotchas hardened by the #808 review (reusable for ANY Windows-service deploy here):**
  (1) the port check must confirm the LISTENER'S OWNER, not just that *something* listens — resolve
  `(Get-NetTCPConnection -LocalPort N -State Listen).OwningProcess` → that process's `ExecutablePath`
  and require it be the DEPLOYED exe; a bare "port N is Listening" false-greens when a stale/foreign
  instance (e.g. the manual `C:\stage-bkshading` from #1157) holds the port while the new exe fails to
  bind. Pair it with an install-time **by-NAME** stop (migration off the manual stage) while the
  steady-state keep-alive pass keeps the EXACT-path match. (2) byte-verify the scp'd exe:
  local `sha256sum` vs remote `certutil -hashfile <path> SHA256` (line 2 is the hash — strip
  whitespace, lowercase; empty side = mismatch), mirroring the relay sibling — a truncated scp is
  otherwise caught only by scp's exit code.
- **UNVERIFIED (supervisor rig step):** the LIVE `--execute` install against strih (scp +
  `Register-ScheduledTask` + `:8770` verify) + confirming the panel is up — done from a session with
  win-strih MCP / rig access, not an isolated worktree lane. This complements the deferred libndi
  provisioning + live NDI-preview verify already noted above.


## Service DEPLOY path onto strih (Linux notebook strih-snv) — issue 1353 (the post-M4 sibling of the Windows path)

Post-M4 the notebook IS the strih, so the service is PROVISIONED (not "deployed" ad-hoc) as a systemd
unit — the `setup-strih.sh` / `verify-strih.sh` / `strih-provision.sh` machinery, not a bespoke
orchestrator. The service comes from the pinned release (`shading_release_service`, `shading-pinned-release.md`).
Pieces: two pure printers (`strih_bkshading_unit_text` /
`strih_bkshading_config_text`), the committed `systemd/bkshading-service.service` (byte-identical to
the printer — parity-tested), `setup-strih.sh` step **"16c"** (lettered, TOTAL_STEPS stays 17), and
`verify-strih.sh` item 31. Tests: `tests/strih_provision_pure_functions.rs` (source-and-call + anchors).

- **The panel web assets are EMBEDDED in the binary**, so the service is SELF-CONTAINED: the unit +
  verify depend only on the binary + its `:8770` listener + `/api/version`, never a filesystem `web/`.
  The release's `web/` is installed beside it as a copy; do not add a unit dependency on it.
- **NO cambox relay repoint is needed to migrate the service to another strih.** The SERVICE still
  connects OUT to each relay on :8771. Since issue 1431 the cambox relay also ANNOUNCES itself to
  `strih.lan` (its relay.env carries `CAMERA_BOX_CAPTURE_FPS`, `BKSHADING_REGISTER_URL` and
  `BKSHADING_RELAY_ID`), so the address the service dials comes from that registration, else the
  block's `fallback_address`. Moving the service host means pointing the `strih.lan` record at it,
  nothing on the boxes.
- **Config seeded ONLY IF absent** (`[ ! -f /etc/bkshading/bkshading.toml ]`, the `projector.json`
  precedent) from `strih_bkshading_config_text` (pinned from the canonical
  `shading:bkshading/service/bkshading.example.toml` — the schema the Windows config was itself seeded from,
  since a live read of `C:\bkshading\bkshading.toml` needs the Windows PC ON). No secret in the config.
- **`systemctl enable` ONLY — never start from the installer** (the SUPERVISOR deploys the running
  service; the Windows service is the fallback until the owner accepts). So **`verify-strih.sh` must
  NOT hard-FAIL an enabled-but-INACTIVE unit** — grade it like the `intercom-hub`/`janus` enable-only
  items (report-only `note` on inactive; the unit-file ABSENT is a hard FAIL; an ACTIVE unit is
  asserted HARD). The active check is the same "confirm the LISTENER'S OWNER" rule as Windows: the
  `:8770` owner (via `ss -tlnp`) must be the `bkshading` binary AND `/api/version` must answer.
- **GOTCHA — `/api/state` is the RELAY's endpoint, not the SERVICE's** (live 23.9.2026): the relay
  (cambox `:8771`) and the intercom hub (`:8790`) serve `GET /api/state`; the service router
  (`shading:bkshading/service/src/http.rs`) serves `/api/version`, `/api/cameras`, the panel assets and `/ws`,
  so `:8770/api/state` is 404. verify item 31 first probed `/api/state` and graded a healthy running
  service FAIL "silent"; `tests/strih_provision_pure_functions.rs` now parses the router and requires
  the probed path to be one of its routes. Any new service health probe: pick a route from the router.
- **GOTCHA — the `ss -tlnp` owner-extraction trailing-quote trap** (`strih_bkshading_listener_owner`):
  `grep -oE 'users:\(\("[^"]+"'` captures the CLOSING quote too (`users:(("bkshading"`), so a naive
  `sed -E 's/.*\("//'` leaves `bkshading"` and `[ owner = bkshading ]` is ALWAYS false — a healthy
  service graded unhealthy (it fails SAFE, never false-green, but the active-health check becomes
  unpassable). The parse MUST capture-group: `sed -E 's/.*\("([^"]+)".*/\1/'`. It is a pure fn so it
  has a regression test; any similar `users:((...))` owner parse needs the capture group.
- **UNVERIFIED (supervisor rig steps):** the LIVE `setup-strih.sh` run on strih-snv (10.77.9.202) +
  `verify-strih.sh` green + STARTING the service (`systemctl start bkshading-service`) + the owner's
  panel acceptance on `http://strih.lan:8770/` (development = one camera on cam1) with the PC off.


## E2E harness must PAUSE the relay on the two measurement-critical camboxes (issue 808, live evidence)

The relay is a fleet-standby service — owner directive: it runs on EVERY cambox so any camera can
be shaded on demand — but its gphoto2 USB-PTP polling causally degrades the E2E harness's own
measurement quality on the two boxes it needs to trust most: the SOURCE camera (USB-bus
contention with the physical camera's Cam Link 4K capture device — cam1 measured 58.3-58.9 fps vs
a healthy 60.0, confirmed by stop/start isolation) and cam2/painter (a 3-core box already running
camera-box RT + the painter, where the extra CPU/jitter correlates with worse dual-QR window
quality — 2/2 clean relay-off vs 4/5 over-tolerance relay-on). Evidence: issue 808 comments
2026-08-29T09:59:31Z / 2026-08-29T15:54:47Z. If you ever see a mysteriously degraded/dropped-frame
E2E run and a camera happens to be physically cabled to a cambox at the time, check
`systemctl is-active bkshading-relay` on the SOURCE box and cam2 first — it is a known, already-
mitigated contention source, not a mystery regression in camera-box/genlock code.

- **The fix is `scripts/lib/bkshading-e2e-pause.sh`** (mirrors the sibling
  `bkshading-preflight.sh` split: pure remote-text builders + a fail-safe pure parser + two thin
  ssh orchestrators). `scripts/recording-e2e.sh` pauses (`systemctl stop`) the relay on the
  run-resolved `$CAM1_IP` (the SOURCE camera, whichever of cam1/cam3/cam4/cam5/cam6 was selected)
  and `$PAINTER_IP` (cam2) right after the existing `bkshading_preflight_report` call, recording
  each box's PRIOR active state; `cleanup()` restores it at the very end, but ONLY on a box where
  the pause step found it genuinely active beforehand — a box the operator deliberately silenced
  (e.g. via the interim manual `systemctl stop`) is never woken back up by a run.
- **Do not conflate this with the M3 preflight check** (`scripts/lib/bkshading-preflight.sh`,
  automated shutter-checklist WARNING) — that reads the camera's state; this pauses the relay
  process entirely, and both run back-to-back at `[0/8]`.
- **This is deliberately its OWN, dedicated ssh call — never spliced into the existing
  `CAMBOX_PARALLEL_*` device-restore group** in `cleanup()` (`cambox_parallel_retry_failed`'s own
  retry command is camera-box-specific and would be wrong to apply to a bkshading-relay-only
  restore). It runs LAST in `cleanup()`, after the `#684`-class FINAL camera-box.service verify,
  so this non-safety-critical restore never delays the safety-critical device-restore phase.

### The CANCELLED-run case — a persistent ON-BOX marker, not just the in-memory was-active var (issue 1278)

**The pause/restore pair above has a gap the runner's own kill semantics create: a CANCELLED (`gh
run cancel`) or otherwise KILLED run never reaches its restore call at all.** `_bksh_was_active`
(and the `$BKSH_PAUSE_*_WAS_ACTIVE` vars that carry it) live ONLY in the memory of the ONE
`recording-e2e.sh` process running that attempt — the runner kills the process tree SECONDS after
a cancel, while `cleanup()`'s own device-restore phase (which must run BEFORE the bkshading
restore, per the ordering rule above) takes MINUTES. Live incident (2026-09-02, run 33640143227):
attempt 2's `[0/8]` pause found the relay active on cam1+cam2 and stopped it, was cancelled with
no `[cleanup]` line ever printed; attempt 3's OWN pause then found the unit already `inactive`
(from attempt 2's stop) with no way to know that was OUR doing, read the fail-safe default
was-active=0, and — correctly per its OWN inputs — left it stopped. Net: both boxes sat silently
shading-dead for 76 minutes until a human noticed and ran `systemctl start` by hand.

- **The fix: a persistent marker file ON THE BOX**, `/run/bkshading-e2e-paused` (tmpfs — writable
  even on the cambox's read-only rootfs; vanishes on reboot, after which the `enabled` relay unit
  starts on its own anyway). ONE source of truth: `bkshading_e2e_pause_marker_path` (default
  `/run/bkshading-e2e-paused`, overridable via `BKSHADING_E2E_PAUSE_MARKER` for testing),
  consumed by BOTH `bkshading_e2e_pause_stop_cmds` and `bkshading_e2e_pause_restore_cmds`.
- **`bkshading_e2e_pause_stop_cmds` now treats EITHER `systemctl is-active` OR an already-existing
  marker file as was-active=1** — a marker still on disk means a PRIOR run paused the relay and
  never got to restore it, so THIS run must (and does) report was-active=1 and takes over the
  restore obligation. When was-active becomes 1, the marker is written BEFORE `systemctl stop` —
  so a run killed between the write and the stop still leaves the marker behind for the next run.
- **`bkshading_e2e_pause_restore_cmds`'s "1" branch clears the marker ONLY in the SUCCESS path**,
  after `systemctl is-active` has actually confirmed the relay came back up — the pre-existing
  WARNING/failure branch (unit never comes back active) deliberately LEAVES the marker in place,
  so a genuinely-failed restore is retried by a LATER run's pause step (or a future watchdog)
  instead of the marker being lost and the relay staying silently dead forever. The "0" (no-op)
  branch is untouched — an operator's deliberate manual stop, WITH NO UNRESOLVED E2E-CAUSED
  MARKER ON THE BOX (unit already inactive, no marker), is still never woken back up by a run,
  exactly the pre-existing #808 guarantee. **Precision caveat (review finding):** the marker
  itself can't distinguish "we still owe a restore" from "the operator separately silenced it in
  that same window" — if an earlier CANCELLED run's marker is still on disk when an operator
  independently `systemctl stop`s the relay by hand, the NEXT run's restore will re-activate it
  against that fresh manual intent (a narrow coincidence-window residual, not a regression: the
  pre-1278 code had no better answer here either — it just silently left the relay broken).
- **No `scripts/recording-e2e.sh` edit at all** — the whole fix lives in the sourced lib (the #675
  pattern); both existing anchored call sites (the `[0/8]` pause + both cleanup/temp-trap restore
  calls) stay byte-identical. Tests: `tests/harness_bkshading_e2e_pause_808.rs` — pure builder/
  parser assertions for the marker existence-check + write-before-stop ordering + marker-clearing-
  only-on-success, PLUS two full functional simulations (a fake `systemctl` + a fake unit-state
  file, driven through the REAL stop_cmds/restore_cmds/parse_state) reproducing the exact live
  incident sequence (pause→cancel→pause-again reads was-active=1 via the marker→restore succeeds→
  marker cleared) and its negative companion (no marker + already-inactive unit stays was-active=0,
  restore stays a no-op, no marker ever created).
- **Not done here (optional per the ticket, not required for the fix): a dev1 watchdog that
  notices a stale marker with no live E2E run holding the rig lease and restores it proactively.**
  The marker alone already closes the 76-minute silent-dead window down to "the NEXT E2E run" —
  a watchdog would shrink it further for the case where no E2E runs again soon, but is deliberately
  out of scope for this fix (the ticket names it optional).


## The relay lifecycle is EVENT-ONLY on the rig — `rig-mode.sh test` stops+disables it, `event` starts it (issue 1311)

**Two boot sticks died in 24h (cam1 13.9., cam2 14.9.) on exactly the two boxes with a shading
camera on the SAME USB3 root hub as the boot stick** (issue 1309/1311 Finding 1/2): a consumer
896 mA SanDisk + a 512 mA Cam Link grabber + a 144 mA+ BMPCC PTP camera share one mini-PC 5 V rail,
and every relay start/stop is a PTP-session power change on that hub. Owner ruling
(14.9.2026): „nie nemas nic co by moholo odpalit disk skusat" — pursue it PASSIVELY, and remove
the software provocations. During DEVELOPMENT the shading panel is not needed, so the relay has no
business running (and polling the shared bus) while measurements run.

- **`rig-mode.sh test` STOPS + DISABLES `bkshading-relay`; `rig-mode.sh event` ENABLES + STARTS it**
  — via `scripts/lib/bkshading-relay-mode.sh` (`bkshading_relay_mode_apply <test|event> <cam_pw>
  <label=ip>…`), an additive sourced-helper call in `do_test`/`do_event` (no anchored `rig-mode.sh`
  line edited, the additive pattern). The roster is DERIVED, not a literal list: the same two boxes
  the issue-808 E2E pause + `EVENT_ASSERT_TARGETS` use — the resolved source box
  (`${RIG_SOURCE_BOX}=$RIG_SOURCE_IP`) + `cam2=$PAINTER_IP`. The unit name is the ONE source of
  truth `bkshading_relay_unit_name` (from `bkshading-relay-runtime.sh`). Every systemctl line is
  `|| true`-tolerant and the ssh loop tries every box. A box whose state did not land (unreachable,
  persist not landed, or since issue 1407 a root that did not close read-only) makes the apply
  return non-zero, and rig-mode RECORDS that rc: it warns by name, runs every remaining step of the
  switch (EVENT's burn-OFF, mapping and contract; TEST's painter steps) and exits non-zero at the
  end, naming the box and its writers (the issue-868 pattern, `.claude/rules/ro-window.md`).
- **This makes the issue-808 E2E pause/restore a TRUE no-op in TEST mode:** with the relay already
  stopped+disabled, `bkshading_e2e_pause_stop_cmds` reads `systemctl is-active`=false AND (in steady
  state) no `/run/bkshading-e2e-paused` marker → `was-active=0` → `bkshading_e2e_pause_restore_cmds`
  takes its `true # leave it stopped` branch → toggles nothing. No change to
  `bkshading-e2e-pause.sh` was needed; verify that path STAYS a no-op if either lib changes.
- **It also removes the issue-1229 gphoto2 polling noise from every measurement** — the bus-friendly
  min-interval floor below still applies while the relay IS running (EVENT mode), but development
  runs no longer pay any relay bus traffic at all.
- **Distinct from the deploy/ops gate below** ("NEVER deploy OR restart a cambox relay during
  production"): that is a rig-busy gate on the DEPLOY TOOL; this is the relay's steady-state
  lifecycle on the TEST/EVENT switch. Both point the same way — the relay is a broadcast-time
  service, not a development-time one.


## A manual interim `systemctl stop bkshading-relay` is NEVER auto-restored — not even by `Restart=on-failure` (issue 1228 TERM-origin finding)

**Root cause of the 29.8.-30.8. cam1 incident (relay found dead a full day after it was stopped):**
NOT `bkshading-deploy-relay.sh`'s own stop→start (that always re-starts what it stops, and the
deploy at 06:22-06:33 UTC on 29.8 was 3+ hours before the observed TERM). The actual cause was a
**manual interim mitigation** — `systemctl stop bkshading-relay` run by hand on cam1 at
`2026-08-29T09:56:25Z` while investigating the SAME gphoto2/USB-bus contention issue 1229 later
fixed properly (issue 808 comment `2026-08-29T09:59:31Z`, 3 minutes after the TERM: *"relay STOP:
captured 59.8-60.0 fps... Mitigácia TERAZ: bkshading-relay na cam1 STOPNUTÝ"*). `systemctl stop`
sends `SIGTERM` to the main process — exactly the journal's `code=killed, signal=TERM`. The unit was
left `enabled` (comes back only on a REBOOT) and nobody manually restarted it, so it stayed dead
until the owner tried to use shading the next day.

**The lesson generalizes past this one incident: a DELIBERATE `systemctl stop` is never
auto-recovered by `Restart=on-failure`, by design — and neither is ANY OTHER SIGTERM, regardless of
who sent it.** Two independent systemd mechanisms both point the same way here: (1) systemd
suppresses `Restart=` entirely when a stop was requested by the service manager itself
(`systemctl stop`/`systemctl restart`, e.g. the deploy flow), and (2) separately, systemd treats
SIGHUP/SIGINT/**SIGTERM**/SIGPIPE as a CLEAN exit by default (same as exit code 0) for the purpose
of `Restart=on-failure`'s own "was this a failure?" check — so even a SIGTERM from a source OTHER
than the manager (a stray `kill -TERM`, some other tool) still would not trigger a restart under
`on-failure`. Issue 1228's `Restart=` code lane has now LANDED (`systemd/bkshading-relay.service`:
`Restart=always`/`RestartSec=3` → `Restart=on-failure`/`RestartSec=5`) — the precondition (issue
1229's USB2 topology question) was settled once the owner ruled the physical re-cabling step
impossible (single xHCI controller), so today's topology already IS the target one. As predicted
here, it **only** protects against a genuine unexpected crash — a non-zero exit, an operation
timeout, a watchdog failure, or termination by a signal OTHER than the four "clean" ones above
(SIGSEGV, SIGABRT, SIGKILL/OOM-kill) — it does NOT bring back a relay that was deliberately
silenced as an interim mitigation (correct behavior: an operator's deliberate stop should stay off
until they undo it), and it does NOT change the deploy stop→start flow either (both `always` and
`on-failure` already skip a manager-requested stop, and `on-failure` additionally treats SIGTERM as
clean regardless of origin). **Any interim "stop this on box X while we investigate" mitigation
still needs its OWN explicit tracking** (a ticket comment naming which boxes were stopped + a
reminder to restore them) — the harness-managed pause (`bkshading-e2e-pause.sh`, above) only covers
stops the E2E harness ITSELF performs; it has no visibility into an ad-hoc manual stop done directly
on the rig.

**Why this "SIGTERM is always clean" fact applies cleanly to `bkshading-relay` specifically —
verified against its own source, not just systemd theory (issue 1228 review-fix follow-up):**
`shading:bkshading/relay/src/main.rs` only installs `tokio::signal::ctrl_c()` (SIGINT / Ctrl-C) for graceful
shutdown — it registers **no SIGTERM handler at all**. So a SIGTERM (from `systemctl stop`, the
deploy flow, or a stray external `kill -TERM`) is never intercepted; it kills the process via the
raw OS default, which is exactly the `code=killed, signal=TERM` shape the 29.8 incident's journal
showed, and exactly systemd's "clean exit" case (confirmed from this box's own local
`man systemd.service` "Table 1. Exit causes and the effect of the Restart= settings" — `on-failure`
has NO entry for the "Clean exit code or signal" row). **This does NOT contradict
`imag-obs-supervision.md`'s opposite-sounding claim** ("an external `pkill -TERM` … STILL triggers
`Restart=on-failure`") — that is about a DIFFERENT process (OBS) that DOES install its own signal
handling and exits via a controlled-but-non-zero path (or crashes) in response, which lands in the
table's "Unclean exit code" / "Unclean signal" rows instead. The generalizable rule: whether a raw
SIGTERM is "clean" (no restart) or "unclean" (restart) under `on-failure` depends on **whether the
specific binary catches the signal and how it exits in response** — never assume either way without
checking that binary's own signal handling, the way this was checked here.

## E2E `[0/8]` camera pre-run auto-check reads `/api/state` — shutter+iso+aperture, NOT focus/exposure-MODE (issue 808 shutter half + issue 1237 exposure half)

`scripts/lib/bkshading-preflight.sh` (wired at `recording-e2e.sh:650`, `bkshading_preflight_report
"$CAMERA_NAME" "$CAM1_IP"`, tested by `tests/harness_bkshading_preflight_808.rs`) automates the
`#220` CAMERA PRE-RUN checklist by reading the relay's `GET /api/state` — ONE `curl -fsS`, served
from the relay's issue-1229 read-floor cache (never a direct gphoto2 call). It is REPORT-ONLY
(always `return 0`, WARN never abort — owner M3 decision).

- **What is measurable from `/api/state`, and what is NOT.** `RelayState`/`ShadingParams`
  (`shading:bkshading/proto/src/wire.rs`) + the relay read plan (`relay/src/transport.rs` reads only
  `iso, f-number, d002, d004/d005, d006/d007`) expose SHUTTER (`params.shutter`, a DENOMINATOR —
  500 == 1/500s, LARGER = faster), ISO/gain (`params.iso`), and APERTURE (`params.apertureAv`).
  There is **NO focus-mode field and NO auto/manual exposure-MODE field.** So the shutter check
  (issue 808) and the exposure-VALUES-readable check (iso+aperture, issue 1237) are real; manual
  FOCUS and auto/manual EXPOSURE MODE are genuinely unreadable → surfaced as a report-only
  `bkshading_preflight_focus_note_message` NOTE (LOUD-UNKNOWN, never a fabricated pass); issue 1238
  additionally wired an informational `bkshading_preflight_focus_distance_message` line for the one
  honest focus signal that IS readable (manual focus DISTANCE, d003) — see the "Relay
  focus-distance exposure" section above. **Do NOT let an OK line claim "exposure fixed /
  satisfied automatically" — presence of a value ≠ a fixed MODE; a BMPCC in auto still reports
  concrete iso/f-number.** (The exposure OK line was caught doing exactly this in review.)
- **Report-only python3-safety pattern (reuse for any python3-backed preflight lib).** The JSON
  extractors are python3 one-liners. Under the caller's `set -euo pipefail`, a bare
  `x="$(py_extractor "$raw")"` will ABORT the whole E2E if python3 is missing/crashes — the exact
  opposite of a report-only check. Guard it: a LOUD-BY-NAME `command -v python3 || { NOTE; return
  0; }` gate at the top of the orchestrator + `|| true` on every python3-backed substitution (a
  transient failure degrades to EMPTY → a report-only warn, never a crash). The extractors treat a
  JSON bool as ABSENT (`not isinstance(v, bool)` — python bool is an int subclass) and print EMPTY
  on a non-dict body / non-dict `params` / null (never a fabricated value).
- **Extending it is anchor-safe by construction.** New behavior goes into the LIB
  (`bkshading_preflight_report` + pure fns) — `recording-e2e.sh`'s one call line stays
  byte-identical, so the #675 anchor sweep is trivially clean. Keep the classifier's decision the
  single source of truth for a WARN's named parameter (pass the STATUS into the message, don't
  re-derive the missing set from the values in two places).
- **Its ENFORCING sibling (issue 1371) is a different step: do not merge them.** This preflight
  runs BEFORE the relay pause and only WARNS from `/api/state`. `scripts/lib/camera-test-settings.sh`
  runs AFTER the pause, talks gphoto2 directly (the relay is stopped), SETS `iso` + `d002` to
  `scripts/camera-test-baseline.json`, and aborts on a read-back MISMATCH. It uses the relay's own
  key names, pinned by pytest against `CORE_CONFIG_KEYS` + `plan_writes`, so renaming a relay key
  breaks that test first. See `.claude/rules/camera-test-settings.md`.

## shading v0.2.x = the REST relay, chosen per box (issue 1452)

The relay drives the camera over the Blackmagic REST API on the camera's USB-C link (CDC-NCM); the
gphoto2 text above is the v0.1.x relay, still the fleet default. Which box runs which, the camera link
file and the service's pin: `.claude/rules/shading-pinned-release.md`. The issue-1238 preflight prints
`focusNorm` (lens/focus normalised) from a REST relay, whose `focusDistance` is always null. The E2E
test-camera step speaks REST too (`.claude/rules/camera-test-settings.md`).

## The fleet upgrade carries the relay (issue 1452)

`scripts/deploy-fleet.sh` deploys the relay of the SAME CI run on every cambox that has one, through
`bkshading-deploy-relay.sh` itself, names a relay failure in FAILED and prints each box's camera-box
+ relay version; `rig-dev-handover-check.sh` FORGOTs a relay older than camera-box. The relay deploy
accepts a rig lease held by the caller's own run. Detail: `.claude/rules/fleet-relay-upgrade.md`.

## NEVER deploy OR restart a cambox relay during production — the deploy tool is now rig-busy gated (2026-09-13 escalation, issue 1229)

**The 2026-09-13 escalation, past the capture-dip class this ticket started on:** deploying the
relay to cam1 (`scripts/bkshading-deploy-relay.sh`-style scp/mv-over-running + `systemctl restart
bkshading-relay`) DURING a live production recording did not merely dip the capture rate — it
fork-WEDGED the whole box. ssh, the `linux-cam1` MCP agent, and `gphoto2 --auto-detect` all
reset/timed out while the box still pinged and the already-running relay still answered HTTP;
recovery needed an owner power-cycle (no remote reboot path to a cambox). The gphoto2 PTP polling
sharing the single xHCI controller with the grabber is dangerous UNDER PRODUCTION LOAD beyond the
capture drop — it cascades into stuck D-state gphoto2 processes and fork-exhaustion of the box.

**The doctrine (both halves now enforced in the deploy tool):**

- **Every bkshading relay deploy/restart is gated on rig-not-busy.** `bkshading-deploy-relay.sh`
  runs a rig-busy PREFLIGHT before its first cambox ssh/scp, reusing the ONE shared guard
  `recording-e2e.sh` uses — `stray_session_check_assert HERE STRIH STREAM WHAT` (→ `obs_phase2.py
  rig-busy-check`; NEVER a duplicated per-box WS loop, see `rig-mutation-broadcast-guard.md`). It
  REFUSES (non-zero, naming what is streaming) when strih/stream are broadcasting, and fail-OPENs
  (WARN + proceed) ONLY when NO box is readable — never blocks on a transient read error.
  `--force-live` (logged loudly) is the SUPERVISOR-ONLY override for a genuine idle-rig-but-guard-
  unavailable case; a normal deploy never passes it. Env `STRIH_HOST`/`STREAM_HOST` (default
  10.77.9.202/.204) + `OBS_PASSWORD` mirror `rig-busy-gate.sh`; `BKSHADING_DEPLOY_OBS_PHASE2_DIR`
  is a test seam pointing the guard at a fake `obs_phase2.py`. The gate applies to EVERY
  deploy including a handheld SBC (`--arch arm64`), which shares no xHCI bus with the rig — this is
  a deliberate conservative default (a handheld camera may itself be in use during a broadcast);
  pass `--force-live` for a genuinely off-rig handheld deploy.
- **SUPERSEDED 25.9.2026 (issue 808, see the section below): the deploy now STOPS an active relay
  before the swap and starts it again after, restoring its previous state.** It used to leave the
  running relay on the old (deleted-but-open) inode, which made the final `remount,ro` fail
  "busy" and left cam6/cam7 read-WRITE. The ETXTBSY-safe stage + atomic `mv -f`
  (`<dest>.deploy.<pid>`) stays. The deploy never STARTS a relay that was stopped, and it is still
  rig-busy gated, so the stop/start only happens on an idle rig. (The relay `Restart=on-failure`
  lifecycle is issue 1228, unchanged — this deploy touches no systemd unit.)


## Every cambox gets the relay from setup-device; the relay deploy stops, restores and fails loud (issue 808, 25.9.2026)

**The finding:** the 24.9 M.2 re-provisioning of cam1-4 left NO relay on them (no unit, no binary,
no gphoto2). Only the separate `bkshading-provision-relay.sh` installed it, and `verify-device.sh`
`(am)` grades a missing relay `na`, so nothing noticed. Separately, `bkshading-deploy-relay.sh`
swapped the binary under a RUNNING relay: the process kept the deleted-but-open inode, the final
`remount,ro` failed "busy", the script swallowed it (`2>/dev/null; true || true`), printed OK, and
the root stayed read-WRITE (cam6/cam7).

- **ONE install body:** `scripts/lib/bkshading-relay-provision.sh` (gphoto2 + env derived from the
  camera-box drop-ins + unit + optional binary + `enable` OR `disable`, literal is-enabled
  read-back, never a start). Both `bkshading-provision-relay.sh` (CLI) and `setup-device.sh`
  `[bkshading-relay]` source it. Never copy the body back into a script. The CLI takes
  `--rig-mode test|event` too (hostname = the box, `BKSHADING_RELAY_DEVICE_NAME` overrides) and
  `--check` grades the same enable-state. On a ROSTER box (source box / cam2) the CLI REFUSES
  (exit 2) without `--rig-mode` or `CAMERA_BOX_RIG_MODE` -- the answer differs per mode, and a
  guessed `test` during an event would disable the broadcast relay; any other box is `enabled` in
  both modes, so no mode is needed there.
- **setup-device enable-state follows `--rig-mode` (default `test`):** TEST = the relay roster (the
  source box from `camera_source_box` + cam2, the SAME two boxes `rig-mode.sh` stops+disables)
  installed DISABLED, every other box enabled; EVENT = enabled. The default is `test` because it is
  the development steady state AND the passive issue-1311 direction; `rig-mode.sh event` enables +
  starts it. The pure decision is `bkshading_relay_expected_enable_state` (ONE table for setup and
  verify; an unresolvable source box outside EVENT = `unknown` for every box -> setup installs
  disabled + records it, verify fails). The painter box comes from `bkshading_relay_roster_painter_box`.
  A re-provision in TEST only DISABLES a running relay, never stops it: a stop is itself a PTP power
  change, and `rig-mode.sh test` owns the relay lifecycle.
- **The relay binary on a cambox:** the pinned shading release, by `curl` from the box, or
  `setup-device.sh --relay-binary <path|url>` carrying the same bytes (the `--probe-binary` idiom;
  `bkshading_relay_provision_binary_plan` decides, `bkshading_relay_provision_fetch_binary` carries it out).
  No source = a RECORDED `RELAY_PROBLEM` (the unit/env/gphoto2 still go in), and STEP 19 refuses
  Setup Complete — never an abort before the ro fstab.
- **verify-device `(ao)`** grades binary + byte-matching unit + env + gphoto2 + the mode's
  enable-state. The mode is `CAMERA_BOX_RIG_MODE=test|event` (the SAME env setup-device reads), else read off cam2's painter state
  (`rig_mode_state_probe_remote_snippet`); only a roster box needs it, and an unreadable mode FAILs
  for one. It sits after `(am)` and BEFORE `(an)`: the `(an)` block is sliced to `(q)` and EXECUTED
  by `tests/python/test_ndi_discovery_1342.py`, so a new block between `(an)` and `(q)` runs inside
  that test and fails it.
- **The deploy's relay state:** read `systemctl is-active` first. An unreadable read (ssh failure /
  empty) REFUSES before touching the box -- never taken for "not running".
  `bkshading_deploy_restore_action` = `start` for `active|activating|reloading` (the unit is
  Restart=on-failure, so `activating (auto-restart)` would otherwise relaunch the old inode mid-swap),
  `none` otherwise. A running relay is stopped BEFORE the rw remount and started AFTER the ro
  remount. A byte-verify failure leaves it stopped, loudly, never running unverified bytes.
  `bkshading_deploy_should_start` stays `no`.
- **Every exit after the box is touched restores it:** `finish_once` (ro remount + relay restore,
  exactly once; `BOX_DIRTY` clears only AFTER it returned) runs on each explicit failure path AND
  from an EXIT trap (INT/TERM/HUP -> exit 130/143/129), so a Ctrl-C mid-scp still leaves the root ro
  and the relay in its previous state. The trap turns errexit off, ignores further signals and
  guards every write (`echo ... >&2 2>/dev/null` -- NOT `{ echo >&2; } 2>/dev/null`, which silences
  it), so a HUP'd terminal whose stderr writes all fail still restores. A signal INSIDE the restore
  (`FINISHING=1`) RE-RUNS it (finish_box is idempotent: remount,ro on an ro root and start on an
  active unit are no-ops) and asks for a hand check (`findmnt` / `is-active`) only if the re-run
  fails too. `trap ''` alone does NOT make that re-run Ctrl-C-proof: sshpass installs its own
  handlers and forwards SIGINT to ssh, so the trap runs the re-run's ssh under `setsid -w`
  (`RESTORE_SESSION`), out of the terminal's process group. The test for it needs a fake sshpass
  that handles + forwards SIGINT -- with an empty prefix, the inherited SIG_IGN hides the bug.
  Consequence: the re-run restore can only be stopped with SIGKILL. That is bounded (ConnectTimeout
  10, ServerAlive ~30 s, the one verified ro close; the Type=simple unit's `start` returns at once).
  `ssh_box`/`scp_box` carry ServerAliveInterval so a dead connection cannot postpone the restore.
- **The ro remount is checked** -- since issue 1407 through the ONE shared verified close
  (`ro_window_close_cmds`, `scripts/lib/ro-window.sh`, `.claude/rules/ro-window.md`), run on the box
  in ONE ssh call, no retry. The root mode is READ (never the mount exit code alone); a root left
  writable prints the box's FAIL lines: the writers (`fuser -vm /` filtered to ACCESS `F`, never a
  `head -n 40` cut) and the deleted-but-open holders (`ro_window_holder_probe_cmd`: `lsof +L1`, or --
  a cambox has no lsof -- the same columns built from /proc), then one summary line
  (`ro_window_holders`), and exit non-zero. **A failed close starts nothing: the relay is left
  STOPPED and says so** (issue 1407 reversed the old "the relay restore still runs").
  The /proc fallback must scan `/proc/<pid>/exe` (a replaced RUNNING binary -- the 25.9. incident --
  is held there, lsof `txt`) and `/proc/<pid>/maps` (`mem`), not only `fd/*`; its PROC_ROOT argument
  lets a test plant a fake tree (substituted with bash `${body//__PROC__/$root}`, never sed). It
  drops `/memfd:`, `/dev/shm/`, `/SYSV` noise (always "(deleted)", never holds /) so it cannot crowd
  the real holder out of the 40-line cap, and turns spaces in a process name into `_` so the lsof
  columns hold. The close exits only 0 or 1, so any other rc (ssh 255, sshpass 5/6) is reported
  as an ssh failure, not a busy mount.
- **Run resolution is ONE shared resolver:** `scripts/lib/ci-run-resolve.sh`
  `ci_run_latest_success REPO BRANCH WORKFLOW ARTIFACT [LIMIT=100]`, used by the relay deploy,
  `deploy-fleet.sh`, the setup-device relay `latest` plan and (since #1394) setup-device STEP 3 /
  STEP 3b's camera-box + probe-tools lookups and the strih service deploy
  (`bkshading-deploy-service.sh`). The relay still follows the SAME run STEP 3 picked.
  - **Contract: the workflow runs on EVERY push of the branch** (no `paths:` filter). ci.yml does.
    A path-filtered workflow (linux-genlock.yml) leaves most heads with no run, which the resolver
    reads as a stale listing and refuses -- so `setup-imag.sh`'s linux-genlock lookup keeps its own
    query, and the parity-align libs stay commit-scoped (`--commit <sha>`). Never point this
    resolver at a path-filtered workflow.
  - **It anchors on the branch HEAD (#1394).** The 30.9.2026 recurrence: a relay deploy to
    cam4-cam7 got the 4.9. run 33857572305 while the main head 1f7e6569b had its own success run
    36687519583. The filtered runs listing itself was STALE (newest success 15.9., artifact
    expired), and the same query read correctly a minute later, so no client-side sort can fix it:
    the run is absent from the payload. The resolver reads the head sha from the branch REF
    (`gh api repos/R/branches/B --jq .commit.sha`, strongly consistent), then the head commit's own
    runs (`gh run list --commit <head>`); a success carrying the artifact is the pick, logged
    `head-anchored`. The branch listing is read ONLY when that lookup has no run for the head (the
    stale check) or the head's run is not usable (the fallback), so a listing hiccup never refuses
    a build already known good.
  - **No run for the head in the head lookup NOR the branch listing = a stale listing.** It
    re-reads both `CI_RUN_RESOLVE_RETRIES` times (default 3, `CI_RUN_RESOLVE_RETRY_S` 10 s between,
    the sleep injectable via `CI_RUN_RESOLVE_SLEEP`), then fails loud naming the head, "stale GitHub
    listing" and `pass --run <id>`. ci.yml runs on every push to dev and main, so a head with no run
    anywhere is a stale listing or a run seconds from being created -- never a reason to deploy an
    older build silently.
  - **The head's run queued / in progress / failed / without the artifact:** the older-success walk
    (success newest first by createdAt, client-side, NO server-side `--status` filter, artifact
    present + non-expired), with ONE loud `FALLING BACK` line naming the head's state and the
    fallback run's id, date and sha.
  - **The walk only uses a listing read that itself holds a run of the head** (review round 1). A
    listing without one is the stale set even when the head lookup already shows the head's run in
    flight -- walking it redeployed the 4.9. run in the review's replay. It is re-read within the
    same retry budget, then refused; a listing from an earlier read is never walked.
  - **Fail loud, never a fallback:** a gh error on the head read (or a head that is not a 40-hex
    sha), on the head's run lookup, on the listing, or an UNREADABLE artifact list
    (`ci_run_has_artifact` rc 2).
  - Every JSON step goes through gh's BUILT-IN `--jq` (`ci_run_newest_success_filter`,
    `ci_run_rows_of_sha_filter`, `ci_run_listing_filter`): a cambox has gh but no jq.
  - Tests: `tests/python/test_ci_run_resolve_1394.py` (a fake gh that replays listing SNAPSHOTS, so a
    stale-then-fresh listing is a real case, plus a fake sleep; it also runs setup-device's own call
    lines against the fake gh on PATH) and the issue-808 cases in
    `tests/python/test_bkshading_relay_gaps_808.py`. A fake gh for this lib must answer the branch
    head with a full 40-hex sha and filter `run list --commit`. `CI_RUN_RESOLVE_RETRIES` is read as
    decimal (`08` is a count, never octal).
  - setup-device's old inline `gh run list --status success --limit 1 -q '.[0].databaseId // empty'`
    was pinned by `tests/setup_device_fleet_binary_ndi.rs` + `setup_device_provisioning_defects_1066.rs`;
    those tests now pin the lib call and the ABSENCE of `--status success`. The `no successful CI run
    found on branch` fail wording stays (`setup_device_provisioner_hardening.rs` pins it).

## LAN HTTPS front — PRIMARY full-PWA path (issue 808, owner ruling 17.9.2026)

The owner ruled 17.9.2026: HTTPS **áno, ale cez lokálnu sieť, nie cez internet** — a trusted-cert
HTTPS origin, but traffic stays on the LAN, never through the internet. So the full PWA install
from any LAN PC/mobile now comes from a **dev1 nginx TLS reverse proxy on a PUBLIC DNS NAME that
resolves to dev1's PRIVATE LAN IP** — never the cloudflared tunnel.

- **Topology:** browser (strih/stream/mobile on the LAN) → DNS `shading.newlevel.media` =
  `10.77.9.200` (dev1 LAN, **DNS-only / not proxied**) → dev1 nginx `:443` (Let's Encrypt cert via
  DNS-01) → `http://strih.lan:8770` (the bkshading panel). Traffic never leaves the LAN; the
  internet is used **only** for the DNS lookup and the ACME cert renewal (the rig on an event has
  mobile data for those).
- **Reproduce it on dev1:** `scripts/dev1-shading-https-install.sh --install` (idempotent) and
  `--check` (report-only: packages, DNS answer, cert presence, nginx site enabled + `nginx -t`,
  `curl` 200 on the manifest). The committed nginx site is `scripts/nginx/shading.newlevel.media.conf`
  (== the lib's default render — a drift-guard test pins the equality). The pure decisions (site
  render, certbot argv, deploy hook, cloudflare.ini, --check verdict) live in
  `scripts/lib/shading-https.sh` and are tested by `tests/python/test_shading_https_install_808.py`.
  Flags: `--hostname` / `--upstream` / `--lan-ip` / `--email` (live defaults baked in), `--dry-run`
  (rehearse the DNS record step). The installer needs privilege for `/etc` writes; the owner/
  supervisor runs it on dev1 (never a lane worker).
- **Cloudflare pieces (dev1, NOT committed):** the DNS A record is created via the airuleset
  `cli_cloudflare_dns` API client (zone token `~/.secrets/cloudflare-newlevel`); the certbot DNS-01
  credentials live in `/etc/letsencrypt/cloudflare.ini` (root:600, token copied from that secret
  file — NEVER printed, NEVER committed); `certbot.timer` renews and the deploy hook
  `/etc/letsencrypt/renewal-hooks/deploy/nginx-reload.sh` reloads nginx.
- **GOTCHA — negative DNS cache:** never query the public name BEFORE the A record exists. A failed
  lookup poisons resolver negative caches for the zone SOA minimum (**1800 s**). `--install` creates
  the record FIRST; run `--check` (which queries) only after.
- **GOTCHA — h2 vs WebSocket:** the panel pushes live state over WS, and browsers speak WS over
  **HTTP/1.1** (the proxy uses `proxy_http_version 1.1` + `Upgrade`/`Connection` passthrough; the
  supervisor's live dev1 stand-up confirmed `/ws` → 101). Over HTTP/2 the `Upgrade` handshake is
  rejected 400 — so a raw `curl --http2` WS
  probe returns 400 while a real browser (which negotiates 1.1 for the WS) works. Test WS with an
  HTTP/1.1 client, not `curl` over h2. nginx 1.24 has NO `http2 on;` directive — the http2 flag rides
  on `listen 443 ssl http2;`.
- **Rejected alternative — cloudflared tunnel** (`scripts/bkshading-provision-cloudflared.sh`, kept
  for reference): the owner rejected any internet path 17.9.2026 (every request would leave the LAN
  and come back). The provisioning script + its systemd unit stay in the repo as the optional
  alternative, but the LAN HTTPS front above is the canonical path.


## Relay + service LOGGING is on by default, and gphoto2 shell-outs are BOUNDED (issue 1309, 15.9.2026)

The 2026-09-13/15 half-dead-cambox class lands right after bkshading-relay activity, and each time
it was undiagnosable: the relay emitted ZERO journal lines on a RUNNING unit (`journalctl -u
bkshading-relay -b` = 0 — its subscriber defaulted to info but nothing logged per command), and the
strih service logged NOTHING for a `PUT /api/params` while spamming `WARN relay unreachable` every
2 s (365 KB/run). The 15.9. escalation was "shading crashol po dvoch zdvihnutiach clony" — two
aperture SETs = two gphoto2 shell-outs. The fix makes the whole chain reconstructible + bounded:

- **The relay logs EVERY gphoto2 command at info through ONE centralised seam** `Gphoto2Cli::run`
  (`shading:bkshading/relay/src/transport.rs`): kind (detect/read/set), param + value, the exact argv, rc,
  duration_ms, and on failure the stderr tail — one structured line per command. Startup logs the
  camera-detect result; `note_online_transition` logs one line per camera online/offline FLIP. The
  `Gphoto2Runner` TRAIT is deliberately untouched (the logging + timeout live only in the real
  `Gphoto2Cli`), so every fake-runner unit test stays green in shape. Reads stay cached (issue 1229),
  so info volume is ~3 lines per real read cycle (once per 10 s floor), never per-poll.
- **gphoto2 shell-outs are BOUNDED.** `run_with_timeout` gives every command a hard 8 s timeout
  (`GPHOTO2_TIMEOUT`) that KILLS + reaps the child (stdout/stderr drained on threads so a large
  output can't deadlock the wait) — a gphoto2 hung on a busy USB-PTP device can no longer hold the
  serialized camera lock forever. SET and READ already serialise through the one `read_cache` mutex
  (no parallel fork); on top of that a pure single-flight `SetQueue` (`bkshading-proto`) coalesces a
  burst of SETs latest-wins: the first runs, concurrent SETs fold into `pending`, the in-flight
  worker drains the coalesced latest — so "raising the aperture twice" NEVER forks a second gphoto2.
  `apply()` is unchanged (existing `apply(&req)==count` tests stay green); the HTTP handler calls the
  new `submit()`, which returns `Applied(n)` or `Coalesced` (`202 {"coalesced":true}`).
- **The service logs every `PUT /api/params`** (`forward_set`: id/params/status/latency_ms, error
  body on failure), and the per-poll `relay unreachable` WARN is DOWNGRADED to `debug`; reachability
  is now logged ONCE per transition (`monitor::reach_transitions`, mirroring `fps_alert_transitions`)
  + a 5-min heartbeat count in the pump — so a chronic-down relay is visible without the spam.
- **verify-device `(am)`** HARD-FAILs a box that RUNS the relay if the LIVE unit's `TasksMax` > 512
  (the #1309 blast-radius ceiling, already in the unit) OR an ACTIVE relay emits zero info journal
  lines this boot (an old binary predating info-by-default logging). Relay absent = `na`;
  disabled/inactive (the TEST-mode default per the lifecycle section above) skips the log assertion.
  Pure `relay_blast_radius_verdict`/`relay_tasksmax_within_ceiling`, inserted BEFORE `(q)`.
- **rig-dev-handover-check** gains a `shading` item (`rig_dev_handover_decision.py` + a read-only
  per-cambox ssh+curl probe in the orchestrator): SHADING-ON (enabled+active+online) = OK,
  SHADING-DEAD (enabled but crashed) = FORGOT (the crash class), SHADING-OFF (disabled = the dev
  default) / SHADING-NO-CAMERA / SHADING-UNREACHABLE = neutral → UNKNOWN. So "shading is off on
  cam1" is reported, never discovered, and an all-disabled dev fleet never false-forgots.

## Service redeploy on strih: the installer's start dies with the ssh session — relaunch via the keep-alive task (16.9.2026)

`scripts/bkshading-deploy-service.sh --execute` copies the exe, registers the `bkshading-service` keep-alive task and starts the service from the ssh session; Windows OpenSSH tears that process down at disconnect (the same job-object physics as OBS-over-ssh, issue 859), so 60 s later there is no process and no `:8770` listener while the script printed `OK … Listening`. Recovery = `schtasks /run /tn bkshading-service` over ssh (session-agnostic; the task launches the exe detached) → `keep-alive: bkshading service was not running - relaunched`; verify from dev1 with `curl :8770/api/version`. The relay on a cambox is unaffected (systemd unit). Followup for the script: start via the task, verify from a SECOND connection.

