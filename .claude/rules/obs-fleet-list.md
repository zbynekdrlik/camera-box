---
paths:
  - "scripts/lib/obs-fleet.sh"
  - "scripts/lib/strih-platform.sh"
  - "scripts/obs_fleet_table.py"
  - "tests/python/test_strih_windows_remnants_1317.py"
  - "scripts/audio-lag-alert-watchdog.sh"
  - "scripts/av-step-alert-watchdog.sh"
  - "scripts/bundle-state-alert-watchdog.sh"
  - "scripts/network-reach-alert-watchdog.sh"
  - "scripts/vb-matrix-alert-watchdog.sh"
  - "scripts/obs-liveness-watchdog.sh"
  - "scripts/genlock-lock-alert-watchdog.sh"
  - "scripts/render-freeze-alert-watchdog.sh"
  - "scripts/audio-mixer-alert-watchdog.sh"
  - "scripts/obs-handles-alert-watchdog.sh"
  - "scripts/dantesync-clock-alert-watchdog.sh"
  - "scripts/obs-session-watchdog.sh"
  - "scripts/obs-burn-reconcile-watchdog.sh"
  - "scripts/rig-restore-watchdog.sh"
---

# OBS_FLEET — the ONE declared list of managed broadcast-OBS boxes (#1296)

`scripts/lib/obs-fleet.sh` is the single source of truth for the managed broadcast-OBS fleet the
dev1-side mechanisms watch + version-check — the direct analogue of `scripts/camera-set.sh` /
`CAMERA_ACTIVE_SET` (`.claude/rules/camera-active-set.md`) for the OBS boxes. Before #1296 the fleet
membership was duplicated as SIX independent literals (the five `BOXES="${X_BOXES:-…}"` defaults in
`audio-lag`/`av-step`/`bundle-state`/`network-reach`/`vb-matrix-alert-watchdog.sh`, plus
`obs-liveness-watchdog.sh`'s hardcoded `STRIH_HOST`/`STREAM_HOST` `--box` pair), so registering a new
box (RESOLUME-SNV) meant editing six files. Now every watchdog DERIVES its default from the lib and a
new box is ONE table edit.

## The table + the three entry points

`OBS_FLEET` is a `name|host-or-ip|class|home-check` row per box (env-overridable as a whole for
tests/ops). Three helpers consume it:

- **`obs_fleet_boxes <facet>`** — PURE policy, emits the `name|host name|host …` pairs each watchdog's
  `BOXES=` consumes (`for pair in $BOXES; do box="${pair%%|*}"; ip="${pair##*|}"`). The facet→member
  policy is decided HONESTLY per each watchdog's own header, NOT per-entry (so the row stays the fixed
  4-field shape) — the per-facet table is in the issue-1317 section below. resolume is EXCLUDED from
  audio-lag/av-step (no program audio on the CG box) and vb-matrix (no VB-Matrix install).
- **`obs_fleet_is_home <name>`** — the traveling-box gate. `always` boxes (strih-snv/stream) are
  unconditionally home. A `traveling` box (resolume) is home iff it resolves AND its OBS-WS
  (`OBS_FLEET_HOME_PORT`, default 4455) answers. **NOT the ticket's `:8898`/dantesync example —
  RESOLUME-SNV DOES run dantesync 1.8.54 (:8898 answers whenever the box is up, supervisor read-back
  2026-09-12), but :8898 alone would read "home" with the cg OBS down; OBS-WS :4455 is the honest
  "home + serving" signal** (resolume runs a genlock cg-obs; the watchdogs need the OBS, not just the box). The `OBS_FLEET_HOME` env force-list
  (space-separated names) short-circuits the live probe for tests/ops and keeps the offline tests
  offline.
- **`obs_fleet_host`/`obs_fleet_class`/`obs_fleet_home_check <name>`** — plain fact lookups.

## Invariants that bite

