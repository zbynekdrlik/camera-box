---
paths:
  - "scripts/version-integrity-gate.sh"
  - "scripts/bundle_state_gather.py"
  - "scripts/bundle-state-server.py"
  - "tests/version_integrity_gate.rs"
  - "tests/python/test_bundle_state_gather.py"
  - "tests/python/test_bundle_state_server_log.py"
  - "tests/python/test_bundle_state_server_port4455.py"
---

# version-integrity-gate.sh — the pre-rig-test Windows-stack drift gate (#123/#119)

`scripts/version-integrity-gate.sh` runs FIRST before every rig E2E, reads each Windows box's
observed state (served by `bundle-state-server.py` on `:8899`, gathered by `bundle_state_gather.py`),
and REFUSES the run on DRIFT (exit 20) or UNKNOWN (exit 11). Exit-code roll-up in `main()`:
`bad>0 → 20`, else `unknown>0 → 11`, else `GATE PASS → 0`. It is invoked with `--win-state
"strih=<file>"` + `--win-state "stream=<file>"` (labels are the box `$name`).

## Two-step facet rollout: opt-in (#756-shape) → ENFORCED (#758-shape)

New machine-check facets land **opt-in** first (engage only when the box reports the key) so an
un-upgraded `bundle-state-server` is silently skipped, never a false UNKNOWN. Once the servers are
redeployed to serve the key fleet-wide, a follow-up flips the facet to **enforced** (runs
unconditionally; an absent box → gate-blocking UNKNOWN). `genlock_build_sha` did this #756 → #758;
the #826 obs-identity facet did it #826 → #829.

## Enforced-facet fixtures MUST carry the enforced keys (the with_* injection pattern)

