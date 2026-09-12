---
paths:
  - "scripts/lib/obs-fleet.sh"
  - "scripts/audio-lag-alert-watchdog.sh"
  - "scripts/av-step-alert-watchdog.sh"
  - "scripts/bundle-state-alert-watchdog.sh"
  - "scripts/network-reach-alert-watchdog.sh"
  - "scripts/vb-matrix-alert-watchdog.sh"
  - "scripts/obs-liveness-watchdog.sh"
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
  4-field shape): `audio-lag`/`vb-matrix`=strih,stream; `av-step`=stream; `bundle-state`/
  `network-reach`/`obs-liveness`=strih,stream,resolume. resolume is EXCLUDED from audio-lag/av-step
  (no mbc audio on the CG box) and vb-matrix (no VB-Matrix install).
- **`obs_fleet_is_home <name>`** — the traveling-box gate. `always` boxes (strih/stream/imag) are
  unconditionally home. A `traveling` box (resolume) is home iff it resolves AND its OBS-WS
  (`OBS_FLEET_HOME_PORT`, default 4455) answers. **NOT the ticket's `:8898`/dantesync example —
  RESOLUME-SNV carries no dantesync (issue 811), so :8898 would be inert; OBS-WS :4455 is the honest
  "home + serving" signal** (resolume runs a genlock cg-obs). The `OBS_FLEET_HOME` env force-list
  (space-separated names) short-circuits the live probe for tests/ops and keeps the offline tests
  offline.
- **`obs_fleet_host`/`obs_fleet_class`/`obs_fleet_home_check <name>`** — plain fact lookups.

## Invariants that bite

- **Every facet default MUST stay byte-for-byte identical to the legacy literal** for the three
  pre-#1296 facets, or an env-override test breaks. `obs_fleet_boxes audio-lag` ==
  `strih|10.77.9.202 stream|10.77.9.204`, etc. Pinned by `tests/harness_obs_fleet_list_1296.rs`.
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

## Scope beyond the watchdogs

- `scripts/version-integrity-gate.sh` — `--win-state-report-only NAME=FILE` surfaces RESOLUME-SNV as
  an informational row that NEVER enters the pass/fail roll-up (it stays OUT of the `[0/8]` blocking
  set; `targets.md`). Pinned by `tests/version_integrity_gate.rs`.
- `scripts/rig-health-audit.py` — `grade_resolume_bundle`/`check_resolume` render resolume's genlock
  build + bundle-state facets WHEN PRESENT (omitted when away), never graded for rate (the issue-787
  exemption stays), never FAIL/WARN. Pinned by `tests/python/test_rig_health_audit_resolume_1296.py`.
- The ON-BOX `:8899` BundleStateServer install on RESOLUME-SNV (Scheduled Task) is a SUPERVISOR rig
  step — see the `resolume-snv` section of `.claude/skills/ops/SKILL.md` + `targets.md`.