- **Every facet default is byte-pinned** by `tests/harness_obs_fleet_list_1296.rs` (both
  `obs_fleet_boxes <facet>` and each watchdog's SOURCED `BOXES`). A roster change is a deliberate
  test edit: issue 1317 moved `audio-lag` to `strih-snv|10.77.9.202 stream|10.77.9.204` and
  `vb-matrix` to `stream|10.77.9.204`.
- **The env override (`X_BOXES=`) must stay authoritative** — each watchdog writes
  `BOXES="${X_BOXES:-$(obs_fleet_boxes <facet>)}"`, so a test/ops override bypasses the derivation
  entirely (byte-compatible).
- **resolume is a TRAVELING box — never false-page it while away.** The is-home gate is applied by
  the CONSUMER, never baked into the pure list:
  - `network-reach` keeps resolume REPORT-ONLY by default and promotes it to a paging node ONLY while
    `obs_fleet_is_home resolume` holds. The is-home probe is computed ONLY when
    `NETWORK_REACH_REPORT_ONLY_BOXES` is UNSET, so an explicit override wins byte-compatibly AND the
    issue-811 offline test stays offline (no live probe).
  - `obs-liveness` polls resolume ONLY while home (away → not in the `--box` set → no false wedge).
  - `bundle-state` needs no is-home gate — a fully-unreachable box is already deferred to the reach
    watchdog (no page / no restart against a dark box).
- **obs-liveness collision residual (#1296 review, narrow + supervisor-gated).** The is-home gate is
  collision-SAFE for `network-reach` (its promotion AND page condition both key on .201 liveness, so
  they cannot contradict), but `obs-liveness` is a PAGING watchdog whose promotion signal (is_home =
  :4455 answers) DIFFERS from its page condition (render wedged). Because `resolume.lan` currently
  resolves to 10.77.9.201 (= `bridge`), a render-wedged, password-authenticating OBS at .201 that is
  NOT resolume could page "resolume WEDGED". It is NARROW (needs .201 to be an authenticating OBS-WS
  that is render-wedged) and GUARDED: obs-liveness ships DISABLED and the supervisor confirms
  resolume identity (its OBS profile / its own :8899 bundle-state name — never "the shared OBS-WS
  password worked") before enabling (targets.md RESOLUME-SNV checklist). A future
  identity-confirm-before-poll (read resolume's own :8899 profile name) closes it in code.
- **resolume's host is the HOSTNAME `resolume.lan`, not a pinned IP** — it currently resolves to
  10.77.9.201, the SAME IP `bridge` lists in `targets.md` (event-LAN DHCP collision). A report-only
  node tolerates a false reachable; confirm box IDENTITY (`getent hosts resolume.lan` + its OBS
  profile, `.claude/rules/rig-state-inspection.md` §2) before relying on it as a paging node.
- **The lib is source-only (no `set -euo pipefail`, the `# airuleset:script-ok` convention)** — it
  runs in the caller's shell. `obs_fleet_is_home`'s probes are overridable seams
  (`obs_fleet_resolve_host` / `obs_fleet_status_probe`) + the `OBS_FLEET_HOME` force-list, so the
  whole thing is Tier-0 testable offline.

## Box names carry the venue: `<role>-<venue>` (issue 1414, owner 12.10.2026)

Every row name is the box's role + venue, never a platform word: `strih-snv`, `stream-snv`,
`resolume-snv` (SNV), `strih-pp`, `stream-pp` (Poprad), plus `imag`.
The SNV rows were `strih-lx`, `stream` and `resolume` before issue 1414.
Owner (verbatim, issue 1414): "inak tim ze si dal take nazvy ako strih-lx tak ja neveim ci je to
pp ci snv ... a tiez to slovo lx je zbytocne len sposobuje zmatok".

- **The rename's consumers:** every name-keyed arm (bundle-state ssh creds, burn-reconcile / rig-restore
  host overrides, obs-liveness, network-reach + obs-liveness `obs_fleet_is_home resolume-snv`, the
  dantesync-maintenance default box, the cg backstop box, the dantesync fleet rows, the deploy
  planner's `--boxes` vocabulary `strih-snv,stream-snv,imag,resolume-snv`). Watchdog state files and
  alert dedup keys are name-keyed, so the first pass after the rename seeds fresh state (a one-off).
  The obs-session watchdog's generic overrides follow the name: `STREAM_SNV_HOST` / `RESOLUME_SNV_PW`.
- **ONE declared transition alias table** (issue 1414: `OBS_FLEET_ALIASES` = `strih-lx=strih-snv
  stream=stream-snv resolume=resolume-snv`; the python twin `obs_fleet_table.FLEET_ALIASES`, pinned equal): consulted
  by `obs_fleet_name_for_host` after the exact match (also as a DNS name's first label), and by
  `obs_fleet_canonical_name` (case-folded), and by `_obs_fleet_entry`, so every fact lookup
  (`obs_fleet_host` & co) and every `--box` CLI takes an old name. An exact row always wins, and an
  alias whose renamed row is not in the table is no alias (a test override keeps its own rows). The
  box-facts loader reuses this table (`strih_box_alias_of`). The retired Windows `strih` is no alias.
  The role tokens below resolve through it, so removing an alias means migrating them first.
- **Role tokens are NOT fleet names and stay as they are:** a plan prints the role (`${box%%-*}`) to
  the role tools (`latency_pins_verify.py`, `mark-imag-restart.sh`); `launch-obs-genlock.sh --box stream`,
  `obs-self-heal-install.sh --box stream` (its scheduled-task name `camera-box-obs-self-heal-stream`
  lives on the box), the `strih` / `stream` labels of the non-fleet watchdogs (frozen-input,
  ndi-halving, asio-starve, cadence, mv-fps), `obs_phase2.py --role`, the cg-chain hop names.
- **The forced-table audit keys on the box CLASS** (`resolume` / `strih` / `stream` / `imag`, the
  Rust table): the deploy preflight passes `${box%%-*}`, the role before the venue.
- **The Windows hostnames** (`STREAM`, `RESOLUME-SNV`) and the MCP server names are a separate live
  step; the repo's `linux-strih-snv` MCP name expects the dev1 MCP entry renamed.

## The production strih is strih-snv at .202 — the Windows strih row is gone (issue 1317, M4)

Since the M4 cut-over (20.9.2026) the Linux notebook strih-snv IS the production strih. Its row is
`strih-snv|10.77.9.202|linux-genlock|always`: it dials the IP (a fixed box; the rig DNS answers
`strih-snv.lan` and the old `strih.lan` with .202 since 13.10.2026, `strih-snv.local` via mDNS). The
old `traveling` + `.lan` row read the production strih as AWAY, so every dev1 watchdog was blind to
it (found by the issue-1363 lane).

- **The Windows `strih` row is REMOVED, not flipped to `retired`.** `retired` (below) is for a box
  whose ROLE returns on new hardware at the same row. Here the role already lives on strih-snv, and
  the old row's address .202 now belongs to strih-snv, so keeping `strih|10.77.9.202|windows-genlock`
  would hand a Linux box to any Windows-class caller naming `strih`. `obs_fleet_host strih` now fails
  closed like any unknown name. Its history lives in `targets.md` (RETIRED 20.9.2026 M4).
- **strih-snv keeps its own NAME** (the design's rejected Approach 3 renamed it to `strih`): state
  files and alert dedup keys are name-keyed, so the new machine never inherits the Windows box's
  confirm counters, baselines or dedup keys.
- **Static-literal consumers naming the box were edited in the same change:** the
  `dantesync-clock` default `DANTE_CLOCK_OBS_NODES` = `strih-snv stream resolume`, and
  `obs-liveness-watchdog.sh`'s per-box arm is `strih-snv)` (STRIH_HOST default .202 + the 30 fps
  target).

Per-facet decision (by each facet's PREMISE — a platform-neutral read joins, a Windows-only one does not):

| Facet | Members | Why strih-snv is in or out |
|---|---|---|
| network-reach | strih-snv stream resolume | ping / OBS-WS :4455 / :8899 — platform-neutral |
| bundle-state | strih-snv stream resolume | curl :8899 is neutral; the AUTO-RESTART is class-resolved (guarded `systemctl --user` on linux-genlock, `schtasks` on windows-genlock — the dev1 watchdog AND the E2E `[0/8]` self-heal, `.claude/rules/bundle-state-watchdog.md`) |
| obs-liveness | strih-snv stream resolume | OBS-WS GetStats — neutral; the strih-snv arm keeps `STRIH_HOST`/`STRIH_TARGET_FPS`, and the alert's recovery is class-resolved (`recovery_plan_for`: plain-ssh `systemctl --user restart '*-obs.service'` = strih-obs.service on a linux-genlock box, the `launch-obs-genlock.sh` + win-* MCP plan only on windows-genlock — that planner refuses a Linux box name) |
| genlock-lock | strih-snv stream imag resolume strih-pp stream-pp | the box's own :8899 `genlock_lock` facet; imag `retired` → dropped; the PP boxes since issue 1441 |
| render-freeze | strih-snv stream resolume strih-pp stream-pp | :8899 `program_render_lagged` / `relock_bursts`; PP since issue 1441 |
| audio-mixer | strih-snv stream resolume strih-pp stream-pp | issue 1381 (PP since issue 1441): :8899 `audio_mixer_*` (every OBS box's libobs mixer) + `vban_pacer_*` (resolume's obs-vban senders); the consumer polls resolume only while home (`obs_fleet_poll_now`) |
| obs-handles | strih-snv stream resolume strih-pp stream-pp | issue 1406 (PP since issue 1441): :8899 `obs_handles*` -- the OBS process's handle count (Windows) or open-fd count (Linux `/proc`); every managed OBS can leak; resolume polled only while home (`obs_fleet_poll_now`) |
| audio-lag | strih-snv stream | :8899 `audio_ts_lag_*` from the vendored OBS `audio-telemetry #800` lines — the same on Linux (reads UNKNOWN while no source carries audio, never a page) |
| vb-matrix | stream | a Windows VB-Audio Matrix process check; strih-snv has none (PipeWire replaced it, issue 1344) |
| av-step | stream | the av-sync dock is on the stream box only |
| ndi-portmap | strih-snv | the ONE strih whose NDI sender port map is watched (issue 1363) |
| obs-session | strih-snv stream resolume | the #979 operator-visibility watchdog; the consumer picks the probe by CLASS (below): windows-genlock = the obs64/AHK SESSION-0 PowerShell probe over `win_ssh_run`, linux-genlock (issue 1474) = the `[0/8]` Linux check (`strih-obs.service` over plain ssh + the bounded obs-websocket round-trip); the PP boxes stay out |
| win-baseline | the windows-genlock rows of `stream resolume` (resolume today) | issue 1357/1474: the Windows OBS-box baseline (`win-baseline-check.sh`); `_obs_fleet_windows_rows` keeps only the windows-genlock rows, so stream-snv (Linux since issue 1462) is out and a Windows stream box joins again |
| burn-reconcile | strih-snv stream | the #1060 fresh-OBS-start burn reconcile — OBS-WS GetStats + `obs_burn_filter` sweeps, neutral; resolume OUT (its cg-OBS burn is the opt-in CG_CHAIN profile) |
| rig-restore | strih-snv stream | the #281 stranded-rig restore — the E2E harness's two program boxes, `obs_phase2.py` program-scene/teardown over OBS-WS, neutral |
| build-parity | strih-snv stream-snv resolume-snv strih-pp stream-pp resolume-pp | issue 1426: ONE OBS genlock build across BOTH venues -- `scripts/fleet-parity-alert-watchdog.sh` reads each box's `:8899` `genlock_build_sha` (an away traveling box = AWAY, never a page), plus `dantesync-version-gate.sh --fleet`, and catches a returning dantesync node up (`dantesync-fleet-upgrade.sh --fleet`) only while the rig lease is free and no home OBS box streams/records; pages a confirmed DRIFT under the stable key `fleet-parity`; ships DISABLED |
| ndi-sender | strih-snv stream resolume | issue 1342: the managed OBS boxes that PUBLISH NDI outputs. Every OBS-box receiver and laptop lists them -- and ONLY them, never a cambox (issue 1389: a finder's discovery connection into a cambox aborts camera-box) -- in `ndi-config.v1.json` `networks.ips`, generated by `scripts/lib/ndi-discovery.sh`; the camboxes themselves carry no list (mDNS only, ROZHODNUTÉ 5879261962). The traveling resolume hostname is resolved at write time. Not a watchdog facet — nothing polls or pages from it (`.claude/rules/ndi-discovery.md`) |

- `obs_fleet_facet_members ndi-portmap` = `strih-snv`: `scripts/ndi-portmap-audit.sh` consumes the
  member NAME only and refuses any member count other than one. It derives the sender prefix from the
  name and reads the IP from the anchor's own mDNS record. Details: `.claude/rules/ndi-portmap-watchdog.md`.
- **The per-box watchdogs that used to dial a literal `strih` now derive from the list too (issue
  1317 part 2).** `obs-session-watchdog.sh` (ENABLED on dev1), `obs-burn-reconcile-watchdog.sh`
  (ENABLED) and `rig-restore-watchdog.sh` each read `<X>_BOXES="${OVERRIDE:-$(obs_fleet_boxes
  <facet>)}"` (overrides `OBS_SESSION_WATCHDOG_BOXES` / `OBS_BURN_RECONCILE_BOXES` /
  `RIG_WATCHDOG_OBS_BOXES`) and loop the `name|host` pairs; the old per-box host knobs stay
  per-NAME overrides (`STRIH_HOST` now repoints strih-snv, `STREAM_HOST` the stream box). The session
  watchdog's host/ssh-credential overrides are GENERIC per fleet name (`<NAME>_HOST` / `<NAME>_USER`
  / `<NAME>_PW`, default the roster host + targets.md `newlevel/newlevel`), so a new windows-genlock
  fleet row is watched with no code edit; its AHK flag is the ONE fact `obs_fleet_has_ahk` (also
  read by `scripts/lib/genlock-fleet-boxes.sh`'s `fleet_box_has_ahk`). The old literal `strih` arm of the
  session watchdog handed the Linux strih-snv the Windows PowerShell probe and logged `strih: ERROR:
  no probe output` on every 5-min pass.
- **rig-restore's UNREADABLE count is roster-relative and FAIL-CLOSED** (`rig_obs_unreadable_count`):
  an EMPTY OBS roster (a mis-set `OBS_FLEET`, a failed roster mktemp) counts 1, so a pass that saw
  no OBS box never clears the E2E marker — the #353 masking-bug class the old fixed `2 - seen`
  arithmetic guarded. `fleet_box_ip strih-snv` likewise fails closed (rc 2, no output) with no row.
- **The obs-session watchdog is ENABLED and now pages resolume while home** — the #1296 `.201`
  `bridge` collision residual applies to it exactly as to obs-liveness (promotion = :4455 answers,
  page = obs64/AHK in session 0 over ssh). Identity read 23.9.2026: `resolume.lan` → 10.77.9.201 =
  `resolume-snv.lan`, ssh `hostname` = `resolume-snv`, AHK watcher seen in session 1. Re-confirm when
  the event-LAN DHCP lease moves.
- **The probe is CLASS-picked in the consumer, not only by facet policy (issue 1474).**
  `obs_session_targets` prints `name host linux` for a linux-genlock member (the `[0/8]` Linux check,
  split into `strih_linux_visibility_probe` + `strih_linux_visibility_ws_ok` in
  `scripts/lib/strih-platform.sh` and graded by `strih_linux_visibility_message`) and the Windows AHK
  mode for a windows-genlock one, and skips (and logs) any other class -- an unknown name in an
  override too -- so a Linux box can never be handed PowerShell even if someone widens the facet. An
  empty Linux probe is connectivity (the watchdog's rule). **One outage pages once:** a dead
  obs-websocket is obs-liveness's page (enabled, same boxes, same unit-restart recovery), so the
  Linux arm decides nothing then and pages only OBS answering while the unit is not active (an OBS
  outside its supervisor), with the plain-ssh `systemctl --user restart strih-obs.service` recovery;
  the Windows alert text is unchanged. `TimeoutStartSec=150` (two hung Linux boxes + resolume). Test
  seams: `OBS_SESSION_OBS_PHASE2_DIR` (a stub `obs_phase2.py`), `OBS_SESSION_LINUX_SSH_TIMEOUT`.
  Same shape for any future per-class consumer: facet policy decides membership, the class picks
  (or refuses) the probe. Tests: `tests/python/test_obs_session_linux_1474.py`.
- **`obs_fleet_poll_now <name>` is the traveling/retired gate for a NEW per-box loop** (the three
  issue-1317 part-2 consumers use it; obs-liveness / network-reach predate it and still call
  `obs_fleet_is_home` for resolume themselves). `always` →
  poll WITHOUT consulting `obs_fleet_is_home` (so the `OBS_FLEET_HOME` force-list test seam never
  drops a fixed box), `traveling` → only while home, `retired` → never, a name with no row (an ops
  override) → polled as given. Use it instead of re-deciding per watchdog. In rig-restore an away
  member is not probed and so is never counted UNREADABLE (which would hold the E2E marker) --
  unless EVERY member is away (only via an override): nothing observed = counted unreadable, the
  marker is kept (fail-closed).
- **State keys follow the fleet NAME.** The burn-reconcile baseline moved from `strih_rtf` to
  `strih-snv_rtf`; its "unknown previous baseline is NOT a restart" rule makes the first pass a
  seed-only NOOP, never a false sweep. The session watchdog's alert dedup is `obs-session-<name>`.
- **Live proof (23.9.2026, read-only `--dry-run`, scratch state files, cams blackholed):** session
  roster `stream|10.77.9.204 resolume|resolume.lan` — stream + resolume `OBS_SESSION=1 AHK_SESSION=1
  wedged=0`, no probe aimed at .202; burn-reconcile `strih-snv: renderTotalFrames … cur=430270`;
  rig-restore `obs strih-snv (10.77.9.202): program scene='Cam 3'`.
- **The strih-snv DIAL default elsewhere is the fleet host too.** `fleet_box_ip strih-snv`
  (`deploy-genlock-fleet.sh`, `STRIH_LX_IP` override) and `strih_lx_host` (`strih-provision.sh`,
  `STRIH_LX_HOST` override, lazy-sources this lib) return `obs_fleet_host strih-snv`, never a DNS
  name. The box's OWN hostname is `strih_lx_hostname` (`strih-snv`) —
  setup-strih no longer cuts it out of the dial address (an IP would have renamed the box `10`).
- **issue 1361: the strih row is PINNED to the strih box fact file, not computed from it.**
  `scripts/strih-boxes/<box>.env` holds each strih's `STRIH_HOSTNAME` + `STRIH_IP`; this table keeps
  its literal row (the python twin parses the literal default block, and every watchdog sources this
  lib — computing the row from the fact files would make them all depend on the fact loader). The
  loader refuses a box whose row host (an IP) differs from `STRIH_IP`, and
  `tests/strih_box_facts_1361.rs` pins `obs_fleet_host strih-snv` == the fact. A new strih (strih PP)
  gets its row + facet memberships here at go-live; before that `strih_lx_host` dials its fact IP.
- **The Windows-strih planner arms are RETIRED too (issue 1317 part 3).** `deploy-genlock-fleet.sh`
  (default fleet `strih-snv,stream`; its strih-snv plan is the setup-strih.sh recipe, never the imag
  program), `launch-obs-genlock.sh` + `obs-self-heal-install.sh` (`--box strih` refused by name,
  pointing at `strih-obs.service`) read ONE per-box table, `scripts/lib/genlock-fleet-boxes.sh`
  (MCP / address / AHK identity / keep-alive tasks; addresses + the AHK fact from THIS list).
  `obs_fleet_has_ahk strih` is now `0`. The retention drivers + `bkshading-deploy-service.sh` have NO
  default box and refuse a linux-genlock address; `recording-verdict-on-strih.sh` requires
  `STRIH_BOX` and refuses a `strih_platform=linux` target.
- **`obs_fleet_class_for_host <host-or-ip>` + `obs_fleet_refuse_linux_target <host> <tool>` are the
  ONE class gate a Windows-only dev1 tool calls before touching an ADDRESS** (issue 1317 part 3).
  The host field OR the name matches (`10.77.9.202` and `strih-snv` → `linux-genlock`); the refusal
  is rc 1 + a named stderr line; an address the list does not know PASSES (an explicit ops target
  stays authoritative). Any new PowerShell / schtasks / `C:\` tool that takes a `--host` calls it
  first -- never a hard-coded "not .202" check.
- **ONE alias-aware authority (issue 1317 part 4): `obs_fleet_name_for_host`.** Both the class gate
  above AND `strih_platform` (scripts/lib/strih-platform.sh, which lazy-sources this lib) resolve an
  address through it, so they can never disagree. Order: (1) exact name/host, case-folded
  (`STRIH-SNV`, `RESOLUME.lan`); (1b) an issue-1414 transition alias (`stream` → `stream-snv`); (2) a
  DNS name's first label vs the row NAME, then vs the aliases (`strih-snv.lan` → `strih-snv`); (3) the
  resolved address (the `obs_fleet_resolve_host` getent seam) vs the row HOST
  fields — this is what catches **`strih.lan`, the retired Windows PC's own name, which the rig DNS
  STILL points at 10.77.9.202** (verified `getent ahosts strih.lan` on dev1, 23.9.2026). An IPv4
  literal is never sent to the resolver (no DNS in the hot path; tests stub `obs_fleet_resolve_host`
  after sourcing; the seam is `timeout`-bounded, `OBS_FLEET_RESOLVE_TIMEOUT`, default 2 s).
  `strih_platform` = linux iff the addressed row's **CLASS** is `linux-genlock` — keyed on the class,
  never the literal name `strih-snv`, so the next Linux strih (strih-pp) is ONE table row with no code
  edit in bash or python. It keeps its env overrides (`STRIH_PLATFORM`, and `STRIH_LX_HOST` now only
  as an explicit extra match — no default). The python twin is `scripts/obs_fleet_table.py`
  (parses the SAME `OBS_FLEET` default block, or the `OBS_FLEET` env) used by rig-health-audit's
  `strih_platform` and the phase/av-sync calibrate push plans; parity pinned by
  `tests/python/test_strih_windows_remnants_1317.py` (runs both sides). Never add a third hand-kept
  "which box is .202" map — extend the table.
- **Live proof (23.9.2026, read-only `--dry-run` sweep):** network-reach `strih-snv (10.77.9.202):
  ping=1 ws:4455=1 bundle:8899=1 -> REACHABLE`; bundle-state `-> HEALTHY`; obs-liveness
  `strih-snv activeFps=30.00 renderAdvanced=True`; genlock-lock / render-freeze / audio-lag /
  dantesync-clock all `reachable=1`. A strih swap (the Poprad strih-pp next) is again a table +
  facet-policy edit here.

## The Poprad rows (issue 1412)

`strih-pp|strih-pp.lan|linux-genlock|traveling` and `stream-pp|stream-pp.lan|linux-genlock|traveling`:
NAME rows (dev1 reaches the PP LAN as 10.76.8.x through the companion-pp subnet router, the boxes
carry 10.77.8.x, so an IP row could never equal `STRIH_IP`). issue 1426 added
`resolume-pp|resolume-pp.lan|windows-genlock|traveling` (the PP CG PC) the same way, and all three
are in `build-parity`. **issue 1441: strih-pp + stream-pp are also in genlock-lock, render-freeze,
audio-mixer and obs-handles** (they serve every :8899 facet those read; read 18.10.2026). Each
watchdog reads a traveling row only while it is home (`obs_fleet_is_home` / `obs_fleet_poll_now`;
render-freeze reads an unfetchable :8899 as SKIP), so a PP box that is off or off the companion-pp
link never pages. The nightly 04:00 date step's 5-min DEGRADED never pages either (5-min pass +
2-pass confirm). bundle-state, network-reach and obs-liveness stay SNV-only: their premises
(Windows schtasks restart, the SNV LAN reference anchors) were not reviewed for PP.
`harness_obs_fleet_list_1296.rs` uses `strih-zz` as its "unknown override" name since strih-pp is
a real row now.

## The last Windows-strih remnants — retired in issue 1317 part 4 (23.9.2026)

Each routes on the ONE resolver above; the Windows branch is kept (a future Windows strih / the
`STRIH_PLATFORM=windows` override) but no longer the default for .202:

- **mv-reverify escalation restart** (`scripts/lib/mv-reverify-escalate.sh`): strih-snv gets a
  BLOCKING `systemctl --user restart strih-obs.service` over plain ssh — see
  `.claude/rules/mv-reverify-escalate.md`.
- **phase/av-sync calibrate push plans**: `.202` → MCP `linux-strih-snv`, destination by fleet class
  (`/home/newlevel/.camera-box/<file>` on a linux-genlock box, `C:\ProgramData\camera-box\…` else).
- **rig-dev-handover-check.sh**: the dantesync version item routes strih through the gate's
  `--linux` arm on strih-snv -- since issue 1372 via the gate's `--fleet` (strih-snv is a `linux` row in
  `scripts/lib/dantesync-fleet.sh`); `strih_dantesync_nodes` lost its only caller and was removed.
  recording-e2e.sh's `[0/8]` gate keeps its inline routing (issue 1351).
- **recording-e2e.sh `[8/8]` text**: banner via `strih_access_label`, planner hand-off via
  `strih_planner_holder_note`, and the pull-back note + all three #652 cleanup plans are a
  `strih_platform` split whose linux branch calls a `strih_lx_*` helper (exact-path `rm -f --` over
  ssh, never a glob; the remote command is ONE double-quoted argument with the path single-quoted
  inside, because the printed line is parsed TWICE — the local paste and ssh's remote shell — and an
  OBS default filename carries a space). **The Windows literals stay INLINE in the else-branch** — Rust static-anchor
  tests pin `FileDownload $STRIH_PIXELS_WIN` and `Remove-Item -Force -LiteralPath '${STRIH_HOST_PATH`
  inside recording-e2e.sh itself, so moving them into a helper would break those pins.
- **AHK defaults**: `ahk_resolve_and_relaunch_ps` has NO default script (rc 2 + named stderr
  without one) and `build_launch_program` defaults to `has_ahk=0`; every box-facing caller passes the
  box's identity from `scripts/lib/genlock-fleet-boxes.sh`.
- **NIC self-heal watcher** (issue 1199: installer, watcher `.ps1`, decision mirror, its pytest, its
  rule): REMOVED with the PC — history only. strih-snv has no on-box self-heal; the dev1 reach
  watchdog is the only outage signal.
- **Still open (followup candidates, not done):** a Linux recordings-retention executor for strih-snv
  `/srv/_REC`; the strih-snv port of the zero-loss restart mode (refused on Linux up front by
  `strih_zero_loss_restart_preflight`; issue 1361 routed its banner, holder and OBS-restart text
  through `strih_access_label` / `strih_obs_restart_hint`, and only the byte-pinned Windows
  pull-back line keeps its win-strih text);
  the strih-snv execute arm of deploy-genlock-fleet.

## Scope beyond the watchdogs

- `scripts/version-integrity-gate.sh` — `--win-state-report-only NAME=FILE` surfaces RESOLUME-SNV as
  an informational row that NEVER enters the pass/fail roll-up (it stays OUT of the `[0/8]` blocking
  set; `targets.md`). Pinned by `tests/version_integrity_gate.rs`.
- `scripts/rig-health-audit.py` — `grade_resolume_bundle`/`check_resolume` render resolume's genlock
  build + bundle-state facets WHEN PRESENT (omitted when away), never graded for rate (the issue-787
  exemption stays), never FAIL/WARN. Pinned by `tests/python/test_rig_health_audit_resolume_1296.py`.
- The ON-BOX `:8899` BundleStateServer install on RESOLUME-SNV (Scheduled Task) is a SUPERVISOR rig
  step — see the `resolume-snv` section of `.claude/skills/ops/SKILL.md` + `targets.md`.

## The `retired` home-check state (issue 1316) — a box that is GONE but whose ROLE returns

imag-nb was returned to the owner 16.9.2026 (10.77.9.182 dark). Its `OBS_FLEET` row is now
`imag|10.77.9.182|linux-genlock|retired` — a THIRD home-check state beside `always`/`traveling`:

- `obs_fleet_is_home imag` → FALSE (a `retired) return 1` arm), so every watchdog that gates on
  `obs_fleet_is_home` (genlock-lock, etc.) skips it — a paging facet can NEVER page the dead box.
- `obs_fleet_boxes <facet>` EXCLUDES a retired member CENTRALLY (a `home-check == retired` skip in
  the loop), so `genlock-lock` drops from `strih stream imag resolume` to `strih stream resolume`
  with NO per-consumer edit. `obs_fleet_facet_members genlock-lock` still LISTS imag (the policy is
  unchanged); the exclusion is the `retired` filter, so re-provisioning is a ONE-WORD flip of the
  row's home-check back to `always` — the box re-enters every facet automatically.
- The ROW + its `obs_fleet_host`/`obs_fleet_class` fact lookups STAY (history + the re-provisioning
  target). Pinned by `harness_obs_fleet_list_1296.rs`
  (`fleet_retired_box_is_never_home_1316`, `fleet_boxes_genlock_lock_excludes_retired_imag_1316`).
- The static-literal rosters that do NOT derive from `obs_fleet_boxes` were edited separately
  (issue 1316): `network-reach`/`bundle-state` `REFERENCE_HOSTS` dropped the dead `.182` anchor;
  `dantesync-clock` `DANTE_CLOCK_OBS_NODES` + `mv-fps` `MV_FPS_BOXES` + `deploy-genlock-fleet`'s
  empty `--boxes` default dropped imag (imag stays a valid EXPLICIT deploy target).