In `tests/version_integrity_gate.rs`, healthy pinned fixtures are built by wrapping the minimal
`STRIH_PINNED`/`STREAM_PINNED` constants at each call site with injection helpers — `with_sha(base,
sha)` (genlock_build_sha) and `with_obs_identity_ok(base, is_strih)` (the #826 keys). When you
ENFORCE a facet, every GATE-PASS test must start carrying its keys or it flips to UNKNOWN and breaks.
`with_obs_identity` (raw pairs) is for the DRIFT/wrong-value tests — inject ONLY the bad key so the
single intended signal is isolated. `state_json_value` is a first-match regex parser, so NEVER
double-inject the same key (baked-in + wrapped) — the first occurrence wins and a wrong-value test
silently reads the healthy value. DRIFT/unread tests that use bare pinned constants still pass by the
`bad>0→20` / `unknown>0→11` precedence even with extra unconditional UNKNOWNs — leave them unless you
need a single clean signal.

## RESOLVED (#1067) — `port4455_owner_path` gather fixed via WMI ExecutablePath, then ENFORCED

**History (#829):** `bundle-state-server.py::port4455_owner()` resolved the :4455 listener's exe
PATH via `Get-NetTCPConnection | Get-Process | .Path`. From the deployed **non-elevated, hidden**
`BundleStateServer` scheduled-task context that was **access-denied** on the elevated OBS process →
`.Path` null → the key OMITTED (omit-when-empty) on the WHOLE live fleet, even though OBS
legitimately owns :4455 at the pinned path (an ELEVATED `win-strih` MCP `Get-NetTCPConnection
-LocalPort 4455 | Get-Process` returned the path fine; a plain `curl :8899/bundle-state.json` did
NOT show `port4455_owner_path`). So `port4455_identity` stayed opt-in behind its own
`if [ -n "$port4455_owner_path" ]` guard — the last obs-identity facet not yet enforced.

**Fix (#1067):** `port4455_owner()` now resolves the path via `Get-CimInstance Win32_Process -Filter
"ProcessId=$id"`.ExecutablePath — the WMI/CIM provider returns ExecutablePath for an elevated
process from a NON-elevated caller where the OpenProcess-based `Get-Process.Path` is denied — with
`Get-Process.Path` kept as a fallback. Then the opt-in guard was REMOVED from
`version-integrity-gate.sh` main(): `port_identity_verdict` now runs UNCONDITIONALLY like
`obs_installs`/`obs_process_count` (empty owner → gate-blocking UNKNOWN). Same 756→758 two-step. The
elevate-the-task alternative was REJECTED (security-boundary escalation of a LAN-facing HTTP task +
a rig redeploy out of a code PR's scope); it stays the documented fallback if a box is ever found
where even CIM ExecutablePath is denied.

**Deploy/verify caveat (still true):** whether CIM ExecutablePath actually reads the elevated obs64
path in the deployed task context is a LIVE-Windows-box property — no worktree worker can verify it.
After deploying the new `bundle-state-server.py` to strih+stream, `curl :8899/bundle-state.json` on
BOTH and CONFIRM `port4455_owner_path`/`_version` now appear BEFORE trusting the enforced gate on a
rig E2E; if still absent, fall back to running the `BundleStateServer` task elevated.

## GOTCHA — strih obs_installs / startup_chain DRIFT is #826's PHYSICAL cleanup, not a code bug

Once strih serves `obs_installs`, the gate flags its 8 `D:\_APPS\_RETIRED_*_2026-07-27` leftover
installs as DRIFT ("renaming aside is not removing"), and `ahk_dead_config_present=1` makes
`startup_chain` DRIFT. Both are #826's remaining PHYSICAL cleanup (delete the retired install folders;
strip the dead `app1_binarypath`/`app2_*` block from `NL_STARTUP.ahk`) — a rig + destructive action,
not fixable in the gate code. They go live under the opt-in code the moment the server serves the key.

## GOTCHA — Tier-0 for these tests: no `# airuleset:build-ok` bypass, cold builds are contended

The `# airuleset:build-ok` bypass is DISABLED for camera-box, so `cargo test` (RUN) cannot execute
locally at all — only `cargo test --no-run` (compile). In a fresh WORKTREE the cold `--no-run` build
(criterion/proptest/etc.) exceeds the ~10-min Bash foreground cap and is heavily CPU-contended by
sibling fleet-worker builds; treat the SUPERVISOR's integration as the authoritative compile+test.
Local Tier-0 you CAN do here: `cargo fmt --all --check`, `bash -n` + `shellcheck` on the `.sh`, and
the Python tests (`python3 -m pytest tests/python/test_bundle_state_*.py` — these DO run locally and
gave a real RED→GREEN for the `log()` dead-stdout fix). `log()` legitimately swallows the dead-stdout
`OSError` (stdout is the broken resource, cannot log it) — bypass-marked `# airuleset:script-ok`.

## #770 — byte-derived DistroAV/libobs parity: gather the DEPLOYED bytes, opt-in

The `[0/8]` byte-vs-manifest COMPARE already lives in the engine (`drift-guard.sh`
`manifest_sha_for_component` #122 / `drift_check_all_files` #121) and the gate already threads the
`obs_dll_sha256`/`distroav_dll_sha256`/`manifest`/`bundle_hashes` `--compare` keys — the missing
piece was the GATHER. #770 added `bundle_state_gather.component_sha256(path)` (chunked sha256, `""`
= UNKNOWN on missing/dir/empty/unreadable — never a false-clean SHA) + `obs_dll_sha256` /
`distroav_dll_sha256` on `build_bundle_state` (omit-when-empty), wired in `bundle-state-server.py`
(`DEFAULT_OBS_DLL` = the pinned install's obs.dll; distroav.dll = the FIRST located copy). This makes
the marker (`GENLOCK_BUILD_SHA.txt`) a POINTER — the truth is the bytes — closing the wrong-direction
#119/#767 hole the marker-only cross-box parity cannot catch. Landed OPT-IN (#756-shape): a box not
yet reporting the SHAs is silently skipped, never a false UNKNOWN. **Still opt-in / dormant in CI
until #1082**: the CI-authoritative `BUNDLE_MANIFEST.json` is not auto-fetched per box yet (only
`VERSION_GATE_MANIFEST=` activates `--manifest`), the imag `.so` ssh byte gather is not wired, and the
ENFORCE flip (#758-shape) is deferred — all in #1082 (needs the live gather deployed + verified first,
a LIVE-Windows property no worktree worker can verify, same class as #1067's port4455 caveat).

## #1082 — imag `.so` byte facet + auto-source the CI manifest per box (parts 1+2; ENFORCE = #1100)

Landed the two deferred #770 halves, still OPT-IN (the ENFORCE flip is #1100 — a follow-up, gated on
the live gather being verified on the rig, which no worktree worker can check).

- **imag byte facet is a TARGETED per-`.so` compare, NOT the whole-bundle walk.** imag is not a
  `--win-state` bundle-state box (its bytes had no path into the gate), and `manifest_sha_for_component`
  resolves only the Windows `obs.dll`/`distroav.dll` basenames. New gate flags `--imag-manifest` +
  `--imag-bytes LABEL=path=sha,…` feed a new `imag_bytes_verdict` that resolves each gathered `.so`
  path (`lib/x86_64-linux-gnu/libobs.so.30`, `.../obs-plugins/distroav.so`, `.../libobs-opengl.so.30`
  — manifest-relative; deployed at `/usr/<path>`) via drift-guard's `manifest_sha_for_path`. Use the
  per-path resolver, NEVER `bundle_hashes`+`drift_check_all_files` — that walks EVERY manifest path
  (~1600) and flips a partial 3-file ssh gather to UNKNOWN. Verdict codes: 10=DORMANT (skip, uncounted),
  20=DRIFT, 11=UNKNOWN, 0=OK.
- **The Windows auto-source MUST be gated on BOTH boxes ALREADY reporting the keys a manifest
  activates.** The gate applies the GLOBAL `--manifest` to EVERY `--win-state` box (strih AND stream),
  and a manifest activates BOTH the `obs_dll_sha256` byte compare AND the `genlock_capability` check on
  each. Supplying it while EITHER box has not yet reported EITHER key flips that box to UNKNOWN
  (drift-guard compares a manifest value vs an empty observed) = a spurious gate-blocking refuse — the
  exact partial-rollout split this opt-in protects against (peer-review 🟡, a strih-only guard was the
  first bug). `recording-e2e.sh` gates the auto-source on `manifest_autosource_state_has_key` for BOTH
  strih AND stream × BOTH `obs_dll_sha256` AND `genlock_capability`.
- **The Windows FAST manifest (`windows-genlock-fast.yml` / `obs-genlock-fast-dll`) is obs.dll-ONLY.**
  Its stage carries only `obs.dll` (+ `BUNDLE_MANIFEST.json`), no distroav.dll. That is SAFE:
  `compare_observed` labels an obs.dll-only manifest's distroav `SKIPPED` (drift-guard l.1877-1884),
  NOT UNKNOWN — so the obs.dll libobs core is byte-verified while distroav stays verified against its
  full bundle in a separate `/drift-guard` run. Do not "fix" the FAST manifest to add distroav; the
  SKIPPED path is by design.
- **`scripts/lib/manifest-autosource.sh`** owns the auto-source (`manifest_autosource_fetch` — resolve
  the CI run at the box's marker SHA, `gh run download` the manifest artifact; jq reads the SHA via
  `env.SHA`, and `find … -print -quit` not `find … | head -1` to dodge the #239 pipefail SIGPIPE) + the
  imag ssh gather (`imag_so_gather_cmd` / `imag_so_bytes_csv`, `sha256sum` fail-loud `TOOL_MISSING`
  #833). Everything best-effort → `""` on any failure → the arg is omitted → the facet DORMANT (never a
  refuse). `gh`/`ssh` are behind the `MANIFEST_AUTOSOURCE_CMD` #836 executable-fixture seam, so the
  whole path is offline-tested (`tests/harness_manifest_autosource_1082.rs`) — no gh/ssh/network.

## GOTCHA — a BUNDLE_MANIFEST test fixture MUST be one `files[]` entry per LINE (drift-guard's grep is line-based)

`drift-guard.sh`'s manifest parsers (`manifest_sha_for_component`, `manifest_all_paths`,
`manifest_sha_for_path`) are `grep`/`sed` LINE-based — they assume each `files[]` entry is on its own
line (`{ "path": "…", "sha256": "…", "size": N }`), exactly as `genlock-manifest.sh::generate_manifest`
emits. A SINGLE-LINE manifest fixture (e.g. Python `json.dump(obj, f)` with no indent) puts every
entry on ONE line, so `grep "…obs[.]dll…" | sed 's/.*"sha256": "\(…\)".*/\1/'`'s GREEDY `.*` grabs the
LAST entry's sha256 on that line — `manifest_sha_for_component obs` returns the distroav sha, and the
byte compare reads DRIFT/OK for the wrong reason. Confirmed live writing #770's offline gate check.
When hand-writing a manifest fixture (in a Rust test's `write_manifest`, a python verify script, or
by hand), emit ONE `files[]` entry per line — never a compact single-line JSON. `tests/version_integrity_gate.rs::write_manifest`
does this correctly (`\n    { … },\n    { … }\n`).

## Offline-proving the byte-vs-manifest gate without cargo (Tier-0)

`tests/version_integrity_gate.rs` invokes the gate as a SUBPROCESS, so its assertions can be verified
WITHOUT `cargo test` (banned locally) by driving the real bash gate directly: build strih+stream
state JSONs carrying the byte-facet keys (`obs_dll_sha256`, `distroav_dll_sha256`, `genlock_capability`
— a `manifest=` ALWAYS also activates the capability check) + the enforced parity/obs-identity keys
(`with_sha`/`with_obs_identity_ok` equivalents), a multi-line manifest, then run
`bash scripts/version-integrity-gate.sh --manifest <m> --win-state strih=<s> --win-state stream=<t>
--genlock-sha imag=<sha>` from the repo root and assert exit 20 (byte drift → REFUSE, names the
drifted component + box) / exit 0 (bytes match → GATE PASS). This is the genuine offline verification
for the Rust fixtures (which only compile+run at CI/integration).

## #1100 — imag byte facet ENFORCED; Windows half STAGED (the bundle-state-server can be STALE)

`version-integrity-gate.sh`'s imag `.so` byte facet is now ENFORCED (the #758-shape flip of #1082's
opt-in landing): `imag_bytes_verdict`'s two DORMANT `return 10` branches are UNKNOWN `return 11`, and
`main()` runs the facet UNCONDITIONALLY (the `if [ -n "$imag_bytes" ] || [ -n "$imag_manifest" ]`
guard removed) — an absent imag gather/manifest is now a gate-blocking UNKNOWN. Enforced-fixture tests
carry clean imag args via the new `clean_imag_bytes_1100(tag)` helper (the imag analogue of
`with_sha`/`with_obs_identity_ok`). Preconditions verified live before flipping: imag `.so`s present +
hashable at `/usr/lib/x86_64-linux-gnu/`, and `imag_so_bytes OK` on the last green E2E `[0/8]` log.

**The WINDOWS obs.dll/distroav.dll byte half is STILL STAGED (the #1100 follow-up):** removing the
`manifest_autosource_state_has_key … obs_dll_sha256` opt-in guard in `recording-e2e.sh` requires
strih+stream to actually serve `obs_dll_sha256`/`distroav_dll_sha256` on `:8899`. **Root cause it did
NOT (2026-08-18): the deployed `C:\ProgramData\camera-box\bundle-state-server.py` +
`bundle_state_gather.py` are deployed by a SEPARATE mechanism from the OBS genlock bundle, so a
full-bundle OBS redeploy to a fresh canonical SHA does NOT update them.** They were dated 2026-08-16
21:03 — PREDATING #770's byte-gather (`61b128162`, 22:46) — and had no `component_sha256`/
`obs_dll_sha256` code at all, so the keys were omit-when-empty absent even though obs.dll/distroav.dll
were present + hashable on-box. **Verify precondition 1 by the DEPLOYED SERVER, never the OBS bundle
SHA:** `curl http://<box>:8899/bundle-state.json` must show `obs_dll_sha256` + `distroav_dll_sha256`
(or grep the deployed `.py` for `component_sha256`). **Unblock:** redeploy the current
bundle-state-server scripts to strih+stream + restart the `BundleStateServer` scheduled task, confirm
the keys appear on BOTH, THEN remove the `recording-e2e.sh` guard — same LIVE-Windows-property class as
#1067's port4455 caveat.

## #1100 (Windows half) — ENFORCED (the staged half, unblocked 2026-08-18)

Precondition 1 was unblocked by the supervisor (~19:30) redeploying the current bundle-state-server +
restarting BundleStateServer. Re-verified LIVE, read-only, at canonical SHA `03cd9c073`: strih
(10.77.9.202) AND stream (10.77.9.204) `:8899/bundle-state.json` now BOTH serve `obs_dll_sha256`
(`200e34dc…`) + `distroav_dll_sha256` (`d6077c16…`) + `genlock_capability`; the deployed
`C:\Program Files\obs-studio\BUNDLE_MANIFEST.json` records `bin/64bit/obs.dll` = `200e34dc…`; and the
CI FAST manifest (`windows-genlock-fast.yml` / `obs-genlock-fast-dll`) at that SHA (run `32140701978`)
carries `obs.dll` = `200e34dc…`. So the enforced gate stays GREEN today AND a box that stops reporting
its bytes now refuses.

The flip: **`recording-e2e.sh`'s Windows FAST-manifest auto-source guard was collapsed from
`if [ -z "$AUTO_WIN_MANIFEST" ] && manifest_autosource_state_has_key … obs_dll_sha256 (×2) +
genlock_capability (×2); then` down to `if [ -z "$AUTO_WIN_MANIFEST" ]; then`** — the manifest is now
auto-sourced UNCONDITIONALLY (when `VERSION_GATE_MANIFEST` is unset), so it is applied to both
`--win-state` boxes and a box missing `obs_dll_sha256` flips to a gate-blocking UNKNOWN via drift-guard
(the same 756→758 second step `#1067` applied to `port4455_identity`, mirrored in recording-e2e.sh
rather than the gate main). The `manifest_autosource_state_has_key` LIB function is KEPT (still
exported + unit-tested); only its USE as the opt-in guard is removed.

- **Residual (in-scope, documented, NOT a refuse):** `manifest_autosource_fetch` stays best-effort — a
  gh/network failure OR an unresolvable strih marker SHA (`[ -n "$sha" ] || return 0`) yields `""` → the
  manifest is omitted for THAT run → the Windows byte facet is dormant for that run. That is a
  fetch-side outage, not a box-drift signal, so it never false-refuses (matches the lib's documented
  contract). The enforce covers "a box PRESENT but NOT reporting its bytes → refuse", which the guard
  removal delivers.
- **The FAST manifest is obs.dll-ONLY, so ONLY the libobs core is byte-enforced on Windows; distroav
  stays SKIPPED (by design).** Found while verifying: the deployed/OBS-loaded distroav is
  `C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll` (`d6077c16…`, served, identical
  on both boxes), while the FULL BUNDLE_MANIFEST records `obs-plugins/64bit/distroav.dll` (`796b9513…`)
  at a path that does NOT exist under `C:\Program Files\obs-studio`. Filed as #1115 — widening Windows
  distroav byte enforcement to the FULL bundle manifest needs that reconciled first (the "separate
  decision" the design deferred).
- **TEST (Tier-0-only):** new `tests/harness_windows_manifest_enforce_1100.rs` — a static-text guard on
  recording-e2e.sh (RED asserts the enforce = no `manifest_autosource_state_has_key` gate, GREEN after
  removal; a regression guard that the `manifest_autosource_fetch … windows-genlock-fast.yml
  obs-genlock-fast-dll` auto-source still runs). Its own body must NEVER contain the literal
  `manifest_autosource_state_has_key` (the assertion forbids it fleet-wide) — the #832 self-collision
  class. Editing recording-e2e.sh MANDATES the full `cargo test` suite at integration (per the
  static-anchor GOTCHA); no test anchors on this block or the removed comment text (verified by grep).

## #1164 — `--imag-acked-offline REASON`: an operator-acked absent imag must NOT UNKNOWN-refuse

After the #1100 ENFORCED flip the gate had TWO facets that fail-closed on an absent imag — the
cross-box genlock parity (empty imag SHA → `UNKNOWN` 11) AND the imag `.so` byte facet (absent
bytes → `UNKNOWN` 11). An operator-acked, physically-absent imag (rig-fleet.txt `imag:…`, the
issue-1013 mechanism; recording-e2e.sh sets `IMAG_OFFLINE_ACKED=1`) therefore refused the WHOLE
E2E even though every OTHER imag site legally skips (run 32480962068: "2 box(es) UNKNOWN:
genlock_parity imag:so_bytes"). This gate was the one imag hard-abort site the issue-1013
inventory (`imag-offline-ack.md`) had missed.

- **Flag, not ambient env (the pure-gate contract):** the gate reads NO ack env / rig-fleet.txt
  itself — the ack decision stays at the recording-e2e.sh call site, which passes
  `--imag-acked-offline "$IMAG_OFFLINE_ACK_REASON"`. This keeps the gate's verdict a pure function
  of its args (it is also run standalone by operators + the subprocess tests, where an ambient
  env-ack would silently change the verdict).
- **When set:** the imag `.so` byte facet prints a LOUD `imag_so_bytes SKIPPED (imag acked
  offline: REASON …)` line and counts as ok (never `unknown`, so `imag:so_bytes` never lands in
  the roll-up), AND any `--genlock-sha` entry labelled exactly `imag` is dropped from the parity
  args (`[ "${ge%%=*}" = "imag" ]`), so parity certifies the remaining fleet strih+stream.
- **WITHOUT the flag the gate is byte-identical to #1100** — an absent imag is still fail-closed
  `UNKNOWN` (both facets). Pinned by `gate_still_refuses_absent_imag_without_the_ack_flag_1164`
  (asserts exit 11 + `imag_so_bytes UNKNOWN` + `UNREAD: imag` — the parity-drop is OFF by default).
- **Call site (recording-e2e.sh):** guarded on `IMAG_OFFLINE_ACKED=1` — emits `imag_leg_skip_note`,
  skips the three imag ssh gathers (SHA read + `.so` probe + manifest fetch, so no ssh to
  `$IMAG_IP`), and branches the gate invocation (acked → `--imag-acked-offline`; else byte-for-byte
  the pre-#1164 call). The non-acked `else` branch and the SHA-read `else` are left un-re-indented
  (bash-legal, smallest diff — the anchor tests match substrings).
- **Tier-0 proof (no cargo):** build clean strih+stream state fixtures (`with_obs_identity_ok` +
  `with_sha`), run `bash scripts/version-integrity-gate.sh --win-state strih=… --win-state
  stream=… --genlock-sha imag= --imag-acked-offline "<reason>"` → exit 0 + `GATE PASS` +
  `imag_so_bytes SKIPPED`; drop the flag → exit 11. This is the same offline gate-subprocess path
  the byte-facet tests use.
